use std::sync::Arc;

use twilight_http::Client;
use twilight_model::{
  application::interaction::application_command::{CommandData, CommandOptionValue},
  channel::message::{AllowedMentions, MessageFlags},
  gateway::payload::incoming::InteractionCreate,
  http::interaction::{InteractionResponse, InteractionResponseType},
};
use twilight_util::builder::InteractionResponseDataBuilder;

use crate::{
  persist::save_disappear,
  scheduler::catch_up,
  state::{BoxErr, Disappear, State},
  util::{human, log_event, now_ms, unit_secs, who},
};

pub async fn handle_disappearing(http: &Arc<Client>, state: &State, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
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
            log_event(http, state, ev.guild_id, format!("{} set disappearing messages in <#{channel}> to {}.", who(ev), human(secs))).await;
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
      log_event(http, state, ev.guild_id, format!("{} turned off disappearing messages in <#{channel}>.", who(ev))).await;
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