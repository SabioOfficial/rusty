mod commands;
mod persist;
mod scheduler;
mod state;
mod util;

use std::{
  env,
  sync::{Arc, RwLock},
  time::{Duration, Instant, SystemTime},
};

use tokio::sync::mpsc;
use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client;
use twilight_model::{
  application::interaction::InteractionData,
  channel::message::MessageType,
  gateway::payload::incoming::InteractionCreate,
};

use crate::{
  commands::{
    anchor::{bump_anchor, handle_anchor, handle_anchor_modal, handle_anchor_toggle},
    boom::handle_boom,
    disappearing::handle_disappearing,
    log_channel::handle_log_channel,
    private_vc::{handle_private_vc, spawn_vc_timer},
    redirect::{handle_message, handle_person_redirect},
    register::definitions,
    usage::handle_usage,
  },
  persist::{load_anchors, load_disappear, load_logs, load_redirects, load_vcs},
  scheduler::{catch_up, scheduler},
  state::{BoxErr, State},
  util::snowflake_ms,
};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

#[tokio::main]
async fn main() -> Result<(), BoxErr> {
  rustls::crypto::ring::default_provider()
    .install_default()
    .expect("failed to install rustls crypto provider");

  dotenvy::dotenv().ok();

  let token = env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN not set");
  let http = Arc::new(Client::new(token.clone()));

  let app = http.current_user_application().await?.model().await?;
  
  http.interaction(app.id)
    .set_global_commands(&definitions())
    .await?;

  let me = http.current_user().await?.model().await?;
  let (tx, rx) = mpsc::unbounded_channel();
  tokio::spawn(scheduler(Arc::clone(&http), rx));
  let state = Arc::new(State {
    redirects: RwLock::new(load_redirects()),
    disappear: RwLock::new(load_disappear()),
    vcs: RwLock::new(load_vcs()),
    anchors: RwLock::new(load_anchors()),
    logs: RwLock::new(load_logs()),
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
        if msg.kind == MessageType::ChannelMessagePinned && msg.author.id == state.bot_id {
          let (http, channel, id) = (Arc::clone(&http), msg.channel_id, msg.id);
          tokio::spawn(async move {
            let _ = http.delete_message(channel, id).await;
          });
        }
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
        if msg.author.id != state.bot_id && state.anchors.read().unwrap().contains_key(&msg.channel_id) {
          bump_anchor(Arc::clone(&http), Arc::clone(&state), msg.channel_id);
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
  if let Some(InteractionData::ModalSubmit(modal)) = &ev.data {
    if modal.custom_id == "anchor-setup" {
      return handle_anchor_modal(&http, &state, &ev, modal).await;
    }
    return Ok(());
  }
  if let Some(InteractionData::MessageComponent(comp)) = &ev.data {
    if comp.custom_id == "anchor-unintrusive" {
      return handle_anchor_toggle(&http, &state, &ev).await;
    }
    return Ok(());
  }
  let Some(InteractionData::ApplicationCommand(cmd)) = &ev.data else {
    return Ok(());
  };

  match cmd.name.as_str() {
    "person-redirect" => handle_person_redirect(&http, &state, &ev, cmd).await,
    "disappearing" => handle_disappearing(&http, &state, &ev, cmd).await,
    "create-private-vc" => handle_private_vc(&http, &state, &ev, cmd).await,
    "anchor" => handle_anchor(&http, &state, &ev, cmd).await,
    "log-channel" => handle_log_channel(&http, &state, &ev, cmd).await,
    "usage" => handle_usage(&http, &state, &ev).await,
    "boom" => handle_boom(&http, &ev, received, received_wall, gateway_latency).await,
    _ => Ok(()),
  }
}
