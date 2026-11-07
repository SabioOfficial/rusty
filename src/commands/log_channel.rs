use twilight_http::Client;
use twilight_model::{
  application::interaction::application_command::{CommandData, CommandOptionValue},
  channel::message::MessageFlags,
  gateway::payload::incoming::InteractionCreate,
};

use crate::{
  persist::save_logs,
  state::{BoxErr, State},
  util::{opt_channel, reply_ephemeral},
};

pub async fn handle_log_channel(http: &Client, state: &State, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
  let text = match (ev.guild_id, cmd.options.first()) {
    (Some(guild), Some(sub)) if sub.name == "setup" => {
      let target = match &sub.value {
        CommandOptionValue::SubCommand(o) => opt_channel(o, "channel"),
        _ => None,
      };
      match target {
        Some(channel) => {
          let probe = http
            .create_message(channel)
            .content("Log channel set. Bot actions will be posted here.")
            .flags(MessageFlags::SUPPRESS_NOTIFICATIONS)
            .await;
          if probe.is_err() {
            format!("I can't send messages in <#{channel}>. Give me View Channel and Send Messages permissions there.")
          } else {
            let mut map = state.logs.write().unwrap();
            map.insert(guild, channel);
            save_logs(&map);
            format!("Logs will now be posted in <#{channel}>.")
          }
        }
        None => "Pick a text channel.".to_string(),
      }
    }
    (Some(guild), Some(sub)) if sub.name == "off" => {
      let mut map = state.logs.write().unwrap();
      map.remove(&guild);
      save_logs(&map);
      "Logging is off.".to_string()
    }
    _ => "Use `/log-channel` in a server.".to_string(),
  };
  reply_ephemeral(http, ev, text).await
}