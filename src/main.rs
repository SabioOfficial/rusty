use std::{
  env, error::Error, sync::Arc, time::{Duration, Instant, SystemTime, UNIX_EPOCH},
};

use twilight_gateway::{Event, EventTypeFlags, Intents, Shard, ShardId, StreamExt as _};
use twilight_http::Client;
use twilight_model::{
  application::{command::CommandType, interaction::InteractionData},
  gateway::payload::incoming::InteractionCreate,
  http::interaction::{InteractionResponse, InteractionResponseType},
};
use twilight_util::builder::{command::CommandBuilder, InteractionResponseDataBuilder};

#[global_allocator]
static GLOBAL: mimalloc::MiMalloc = mimalloc::MiMalloc;

type BoxErr = Box<dyn Error + Send + Sync>;

#[tokio::main]
async fn main() -> Result<(), BoxErr> {
  let token = env::var("DISCORD_TOKEN").expect("DISCORD_TOKEN not set");
  let http = Arc::new(Client::new(token.clone()));

  let app = http.current_user_application().await?.model().await?;

  let command = CommandBuilder::new("boom", "Boom!", CommandType::ChatInput).build();
  http.interaction(app.id)
    .set_global_commands(&[command])
    .await?;
  
  let mut shard = Shard::new(ShardId::ONE, token, Intents::empty());
  println!("rusty online");

  while let Some(item) = shard.next_event(EventTypeFlags::INTERACTION_CREATE).await {
    let received = Instant::now();
    let received_wall = SystemTime::now();

    let event = match item {
      Ok(event) => event,
      Err(err) => {
        eprintln!("gateway error: {err}");
        continue;
      }
    };

    if let Event::InteractionCreate(interaction) = event {
      let gateway_latency = shard.latency().average();
      let http = Arc::clone(&http);
      tokio::spawn(async move {
        if let Err(err) = 
          handle(http, interaction, received, received_wall, gateway_latency).await
        {
          eprintln!("handler error: {err}");
        }
      });
    }
  }

  Ok(())
}

async fn handle(http: Arc<Client>, ev: Box<InteractionCreate>, received: Instant, received_wall: SystemTime, gateway_latency: Option<Duration>) -> Result<(), BoxErr> {
  let Some(InteractionData::ApplicationCommand(cmd)) = &ev.data else {
    return Ok(());
  };
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
