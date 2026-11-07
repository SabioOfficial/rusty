use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use twilight_http::Client;
use twilight_model::{
  gateway::payload::incoming::InteractionCreate,
  http::interaction::{InteractionResponse, InteractionResponseType},
};
use twilight_util::builder::InteractionResponseDataBuilder;

use crate::state::BoxErr;

pub async fn handle_boom(http: &Client, ev: &InteractionCreate, received: Instant, received_wall: SystemTime, gateway_latency: Option<Duration>) -> Result<(), BoxErr> {
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