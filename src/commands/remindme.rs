use std::{sync::Arc, time::Duration};

use tokio::time::MissedTickBehavior;
use twilight_http::Client;
use twilight_model::{application::interaction::application_command::CommandData, channel::message::AllowedMentions, gateway::payload::incoming::InteractionCreate};

use crate::{state::{BoxErr, Reminder, State}, timeparse::{parse_when, parse_zone}, util::{now_ms, opt_bool, opt_str, reply_ephemeral, save_reminders, save_timezones}};

pub async fn handle_remindme(http: &Client, state: &State, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
  let (Some(user), Some(channel)) = (ev.author_id(), ev.channel.as_ref().map(|c| c.id)) else {
    return reply_ephemeral(http, ev, "I couldn't tell where to send that reminder.".into()).await;
  };
  let opts = &cmd.options;
  let text = opt_str(opts, "text").unwrap_or_default().trim().to_string();
  if text.is_empty() {
    return reply_ephemeral(http, ev, "Tell me what to remind you about.".into()).await;
  }
  let when = opt_str(opts, "when").unwrap_or_default();
  let ping = opt_bool(opts, "ping").unwrap_or(true);

  let zone = match opt_str(opts, "timezone") {
    Some(raw) => {
      let Some(z) = parse_zone(raw) else {
        return reply_ephemeral(http, ev, "I don't know that timezone. Use a name like `Europe/London` or `America/New_York`, or an offset like `UTC+2`.".into()).await;
      };
      let label = z.label();
      let changed = state.timezones.read().unwrap().get(&user) != Some(&label);
      if changed {
        let mut map = state.timezones.write().unwrap();
        map.insert(user, label);
        save_timezones(&map);
      }
      Some(z)
    }
    None => state.timezones.read().unwrap().get(&user).and_then(|s| parse_zone(s)),
  };

  let now = now_ms();
  let fire_ms = match parse_when(when, now, zone.as_ref()) {
    Ok(ms) => ms,
    Err(e) => return reply_ephemeral(http, ev, e.message().into()).await,
  };

  let added = {
    let mut list = state.reminders.write().unwrap();
    if list.iter().filter(|r| r.user == user).count() >= 50 {
      false
    } else {
      list.push(Reminder { user, channel, fire_ms, created_ms: now, ping, text });
      save_reminders(&list);
      true
    }
  };
  if !added {
    return reply_ephemeral(http, ev, format!("You already have 50 pending reminders. You cannot add anymore until one goes off.")).await;
  }

  let ts = fire_ms / 1000;
  let place = if ev.guild_id.is_some() { format!("in <#{channel}>") } else { "here in this DM".to_string() };
  let ping_note = if ping { "I'll ping you." } else { "I won't ping you." };
  reply_ephemeral(http, ev, format!("Okay! I'll remind you <t:{ts}:R> (<t:{ts}:F>) {place}. {ping_note}")).await
}

pub async fn reminder_loop(http: Arc<Client>, state: Arc<State>) {
  let mut tick = tokio::time::interval(Duration::from_secs(1));
  tick.set_missed_tick_behavior(MissedTickBehavior::Delay);
  loop {
    tick.tick().await;
    let now = now_ms();
    let due: Vec<Reminder> = {
      let mut list = state.reminders.write().unwrap();
      if list.iter().all(|r| r.fire_ms > now) {
        Vec::new()
      } else {
        let (due, keep): (Vec<_>, Vec<_>) = list.drain(..).partition(|r| r.fire_ms <= now);
        *list = keep;
        save_reminders(&list);
        due
      }
    };
    for r in due {
      let http = Arc::clone(&http);
      tokio::spawn(async move { deliver(&http, r).await });
    }
  }
}

async fn deliver(http: &Client, r: Reminder) {
  let quoted = r.text.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
  let mut content = format!("<@{}>, you asked me to remind you <t:{}:R>:\n{quoted}", r.user, r.created_ms / 1000);
  if now_ms().saturating_sub(r.fire_ms) > 60000 {
    content.push_str(&format!("\n-# Sorry, this was due <t:{}:R>.", r.fire_ms / 1000));
  }
  let mentions = AllowedMentions {
    users: if r.ping { vec![r.user] } else { Vec::new() },
    ..Default::default()
  };
  let sent = http
    .create_message(r.channel)
    .content(&content)
    .allowed_mentions(Some(&mentions))
    .await;
  if let Err(err) = sent {
    eprintln!("reminder send failed in {} for {}: {err}", r.channel, r.user);
    let dm = match http.create_private_channel(r.user).await {
      Ok(resp) => resp.model().await.ok(),
      Err(_) => None,
    };
    if let Some(dm) = dm {
      if let Err(err) = http.create_message(dm.id).content(&content).await {
        eprintln!("reminder DM fallback failed for {}: {err}", r.user);
      }
    }
  }
}