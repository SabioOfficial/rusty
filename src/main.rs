use std::{
  collections::HashMap,
  env,
  error::Error,
  sync::{Arc, RwLock},
  time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client;
use twilight_model::{
  application::{
    command::CommandType,
    interaction::{
      application_command::{CommandData, CommandOptionValue},
      InteractionData,
    },
  },
  channel::message::{AllowedMentions, MessageFlags},
  gateway::payload::incoming::{InteractionCreate, MessageCreate},
  guild::Permissions,
  http::interaction::{InteractionResponse, InteractionResponseType},
  id::{
    marker::{GuildMarker, UserMarker},
    Id,
  },
};
use twilight_util::builder::{
  command::{CommandBuilder, SubCommandBuilder, UserBuilder},
  InteractionResponseDataBuilder,
};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

type BoxErr = Box<dyn Error + Send + Sync>;

struct State {
  redirects: RwLock<HashMap<Id<GuildMarker>, Id<UserMarker>>>,
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

#[tokio::main]
async fn main() -> Result<(), BoxErr> {
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
  http.interaction(app.id)
    .set_global_commands(&[boom, redirect])
    .await?;

  let me = http.current_user().await?.model().await?;
  let state = Arc::new(State {
    redirects: RwLock::new(load_redirects()),
    bot_id: me.id,
  });
  
  let mut shard = Shard::new(ShardId::ONE, token, Intents::GUILD_MESSAGES);
  println!("rusty online");

  while let Some(item) = shard
    .next_event(EventTypeFlags::INTERACTION_CREATE | EventTypeFlags::MESSAGE_CREATE)
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
      Event::MessageCreate(msg) => {
        let http = Arc::clone(&http);
        let state = Arc::clone(&state);
        tokio::spawn(async move {
          if let Err(err) = handle_message(http, state, msg).await {
            eprintln!("message handler error: {err}");
          }
        });
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
