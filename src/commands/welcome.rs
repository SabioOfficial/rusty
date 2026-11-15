use twilight_http::Client;
use twilight_model::{
  application::interaction::{
    message_component::MessageComponentInteractionData, modal::ModalInteractionData,
  },
  channel::{
    ChannelType,
    message::{
      AllowedMentions, MessageFlags,
      component::{ButtonStyle, Component, SelectMenuType, TextInput, TextInputStyle},
    },
  },
  gateway::payload::incoming::InteractionCreate,
  id::{
    Id,
    marker::{ChannelMarker, GuildMarker, UserMarker},
  },
};
use twilight_util::builder::{
  interaction_response::{ChannelMessageBuilder, ModalBuilder, UpdateMessageBuilder},
  message::{ActionRowBuilder, ButtonBuilder, LabelBuilder, SelectMenuBuilder},
};

use crate::{commands::anchor::modal_text, persist::save_welcomes, state::{BoxErr, State, Welcome}, util::{is_admin, log_event, reply_ephemeral, who}};

fn snapshot(state: &State, guild: Id<GuildMarker>) -> Welcome {
  state.welcomes.read().unwrap().get(&guild).cloned().unwrap_or_default()
}

fn update(state: &State, guild: Id<GuildMarker>, f: impl FnOnce(&mut Welcome)) {
  let mut map = state.welcomes.write().unwrap();
  f(map.entry(guild).or_default());
  save_welcomes(&map);
}

fn panel_view(cfg: &Welcome) -> (String, Vec<Component>) {
  let status = if cfg.enabled { "On" } else { "Off" };
  let channel = cfg.channel.map(|c| format!("<#{c}>")).unwrap_or_else(|| "Not set".to_string());
  let quoted = cfg.text.lines().map(|l| format!("> {l}")).collect::<Vec<_>>().join("\n");
  let content = format!(
    "## Welcome message\n\
    Status: **{status}**\n\
    Channel: {channel}\n\
    {quoted}\n\
    -# `{{user}}` mentions the new member, and `{{server}}` is the server name.\n\
    -# Requires the Server Members Privileged Gateway Intent for the feature to function."
  );

  let pick = SelectMenuBuilder::new("welcome-channel", SelectMenuType::Channel)
    .channel_types(vec![ChannelType::GuildText, ChannelType::GuildAnnouncement])
    .placeholder("Choose the welcome channel...")
    .build();
  let edit = ButtonBuilder::new(ButtonStyle::Primary)
    .custom_id("welcome-edit")
    .label("Edit message")
    .build();
  let toggle = ButtonBuilder::new(if cfg.enabled { ButtonStyle::Danger } else { ButtonStyle::Success })
    .custom_id("welcome-toggle")
    .label(if cfg.enabled { "Turn off" } else { "Turn on" })
    .build();
  let components: Vec<Component> = vec![ActionRowBuilder::new().component(pick).build().into(), ActionRowBuilder::new().component(edit).component(toggle).build().into()];
  (content, components)
}

#[allow(deprecated)]
fn text_input(value: String) -> Component {
  TextInput {
    custom_id: "text".into(),
    id: None,
    label: None,
    style: TextInputStyle::Paragraph,
    placeholder: Some("Welcome to {server}, {user}!".into()),
    min_length: Some(1),
    max_length: Some(1000),
    required: Some(true),
    value: Some(value),
  }
  .into()
}

async fn send_panel_update(http: &Client, state: &State, ev: &InteractionCreate, guild: Id<GuildMarker>) -> Result<(), BoxErr> {
  let (content, components) = panel_view(&snapshot(state, guild));
  let response = UpdateMessageBuilder::new().content(content).components(components).build();
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  Ok(())
}

pub async fn handle_welcome(http: &Client, state: &State, ev: &InteractionCreate) -> Result<(), BoxErr> {
  let Some(guild) = ev.guild_id else {
    return reply_ephemeral(http, ev, "Use `/welcome` in a **server**.".into()).await;
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "Only administrators can change the welcome message.".into()).await;
  }
  let (content, components) = panel_view(&snapshot(state, guild));
  let response = ChannelMessageBuilder::new()
    .content(content)
    .components(components)
    .flags(MessageFlags::EPHEMERAL)
    .build();
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  Ok(())
}

pub async fn handle_welcome_component(http: &Client, state: &State, ev: &InteractionCreate, comp: &MessageComponentInteractionData) -> Result<(), BoxErr> {
  let Some(guild) = ev.guild_id else {
    return Ok(());
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "Only administrators can change the welcome message.".into()).await;
  }

  match comp.custom_id.as_str() {
    "welcome-edit" => {
      let cfg = snapshot(state, guild);
      let response = ModalBuilder::new(
        "welcome-text",
        "Welcome message",
        [LabelBuilder::new("Welcome message", text_input(cfg.text)).description("Use {user} for a mention and {server} for the server name.").build()]
      )
      .build();
      http.interaction(ev.application_id)
        .create_response(ev.id, &ev.token, &response)
        .await?;
      Ok(())
    }
    "welcome-channel" => {
      let picked: Option<Id<ChannelMarker>> = comp
        .values
        .first()
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(Id::new_checked);
      let Some(channel) = picked else {
        return Ok(());
      };
      update(state, guild, |c| c.channel = Some(channel));
      log_event(http, state, Some(guild), format!("{} set the welcome channel to <#{channel}>.", who(ev))).await;
      send_panel_update(http, state, ev, guild).await
    }
    "welcome-toggle" => {
      let cfg = snapshot(state, guild);
      if !cfg.enabled && cfg.channel.is_none() {
        return reply_ephemeral(http, ev, "Pick a welcome channel first.".into()).await;
      }
      let now_on = !cfg.enabled;
      update(state, guild, |c| c.enabled = now_on);
      let word = if now_on { "on" } else { "off" };
      log_event(http, state, Some(guild), format!("{} turned the welcome message {word}.", who(ev))).await;
      send_panel_update(http, state, ev, guild).await
    }
    _ => Ok(()),
  }
}

pub async fn handle_welcome_modal(http: &Client, state: &State, ev: &InteractionCreate, modal: &ModalInteractionData) -> Result<(), BoxErr> {
  let Some(guild) = ev.guild_id else {
    return Ok(());
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "Only administrators can change the welcome message.".into()).await;
  }
  let text = modal
    .components
    .iter()
    .find_map(|c| modal_text(c, "text"))
    .unwrap_or_default()
    .trim()
    .to_string();
  if text.is_empty() {
    return reply_ephemeral(http, ev, "The message can't be empty.".into()).await;
  }
  update(state, guild, |c| c.text = text);
  log_event(http, state, Some(guild), format!("{} changed the welcome message.", who(ev))).await;
  send_panel_update(http, state, ev, guild).await
}

pub async fn send_welcome(http: &Client, state: &State, guild: Id<GuildMarker>, user: Id<UserMarker>) {
  let cfg = snapshot(state, guild);
  let Some(channel) = cfg.channel.filter(|_| cfg.enabled) else {
    return;
  };
  let server = match http.guild(guild).await {
    Ok(resp) => resp.model().await.map(|g| g.name).unwrap_or_else(|_| "the server".to_string()),
    Err(_) => "the server".to_string(),
  };
  let content = cfg.text.replace("{user}", &format!("<@{user}>")).replace("{server}", &server);
  let mentions = AllowedMentions { users: vec![user], ..Default::default() };
  if let Err(err) = http
    .create_message(channel)
    .content(&content)
    .allowed_mentions(Some(&mentions))
    .await
  {
    eprintln!("welcome message failed in {channel}: {err}");
  }
}