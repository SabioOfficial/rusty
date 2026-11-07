use std::sync::Arc;

use twilight_http::Client;
use twilight_model::{
  application::interaction::application_command::{CommandData, CommandOptionValue},
  channel::message::{AllowedMentions, MessageFlags},
  gateway::payload::incoming::{InteractionCreate, MessageCreate},
  http::interaction::{InteractionResponse, InteractionResponseType},
};
use twilight_util::builder::InteractionResponseDataBuilder;

use crate::{
  persist::save_redirects,
  state::{BoxErr, State},
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