use std::{
  cmp::Reverse, collections::{BinaryHeap, HashMap}, env, error::Error, sync::{Arc, RwLock}, time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use tokio::sync::mpsc;
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::{Client, error::ErrorType};
use twilight_model::{
  application::{
    command::CommandType, interaction::{
      InteractionData, application_command::{CommandData, CommandDataOption, CommandOptionValue},
    },
    }, channel::{
    ChannelType, message::{AllowedMentions, MessageFlags},
    permission_overwrite::{
      PermissionOverwrite as ChannelOverwrite, PermissionOverwriteType as ChannelOverwriteType,
    },
  }, gateway::payload::incoming::{InteractionCreate, MessageCreate}, guild::Permissions, http::{
    interaction::{InteractionResponse, InteractionResponseType},
    permission_overwrite::{PermissionOverwrite, PermissionOverwriteType},
  }, id::{
    Id, marker::{ChannelMarker, GenericMarker, GuildMarker, MessageMarker, UserMarker},
  },
};
use twilight_util::builder::{
  InteractionResponseDataBuilder, command::{ChannelBuilder, CommandBuilder, IntegerBuilder, StringBuilder, SubCommandBuilder, SubCommandGroupBuilder, UserBuilder},
};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

type BoxErr = Box<dyn Error + Send + Sync>;

type Job = (u64, Id<ChannelMarker>, Id<MessageMarker>);

#[derive(Clone, Copy)]

struct PrivateVc {
  owner: Id<UserMarker>,
  expires_ms: u64,
}

#[derive(Clone, Copy)]
struct Disappear {
  secs: u64,
  since_ms: u64,
}

struct State {
  redirects: RwLock<HashMap<Id<GuildMarker>, Id<UserMarker>>>,
  disappear: RwLock<HashMap<Id<ChannelMarker>, Disappear>>,
  vcs: RwLock<HashMap<Id<ChannelMarker>, PrivateVc>>,
  tx: mpsc::UnboundedSender<Job>,
  bot_id: Id<UserMarker>,
}

fn load_redirects() -> HashMap<Id<GuildMarker>, Id<UserMarker>> {
  let mut map = HashMap::new();
  if let Ok(text)= std::fs::read_to_string("redirects.txt") {
    for line in text.lines() {
      let mut parts = line.split_whitespace();
      if let (Some(g), Some(u))= (parts.next(), parts.next()) {
        if let (Ok(g), Ok(u)) = (g.parse::<u64>(), u.parse::<u64>()) {
          if let (Some(g), Some(u)) = (Id::new_checked(g), Id::new_checked(u)){
            map.insert(g, u);
          }
        }
      }
    }
  }
  map
}

fn save_redirects(map: &HashMap<Id<GuildMarker>, Id<UserMarker>>) {
  let text: String = map
    .iter()
    .map(|(g, u)| format!("{} {}\n", g.get(), u.get()))
    .collect();
  if let Err(err)= std::fs::write("redirects.txt", text) {
    eprintln!("failed to save redirects.txt: {err}");
  }
}

fn now_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as u64
}

fn snowflake_ms(id: u64) -> u64 {
  (id >> 22) + 1420070400000
}

fn unit_secs(unit: &str) -> Option<u64> {
  match unit {
    "seconds" => Some(1),
    "minutes" => Some(60),
    "hours" => Some(3600),
    "days" => Some(86400),
    "weeks" => Some(604800),
    _ => None,
  }
}

fn human(secs: u64) -> String {
  for (n, name) in [
    (604800, "week"),
    (86400, "day"),
    (3600, "hour"),
    (60, "minute"),
    (1, "second"),
  ] {
    if secs % n == 0 {
      let c = secs / n;
      return format!("{c} {name}{}", if c == 1 { "" } else { "s" });
    }
  }
  format!("{secs} seconds")
}

fn load_disappear() -> HashMap<Id<ChannelMarker>, Disappear> {
  let mut map = HashMap::new();
  if let Ok(text) = std::fs::read_to_string("disappear.txt") {
    for line in text.lines() {
      let mut p = line.split_whitespace();
      if let (Some(c), Some(s), Some(t)) = (p.next(), p.next(), p.next()) {
        if let (Ok(c), Ok(secs), Ok(since_ms)) = (c.parse::<u64>(), s.parse::<u64>(), t.parse::<u64>()) {
          if let Some(c) = Id::new_checked(c) {
            map.insert(c, Disappear { secs, since_ms });
          }
        }
      }
    }
  }
  map
}

fn save_disappear(map: &HashMap<Id<ChannelMarker>, Disappear>) {
  let text: String = map
    .iter()
    .map(|(c, d)| format!("{} {} {}\n", c.get(), d.secs, d.since_ms))
    .collect();
  if let Err(err) = std::fs::write("disappear.txt", text) {
    eprintln!("failed to save disappear.txt: {err}");
  }
}

async fn scheduler(http: Arc<Client>, mut rx: mpsc::UnboundedReceiver<Job>) {
  let mut heap: BinaryHeap<Reverse<Job>> = BinaryHeap::new();
  loop {
    let wait = match heap.peek() {
      Some(Reverse((at, _, _))) => Duration::from_millis(at.saturating_sub(now_ms())),
      None => Duration::from_secs(3600),
    };
    tokio::select! {
      job = rx.recv() => match job {
        Some(job) => heap.push(Reverse(job)),
        None => return,
      },
      _ = tokio::time::sleep(wait) => {
        let now = now_ms();
        let mut due = Vec::new();
        while let Some(Reverse((at, _, _))) = heap.peek() {
          if *at > now {
            break;
          }
          let Reverse((_, c, m)) = heap.pop().unwrap();
          due.push((c, m));
        }
        if !due.is_empty() {
          tokio::spawn(delete_all(Arc::clone(&http), due));
        }
      }
    }
  }
}

async fn delete_all(http: Arc<Client>, due: Vec<(Id<ChannelMarker>, Id<MessageMarker>)>) {
  let cutoff = now_ms().saturating_sub(1123200000);
  let mut by_channel: HashMap<Id<ChannelMarker>, Vec<Id<MessageMarker>>> = HashMap::new();
  for (c, m) in due {
    by_channel.entry(c).or_default().push(m);
  }
  for (channel, ids) in by_channel {
    let (fresh, old): (Vec<_>, Vec<_>) = ids
      .into_iter()
      .partition(|m| snowflake_ms(m.get()) > cutoff);
    for chunk in fresh.chunks(100) {
      if chunk.len() > 1 && http.delete_messages(channel, chunk).await.is_ok() {
        continue;
      }
      delete_each(&http, channel, chunk).await;
    }
    delete_each(&http, channel, &old).await;
  }
}

async fn delete_each(http: &Client, channel: Id<ChannelMarker>, ids: &[Id<MessageMarker>]) {
  for &message in ids {
    if let Err(err) = http.delete_message(channel, message).await {
      let gone = matches!(err.kind(), ErrorType::Response { status, .. } if status.get() == 404);
      if !gone {
        eprintln!("delete failed: {err}");
      }
    }
  }
}

async fn catch_up(http: Arc<Client>, tx: mpsc::UnboundedSender<Job>, channel: Id<ChannelMarker>, cfg: Disappear) {
  let mut before: Option<Id<MessageMarker>> = None;
  loop {
    let resp = match before {
      Some(b) => http.channel_messages(channel).before(b).limit(100).await,
      None => http.channel_messages(channel).limit(100).await,
    };
    let msgs = match resp {
      Ok(resp) => match resp.models().await {
        Ok(m) => m,
        Err(err) => {
          eprintln!("catch-up decode error in {channel}: {err}");
          return;
        }
      },
      Err(err) => {
        eprintln!("catch-up fetch error in {channel}: {err}");
        return;
      }
    };
    if msgs.is_empty() {
      return;
    }
    before = msgs.last().map(|m| m.id);

    let mut reached_setup = false;
    for m in &msgs {
      let created = snowflake_ms(m.id.get());
      if created < cfg.since_ms {
        reached_setup = true;
        continue;
      }
      if m.pinned {
        continue;
      }
      let _ = tx.send((created + cfg.secs * 1000, channel, m.id));
    }
    if reached_setup || msgs.len() < 100 {
      return;
    }
  }
}

#[tokio::main]
async fn main() -> Result<(), BoxErr> {
  rustls::crypto::ring::default_provider()
    .install_default()
    .expect("failed to install rustls crypto provider");

  dotenvy::dotenv().ok();

  let token = env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN not set");
  let http = Arc::new(Client::new(token.clone()));

  let app = http.current_user_application().await?.model().await?;

  let boom = CommandBuilder::new("boom", "Boom!", CommandType::ChatInput).build();
  let redirect = CommandBuilder::new(
    "person-redirect",
    "Redirect pings of this bot to a person",
    CommandType::ChatInput,
  )
  .default_member_permissions(Permissions::MANAGE_GUILD)
  .option(
    SubCommandBuilder::new("setup", "Ping this person whenever this bot is pinged")
      .option(UserBuilder::new("user", "Who should be pinged").required(true)), 
  )
  .build();
  let disappearing = CommandBuilder::new(
    "disappearing",
    "Auto-delete messages in this channel",
    CommandType::ChatInput,
  )
  .default_member_permissions(Permissions::MANAGE_MESSAGES)
  .option(
    SubCommandBuilder::new("setup", "Delete new messages in this channel after a delay")
      .option(IntegerBuilder::new("amount", "How many units").required(true).min_value(1))
      .option(
        StringBuilder::new("unit", "Unit of time")
          .required(true)
          .choices([
            ("Seconds", "seconds"),
            ("Minutes", "minutes"),
            ("Hours", "hours"),
            ("Days", "days"),
            ("Weeks", "weeks"),
          ]),
      ),
  )
  .option(SubCommandBuilder::new("off", "Stop deleting messages in this channel"))
  .build();
  let vc_opt = || {
    ChannelBuilder::new("vc", "The private voice channel")
      .channel_types([ChannelType::GuildVoice])
      .required(true)
  };
  let private_vc = CommandBuilder::new(
    "create-private-vc",
    "Create and manage temporary private voice channels",
    CommandType::ChatInput,
  )
  .option(
    SubCommandBuilder::new("setup", "Create a private voice channel")
      .option(StringBuilder::new("name", "Channel name").required(true).max_length(100))
      .option(IntegerBuilder::new("amount", "How many units until it auto-deletes").required(true).min_value(1))
      .option(
        StringBuilder::new("unit", "Unit of time")
          .required(true)
          .choices([("Minutes", "minutes"), ("Hours", "hours"), ("Days", "days")]), 
      ),
  )
  .option(
    SubCommandGroupBuilder::new("modify", "Modify one of your private voice channels").subcommands([
      SubCommandBuilder::new("users-add", "Invite a user (they get a DM)")
        .option(vc_opt())
        .option(UserBuilder::new("user", "Who to invite").required(true)),
      SubCommandBuilder::new("users-transfer", "Transfer ownership to another user")
        .option(vc_opt())
        .option(UserBuilder::new("user", "New owner").required(true)),
      SubCommandBuilder::new("close", "Delete the voice channel now").option(vc_opt()),
    ]),
  )
  .build();
  http.interaction(app.id)
    .set_global_commands(&[boom, redirect, disappearing, private_vc])
    .await?;

  let me = http.current_user().await?.model().await?;
  let (tx, rx) = mpsc::unbounded_channel();
  tokio::spawn(scheduler(Arc::clone(&http), rx));
  let state = Arc::new(State {
    redirects: RwLock::new(load_redirects()),
    disappear: RwLock::new(load_disappear()),
    vcs: RwLock::new(load_vcs()),
    tx,
    bot_id: me.id,
  });
  
  let mut shard = Shard::new(ShardId::ONE, token, Intents::GUILD_MESSAGES);
  println!("rusty online");

  while let Some(item) = shard
    .next_event(EventTypeFlags::INTERACTION_CREATE | EventTypeFlags::MESSAGE_CREATE | EventTypeFlags::READY)
    .await
  {
    let received = Instant::now();
    let received_wall = SystemTime::now();

    let event = match item {
      Ok(event) => event,
      Err(err) => {
        eprintln!("gateway error: {err}");
        continue;
      }
    };

    match event {
      Event::InteractionCreate(interaction) => {
        let gateway_latency = shard.latency().average();
        let http = Arc::clone(&http);
        let state = Arc::clone(&state);
        tokio::spawn(async move {
          if let Err(err) = handle(http, state, interaction, received, received_wall, gateway_latency).await {
            eprintln!("handler error: {err}");
          }
        });
      }
      Event::Ready(_) => {
        let channels: Vec<_> = state
          .disappear
          .read()
          .unwrap()
          .iter()
          .map(|(c, d)| (*c, *d))
          .collect();
        for (channel, cfg) in channels {
          tokio::spawn(catch_up(Arc::clone(&http), state.tx.clone(), channel, cfg));
        }
        let vcs: Vec<_> = state
          .vcs
          .read()
          .unwrap()
          .iter()
          .map(|(c, v)| (*c, v.expires_ms))
          .collect();
        for (channel, expires_ms) in vcs {
          spawn_vc_timer(Arc::clone(&http), Arc::clone(&state), channel, expires_ms);
        }
      }
      Event::MessageCreate(msg) => {
        let secs = state
          .disappear
          .read()
          .unwrap()
          .get(&msg.channel_id)
          .map(|d| d.secs);
        if let Some(secs) = secs {
          let at = snowflake_ms(msg.id.get()) + secs * 1000;
          let _ = state.tx.send((at, msg.channel_id, msg.id));
        }
        if !msg.author.bot && msg.guild_id.is_some() && msg.mentions.iter().any(|m| m.id == state.bot_id) {
          let http = Arc::clone(&http);
          let state = Arc::clone(&state);
          tokio::spawn(async move {
            if let Err(err) = handle_message(http, state, msg).await {
              eprintln!("message handler error: {err}");
            }
          });
        }
      }
      _ => {}
    }
  }

  Ok(())
}

async fn handle(http: Arc<Client>, state: Arc<State>, ev: Box<InteractionCreate>, received: Instant, received_wall: SystemTime, gateway_latency: Option<Duration>) -> Result<(), BoxErr> {
  let Some(InteractionData::ApplicationCommand(cmd)) = &ev.data else {
    return Ok(());
  };
  if cmd.name == "person-redirect" {
    return handle_person_redirect(&http, &state, &ev, cmd).await;
  }
  if cmd.name == "disappearing" {
    return handle_disappearing(&http, &state, &ev, cmd).await;
  }
  if cmd.name == "create-private-vc" {
    return handle_private_vc(&http, &state, &ev, cmd).await;
  }
  if cmd.name != "boom" {
    return Ok(());
  }

  let created_ms = (ev.id.get() >> 22) + 1420070400000;
  let now_ms = received_wall
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as u64;
  let delivery_ms = now_ms.saturating_sub(created_ms);

  let gw = gateway_latency
    .map(|d| format!("{:.1} ms", d.as_secs_f64() * 1000.0))
    .unwrap_or_else(|| "n/a".into());

  let iclient = http.interaction(ev.application_id);

  let process_us = received.elapsed().as_micros();
  let first = format!(
    "Boom!\n\
    -# Command registered by Discord to bot: {delivery_ms} ms\n\
    -# Bot processing before reply: {process_us} μs\n\
    -# Gateway heartbeat latency: {gw}\n\
    -# REST reply round-trip: measuring..."
  );

  let response = InteractionResponse {
    kind: InteractionResponseType::ChannelMessageWithSource,
    data: Some(InteractionResponseDataBuilder::new().content(first).build()),
  };

  let sent_at = Instant::now();
  iclient
    .create_response(ev.id, &ev.token, &response)
    .await?;
  let rest = sent_at.elapsed();

  let total_ms= delivery_ms as f64 + received.elapsed().as_secs_f64() * 1000.0;
  let final_text = format!(
    "Boom!\n\
    -# Command registered by Discord to bot: {delivery_ms} ms\n\
    -# Bot processing before reply: {process_us} μs\n\
    -# Gateway heartbeat latency: {gw}\n\
    -# REST reply round-trip: {:.1}ms\n\
    -# Total, command to reply confirmed: ~{total_ms:.0} ms",
    rest.as_secs_f64() * 1000.0
  );

  iclient
    .update_response(&ev.token)
    .content(Some(&final_text))
    .await?;

  Ok(())
}

async fn handle_person_redirect(http: &Client, state: &State, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
  let target = cmd.options.iter().find_map(|opt| match &opt.value {
    CommandOptionValue::SubCommand(sub) if opt.name == "setup" => {
      sub.iter().find_map(|o| match &o.value {
        CommandOptionValue::User(id) if o.name == "user" => Some(*id),
        _ => None,
      })
    }
    _ => None,
  });

  let text = match (ev.guild_id, target) {
    (Some(guild_id), Some(target)) => {
      {
        let mut map = state.redirects.write().unwrap();
        map.insert(guild_id, target);
        save_redirects(&map);
      }
      format!("Done. Pinging me will now ping <@{target}>.")
    }
    _ => "Use `/person-redirect setup` in a server and pick a user.".to_string(),
  };

  let response = InteractionResponse {
    kind: InteractionResponseType::ChannelMessageWithSource,
    data: Some(
      InteractionResponseDataBuilder::new()
        .content(text)
        .flags(MessageFlags::EPHEMERAL)
        .allowed_mentions(AllowedMentions::default())
        .build(),
    ),
  };
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  Ok(())
}

async fn handle_message(http: Arc<Client>, state: Arc<State>, msg: Box<MessageCreate>) -> Result<(), BoxErr> {
  if msg.author.bot {
    return Ok(());
  }
  let Some(guild_id) = msg.guild_id else {
    return Ok(());
  };
  if !msg.mentions.iter().any(|m| m.id == state.bot_id) {
    return Ok(());
  }
  let Some(target) = state.redirects.read().unwrap().get(&guild_id).copied() else {
    return Ok(());
  };

  let text = format!(
    "<@{target}>\n\
    -# Pinged with Redirect"
  );
  let allowed = AllowedMentions {
    users: vec![target],
    ..Default::default()
  };
  http.create_message(msg.channel_id)
    .reply(msg.id)
    .allowed_mentions(Some(&allowed))
    .content(&text)
    .await?;
  Ok(())
}

async fn handle_disappearing(http: &Arc<Client>, state: &State, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
  let channel = ev.channel.as_ref().map(|c| c.id);
  let sub = cmd.options.first();

  let text = match (sub, channel) {
    (Some(sub), Some(channel)) if sub.name == "setup" => {
      let (mut amount, mut unit) = (None, None);
      if let CommandOptionValue::SubCommand(opts) = &sub.value {
        for o in opts {
          match &o.value {
            CommandOptionValue::Integer(n) if o.name == "amount" => amount = Some(*n),
            CommandOptionValue::String(s) if o.name == "unit" => unit = Some(s.as_str()),
            _ => {}
          }
        }
      }
      match (amount, unit.and_then(unit_secs)) {
        (Some(n), Some(mult)) if n >= 1 => match (n as u64).checked_mul(mult) {
          Some(secs) if secs <= 31536000 => {
            let cfg = Disappear { secs, since_ms: now_ms() };
            {
              let mut map = state.disappear.write().unwrap();
              map.insert(channel, cfg);
              save_disappear(&map);
            }
            tokio::spawn(catch_up(Arc::clone(http), state.tx.clone(), channel, cfg));
            format!("Messages sent in <#{channel}> from now on will be deleted after {}.", human(secs))
          }
          _ => "That's too long. The maximum is 1 year.".to_string(),
        },
        _ => "Pick an amount of 1 or more and a unit.".to_string(),
      }
    }
    (Some(sub), Some(channel)) if sub.name == "off" => {
      {
        let mut map = state.disappear.write().unwrap();
        map.remove(&channel);
        save_disappear(&map);
      }
      format!("Disappearing messages are off in <#{channel}>. Already-queued messages will still be deleted.")
    }
    _ => "Use `/disappearing` in a server channel.".to_string(),
  };

  let response = InteractionResponse {
    kind: InteractionResponseType::ChannelMessageWithSource,
    data: Some(
      InteractionResponseDataBuilder::new()
        .content(text)
        .flags(MessageFlags::EPHEMERAL)
        .allowed_mentions(AllowedMentions::default())
        .build(),
    ),
  };
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  Ok(())
}

fn opt_str<'a>(opts: &'a [CommandDataOption], name: &str) -> Option<&'a str> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::String(s) if o.name == name => Some(s.as_str()),
    _ => None,
  })
}

fn opt_int(opts: &[CommandDataOption], name: &str) -> Option<i64> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::Integer(n) if o.name == name => Some(*n),
    _ => None,
  })
}

fn opt_user(opts: &[CommandDataOption], name: &str) -> Option<Id<UserMarker>> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::User(id) if o.name == name => Some(*id),
    _ => None,
  })
}

fn opt_channel(opts: &[CommandDataOption], name: &str) -> Option<Id<ChannelMarker>> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::Channel(id) if o.name == name => Some(*id),
    _ => None,
  })
}

fn load_vcs() -> HashMap<Id<ChannelMarker>, PrivateVc> {
  let mut map = HashMap::new();
  if let Ok(text) = std::fs::read_to_string("private_vcs.txt") {
    for line in text.lines() {
      let mut p = line.split_whitespace();
      if let (Some(c), Some(o), Some(t)) = (p.next(), p.next(), p.next()) {
        if let (Ok(c), Ok(o), Ok(expires_ms)) = (c.parse::<u64>(), o.parse::<u64>(), t.parse::<u64>()) {
          if let (Some(c), Some(owner)) = (Id::new_checked(c), Id::new_checked(o)) {
            map.insert(c, PrivateVc { owner, expires_ms });
          }
        }
      }
    }
  }
  map
}

fn save_vcs(map: &HashMap<Id<ChannelMarker>, PrivateVc>) {
  let text: String = map
    .iter()
    .map(|(c, v)| format!("{} {} {}\n", c.get(), v.owner.get(), v.expires_ms))
    .collect();
  if let Err(err) = std::fs::write("private_vcs.txt", text) {
    eprintln!("failed to save private_vcs.txt: {err}");
  }
}

fn spawn_vc_timer(http: Arc<Client>, state: Arc<State>, channel: Id<ChannelMarker>, expires_ms: u64) {
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(expires_ms.saturating_sub(now_ms()))).await;
    let still_tracked = state.vcs.read().unwrap().contains_key(&channel);
    if still_tracked {
      delete_vc(&http, &state, channel).await;
    }
  });
}

async fn delete_vc(http: &Client, state: &State, channel: Id<ChannelMarker>) {
  let removable = match http.delete_channel(channel).await {
    Ok(_) => true,
    Err(err) => {
      let gone = matches!(err.kind(), ErrorType::Response { status, .. } if status.get() == 404);
      if !gone {
        eprintln!("failed to delete private vc {channel}: {err}");
      }
      gone
    }
  };
  if removable {
    let mut map = state.vcs.write().unwrap();
    map.remove(&channel);
    save_vcs(&map);
  }
}

async fn allow_member(http: &Client, channel: Id<ChannelMarker>, user: Id<UserMarker>) -> Result<(), BoxErr> {
  http.update_channel_permission(
    channel,
    &PermissionOverwrite {
      allow: Some(Permissions::VIEW_CHANNEL.union(Permissions::CONNECT).union(Permissions::SPEAK)),
      deny: None,
      id: user.cast(),
      kind: PermissionOverwriteType::Member,
    },
  )
  .await?;
  Ok(())
}

async fn send_dm(http: &Client, user: Id<UserMarker>, text: &str) -> Result<(), BoxErr> {
  let dm = http.create_private_channel(user).await?.model().await?;
  http.create_message(dm.id).content(text).await?;
  Ok(())
}

async fn handle_private_vc(http: &Arc<Client>, state: &Arc<State>, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
  let iclient = http.interaction(ev.application_id);
  let ack = InteractionResponse {
    kind: InteractionResponseType::DeferredChannelMessageWithSource,
    data: Some(InteractionResponseDataBuilder::new().flags(MessageFlags::EPHEMERAL).build()),
  };
  iclient.create_response(ev.id, &ev.token, &ack).await?;

  let text = match run_private_vc(http, state, ev, cmd).await {
    Ok(t) => t,
    Err(err) => {
      eprintln!("private vc error: {err}");
      "Something went wrong. Make sure I have Manage Channels and Manage Roles permissions.".to_string()
    }
  };
  iclient.update_response(&ev.token).content(Some(&text)).await?;
  Ok(())
}

async fn run_private_vc(http: &Arc<Client>, state: &Arc<State>, ev: &InteractionCreate, cmd: &CommandData) -> Result<String, BoxErr> {
  let (Some(guild_id), Some(author)) = (ev.guild_id, ev.author_id()) else {
    return Ok("Use `/create-private-vc` in a server.".into());
  };
  let Some(top) = cmd.options.first() else {
    return Ok("Missing subcommand.".into());
  };

  match &top.value {
    CommandOptionValue::SubCommand(opts) if top.name == "setup" => {
      let name = opt_str(opts, "name").unwrap_or("private-vc").trim();
      let len = name.chars().count();
      if len == 0 || len > 100 {
        return Ok("The name must be 1-100 characters.".into());
      }
      let secs = match (opt_int(opts, "amount"), opt_str(opts, "unit").and_then(unit_secs)) {
        (Some(n), Some(mult)) if n >= 1 => (n as u64).checked_mul(mult),
        _ => None,
      };
      let Some(secs) = secs else {
        return Ok("Pick an amount of 1 or more and a unit".into());
      };
      if secs > 604800 {
        return Ok("That's too long. The maximum is 7 days.".into());
      }
      let owned = state.vcs.read().unwrap().values().filter(|v| v.owner == author).count();
      if owned >= 3 {
        return Ok(format!("You already own 3 private VCs. Close one first."));
      }

      let everyone: Id<GenericMarker> = guild_id.cast();
      let overwrites = [
        ChannelOverwrite {
          allow: Permissions::empty(),
          deny: Permissions::VIEW_CHANNEL | Permissions::CONNECT,
          id: everyone,
          kind: ChannelOverwriteType::Role,
        },
        ChannelOverwrite {
          allow: Permissions::VIEW_CHANNEL.union(Permissions::CONNECT).union(Permissions::SPEAK),
          deny: Permissions::empty(),
          id: author.cast(),
          kind: ChannelOverwriteType::Member,
        },
        ChannelOverwrite {
          allow: Permissions::VIEW_CHANNEL | Permissions::MANAGE_CHANNELS,
          deny: Permissions::empty(),
          id: state.bot_id.cast(),
          kind: ChannelOverwriteType::Member,
        },
      ];
      let channel = http
        .create_guild_channel(guild_id, name)
        .kind(ChannelType::GuildVoice)
        .permission_overwrites(&overwrites)
        .await?
        .model()
        .await?;

      let expires_ms = now_ms() + secs * 1000;
      {
        let mut map = state.vcs.write().unwrap();
        map.insert(channel.id, PrivateVc { owner: author, expires_ms });
        save_vcs(&map);
      }
      spawn_vc_timer(Arc::clone(http), Arc::clone(state), channel.id, expires_ms);
      Ok(format!(
        "Created <#{}>. Only you can join. Invite people with `/create-private-vc modify users-add`. It auto-deletes <t:{}:R>.",
        channel.id,
        expires_ms / 1000
      ))
    }
    CommandOptionValue::SubCommandGroup(subs) if top.name == "modify" => {
      let Some(sub) = subs.first() else {
        return Ok("Missing subcommand.".into());
      };
      let CommandOptionValue::SubCommand(opts) = &sub.value else {
        return Ok("Missing subcommand.".into());
      };
      let Some(channel) = opt_channel(opts, "vc") else {
        return Ok("Pick a video channel.".into());
      };
      let vc = state.vcs.read().unwrap().get(&channel).copied();
      let Some(vc) = vc else {
        return Ok("That isn't a private VC I manage.".into());
      };
      if vc.owner != author {
        return Ok("Only the owner of that VC can modify it.".into());
      }

      match sub.name.as_str() {
        "users-add" => {
          let Some(target) = opt_user(opts, "user") else {
            return Ok("Pick a user.".into());
          };
          if target == author || target == state.bot_id {
            return Ok("You can't pick this user.".into());
          }
          allow_member(http, channel, target).await?;
          let dm = format!("You've been invited by <@{author}> to join the private voice channel <#{channel}>.");
          if send_dm(http, target, &dm).await.is_ok() {
            Ok(format!("Added <@{target}> to <#{channel}> and sent them a DM."))
          } else {
            Ok(format!("Added <@{target}> to <#{channel}>, but I couldn't DM them (their DMs may be closed)."))
          }
        }
        "users-transfer" => {
          let Some(target) = opt_user(opts, "user") else {
            return Ok("Pick a user.".into());
          };
          if target == author || target == state.bot_id {
            return Ok("Pick someone else.".into());
          }
          allow_member(http, channel, target).await?;
          {
            let mut map = state.vcs.write().unwrap();
            if let Some(v) = map.get_mut(&channel) {
              v.owner = target;
            }
            save_vcs(&map);
          }
          Ok(format!("<@{target}> now owns <#{channel}>. You keep access but can no longer modify it."))
        }
        "close" => {
          delete_vc(http, state, channel).await;
          Ok("Closed the private VC.".into())
        }
        _ => Ok("Unknown subcommand.".into()),
      }
    }
    _ => Ok("Unknown subcommand.".into()),
  }
}
