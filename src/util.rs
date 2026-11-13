use std::{collections::HashMap, time::{SystemTime, UNIX_EPOCH}};

use twilight_http::{Client, error::ErrorType};
use twilight_model::{
  application::interaction::application_command::{CommandDataOption, CommandOptionValue},
  channel::message::{AllowedMentions, MessageFlags},
  gateway::payload::incoming::InteractionCreate,
  http::interaction::{InteractionResponse, InteractionResponseData, InteractionResponseType},
  id::{
    Id,
    marker::{ChannelMarker, GuildMarker, UserMarker},
  },
};
use twilight_util::builder::interaction_response::ChannelMessageBuilder;

use crate::{persist::unescape, state::{BoxErr, Reminder, State}};

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
  let response = ChannelMessageBuilder::new()
    .content(text)
    .flags(MessageFlags::EPHEMERAL)
    .allowed_mentions(AllowedMentions::default())
    .build();
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  Ok(())
}

pub fn deferred_ephemeral() -> InteractionResponse {
  InteractionResponse {
    kind: InteractionResponseType::DeferredChannelMessageWithSource,
    data: Some(InteractionResponseData {
      flags: Some(MessageFlags::EPHEMERAL),
      ..Default::default()
    }),
  }
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

pub fn load_reminders() -> Vec<Reminder> {
  let mut list = Vec::new();
  if let Ok(text) = std::fs::read_to_string("reminders.txt") {
    for line in text.lines() {
      let mut p = line.splitn(6, ' ');
      if let (Some(f), Some(c), Some(u), Some(ch), Some(ping), Some(body)) = (p.next(), p.next(), p.next(), p.next(), p.next(), p.next()) {
        if let (Ok(f), Ok(c), Ok(u), Ok(ch)) = (f.parse::<u64>(), c.parse::<u64>(), u.parse::<u64>(), ch.parse::<u64>()) {
          if let (Some(user), Some(channel)) = (Id::new_checked(u), Id::new_checked(ch)) {
            list.push(Reminder { user, channel, fire_ms: f, created_ms: c, ping: ping == "1", text: unescape(body) });
          }
        }
      }
    }
  }
  list
}

pub fn save_reminders(list: &[Reminder]) {
  let text: String = list
    .iter()
    .map(|r| {
      let body = r.text.replace('\\', "\\\\").replace('\r', "").replace('\n', "\\n");
      format!("{} {} {} {} {} {}\n", r.fire_ms, r.created_ms, r.user.get(), r.channel.get(), r.ping as u8, body)
    })
    .collect();
  if let Err(err) = std::fs::write("reminders.txt", text) {
    eprintln!("failed to save reminders.txt: {err}");
  }
}

pub fn load_timezones() -> HashMap<Id<UserMarker>, String> {
  let mut map = HashMap::new();
  if let Ok(text) = std::fs::read_to_string("timezones.txt") {
    for line in text.lines() {
      let mut p = line.split_whitespace();
      if let (Some(u), Some(zone)) = (p.next(), p.next()) {
        if let Some(u) = u.parse::<u64>().ok().and_then(Id::new_checked) {
          map.insert(u, zone.to_string());
        }
      }
    }
  }
  map
}

pub fn save_timezones(map: &HashMap<Id<UserMarker>, String>) {
  let text: String = map.iter().map(|(u, z)| format!("{} {}\n", u.get(), z)).collect();
  if let Err(err) = std::fs::write("timezones.txt", text) {
    eprintln!("failed to save timezones.txt: {err}");
  }
}

pub fn opt_bool(opts: &[CommandDataOption], name: &str) -> Option<bool> {
  opts.iter().find_map(|o| match &o.value {
    CommandOptionValue::Boolean(b) if o.name == name => Some(*b),
    _ => None,
  })
}