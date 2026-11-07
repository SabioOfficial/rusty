use std::time::{SystemTime, UNIX_EPOCH};

use twilight_http::{Client, error::ErrorType};
use twilight_model::{
  application::interaction::application_command::{CommandDataOption, CommandOptionValue},
  channel::message::{AllowedMentions, MessageFlags},
  gateway::payload::incoming::InteractionCreate,
  http::interaction::{InteractionResponse, InteractionResponseType},
  id::{
    Id,
    marker::{ChannelMarker, GuildMarker, UserMarker},
  },
};
use twilight_util::builder::InteractionResponseDataBuilder;

use crate::state::{BoxErr, State};

pub fn now_ms() -> u64 {
  SystemTime::now()
    .duration_since(UNIX_EPOCH)
    .unwrap_or_default()
    .as_millis() as u64
}

pub fn snowflake_ms(id: u64) -> u64 {
  (id >> 22) + 1420070400000
}

pub fn unit_secs(unit: &str) -> Option<u64> {
  match unit {
    "seconds" => Some(1),
    "minutes" => Some(60),
    "hours" => Some(3600),
    "days" => Some(86400),
    "weeks" => Some(604800),
    _ => None,
  }
}

pub fn human(secs: u64) -> String {
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

pub fn is_gone(err: &twilight_http::Error) -> bool {
  matches!(err.kind(), ErrorType::Response { status, .. } if status.get() == 404)
}

pub fn who(ev: &InteractionCreate) -> String {
  ev.author_id().map(|u| format!("<@{u}>")).unwrap_or_else(|| "Someone".into())
}

pub async fn reply_ephemeral(http: &Client, ev: &InteractionCreate, text: String) -> Result<(), BoxErr> {
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

pub async fn log_event(http: &Client, state: &State, guild: Option<Id<GuildMarker>>, text: String) {
  let Some(guild) = guild else { return };
  let Some(channel) = state.logs.read().unwrap().get(&guild).copied() else {
    return;
  };
  let no_pings = AllowedMentions::default();
  if let Err(err) = http
    .create_message(channel)
    .content(&text)
    .flags(MessageFlags::SUPPRESS_NOTIFICATIONS)
    .allowed_mentions(Some(&no_pings))
    .await
  {
    eprintln!("log post failed in {channel}: {err}");
  }
}

pub fn opt_str<'a>(opts: &'a [CommandDataOption], name: &str) -> Option<&'a str> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::String(s) if o.name == name => Some(s.as_str()),
    _ => None,
  })
}

pub fn opt_int(opts: &[CommandDataOption], name: &str) -> Option<i64> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::Integer(n) if o.name == name => Some(*n),
    _ => None,
  })
}

pub fn opt_user(opts: &[CommandDataOption], name: &str) -> Option<Id<UserMarker>> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::User(id) if o.name == name => Some(*id),
    _ => None,
  })
}

pub fn opt_channel(opts: &[CommandDataOption], name: &str) -> Option<Id<ChannelMarker>> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::Channel(id) if o.name == name => Some(*id),
    _ => None,
  })
}