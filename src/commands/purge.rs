use twilight_http::Client;
use twilight_model::{
  application::interaction::application_command::CommandData,
  gateway::payload::incoming::InteractionCreate,
  id::{
    Id,
    marker::{ChannelMarker, MessageMarker},
  },
};

use crate::{
  state::{BoxErr, State},
  util::{deferred_ephemeral, is_admin, log_event, now_ms, opt_int, reply_ephemeral, snowflake_ms, who},
};

async fn purge(http: &Client, channel: Id<ChannelMarker>, amount: u16) -> Result<(usize, usize), BoxErr> {
  let msgs = http.channel_messages(channel).limit(amount).await?.models().await?;
  let cutoff = now_ms().saturating_sub(1_123_200_000);

  let mut pinned = 0;
  let (mut fresh, mut old): (Vec<Id<MessageMarker>>, Vec<Id<MessageMarker>>) = (Vec::new(), Vec::new());
  for m in &msgs {
    if m.pinned {
      pinned += 1;
    } else if snowflake_ms(m.id.get()) > cutoff {
      fresh.push(m.id);
    } else {
      old.push(m.id);
    }
  }

  let mut deleted = 0;
  let mut singles = old;
  if fresh.len() >= 2 && http.delete_messages(channel, &fresh).await.is_ok() {
    deleted += fresh.len();
  } else {
    singles.extend(fresh);
  }
  for m in singles {
    if http.delete_message(channel, m).await.is_ok() {
      deleted += 1;
    }
  }
  Ok((deleted, pinned))
}

pub async fn handle_purge(http: &Client, state: &State, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
  let (Some(guild), Some(channel)) = (ev.guild_id, ev.channel.as_ref().map(|c| c.id)) else {
    return reply_ephemeral(http, ev, "Use `/purge` in a **server channel**.".into()).await;
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "Only administrators can purge messages.".into()).await;
  }
  let amount = opt_int(&cmd.options, "amount").unwrap_or(0);
  if !(1..=100).contains(&amount) {
    return reply_ephemeral(http, ev, "Pick an amount from 1 to 100.".into()).await;
  }

  let iclient = http.interaction(ev.application_id);
  iclient.create_response(ev.id, &ev.token, &deferred_ephemeral()).await?;

  let text = match purge(http, channel, amount as u16).await {
    Ok((deleted, pinned)) => {
      log_event(http, state, Some(guild), format!("{} purged {deleted} message(s) in <#{channel}>.", who(ev))).await;
      let skipped = if pinned > 0 { format!(" Skipped {pinned} pinned message(s).") } else { String::new() }; // how do you even have more than one pinned message..?
      format!("Deleted {deleted} message(s).{skipped}")
    }
    Err(err) => {
      eprintln!("purge failed in {channel}: {err}");
      "Something went wrong. Make sure I have Manage Messages and Read Message History permissions.".to_string()
    }
  };
  iclient.update_response(&ev.token).content(Some(&text)).await?;
  Ok(())
}