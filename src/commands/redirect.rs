use std::sync::Arc;

use twilight_http::Client;
use twilight_model::{
  application::interaction::application_command::{CommandData, CommandOptionValue},
  channel::message::AllowedMentions,
  gateway::payload::incoming::{InteractionCreate, MessageCreate},
};

use crate::{
  persist::save_redirects,
  state::{BoxErr, State},
  util::reply_ephemeral,
};

pub async fn handle_person_redirect(http: &Client, state: &State, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
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

  reply_ephemeral(http, ev, text).await
}

pub async fn handle_message(http: Arc<Client>, state: Arc<State>, msg: Box<MessageCreate>) -> Result<(), BoxErr> {
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