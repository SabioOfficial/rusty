use std::collections::HashMap;

use twilight_http::Client;
use twilight_model::{
  application::interaction::{
    message_component::MessageComponentInteractionData,
    modal::ModalInteractionData,
  },
  channel::message::{
    AllowedMentions, MessageFlags,
    component::{ButtonStyle, Component, SelectMenuType, TextInput, TextInputStyle},
  },
  gateway::payload::incoming::InteractionCreate,
  guild::Permissions,
  http::interaction::InteractionResponse,
  id::{
    Id,
    marker::{ChannelMarker, GuildMarker},
  },
};
use twilight_util::builder::{
  interaction_response::{ChannelMessageBuilder, ModalBuilder, UpdateMessageBuilder},
  message::{ActionRowBuilder, ButtonBuilder, LabelBuilder, SelectMenuBuilder, SelectMenuOptionBuilder},
};

use super::anchor::modal_text;
use crate::{
  persist::save_autoreplies,
  state::{BoxErr, State},
  util::{log_event, reply_ephemeral, who},
};

fn is_admin(ev: &InteractionCreate) -> bool {
  ev.member
    .as_ref()
    .and_then(|m| m.permissions)
    .is_some_and(|p| p.contains(Permissions::ADMINISTRATOR))
}

fn clean_name(raw: &str) -> Option<String> {
  let n = raw.trim().trim_start_matches("!!").to_lowercase();
  let ok = !n.is_empty() && n.chars().count() <= 32 && n.chars().all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_');
  ok.then_some(n)
}

fn snapshot(state: &State, guild: Id<GuildMarker>) -> HashMap<String, String> {
  state.autoreplies.read().unwrap().get(&guild).cloned().unwrap_or_default()
}

fn preview(body: &str) -> String {
  let one = body.split_whitespace().collect::<Vec<_>>().join(" ");
  let cut: String = one.chars().take(90).collect();
  if one.chars().count() > 90 { format!("{cut}...") } else { cut }
}

fn panel_view(replies: &HashMap<String, String>) -> (String, Vec<Component>) {
  let mut names: Vec<&String> = replies.keys().collect();
  names.sort();
  let list = if names.is_empty() {
    "None yet.".to_string()
  } else {
    names.iter().map(|n| format!("`!!{n}`")).collect::<Vec<_>>().join(", ")
  };
  let content = format!(
    "## Auto-replies\n\
    Setup auto-replies. The format for the auto-replies is `!!name`.\n\
    {list}\n\
    -# {}/25 used. Names aren't case-sensitive.",
    names.len()
  );

  let add = ButtonBuilder::new(ButtonStyle::Success)
    .custom_id("autoreply-add")
    .label("Add reply")
    .build();
  let mut components: Vec<Component> = vec![ActionRowBuilder::new().component(add).build().into()];

  if !names.is_empty() {
    for (id, placeholder) in [("autoreply-edit", "Edit a reply..."), ("autoreply-remove", "Remove a reply...")] {
      let mut menu = SelectMenuBuilder::new(id, SelectMenuType::Text).placeholder(placeholder);
      for &n in &names {
        menu = menu.option(SelectMenuOptionBuilder::new(format!("!!{n}"), n.clone()).description(preview(&replies[n])));
      }
      components.push(ActionRowBuilder::new().component(menu.build()).build().into());
    }
  }
  (content, components)
}

#[allow(deprecated)]
fn input(id: &str, style: TextInputStyle, placeholder: &str, max: u16, value: Option<String>) -> Component {
  TextInput {
    custom_id: id.into(),
    id: None,
    label: None,
    style,
    placeholder: Some(placeholder.into()),
    min_length: Some(1),
    max_length: Some(max),
    required: Some(true),
    value,
  }
  .into()
}

fn reply_modal(custom_id: String, prefill: Option<(&str, &str)>) -> InteractionResponse {
  let (name, body) = match prefill {
    Some((n, b)) => (Some(n.to_string()), Some(b.to_string())),
    None => (None, None),
  };
  ModalBuilder::new(
    custom_id,
    "Auto-reply",
    [
      LabelBuilder::new("Trigger name", input("name", TextInputStyle::Short, "help", 40, name))
        .description("People can type !!name to trigger the auto reply. Only letters, numbers, - and _ are accepted.")
        .build(),
      LabelBuilder::new("Reply", input("body", TextInputStyle::Paragraph, "blah-blah-blah-blah-blah-blah-blah-blah", 2000, body))
        .description("Up to 2000 characters. Mentions won't ping anyone.")
        .build(),
    ],
  )
  .build()
}

async fn send_panel_update(http: &Client, state: &State, ev: &InteractionCreate, guild: Id<GuildMarker>) -> Result<(), BoxErr> {
  let (content, components) = panel_view(&snapshot(state, guild));
  let response = UpdateMessageBuilder::new().content(content).components(components).build();
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  Ok(())
}

pub async fn handle_autoreply(http: &Client, state: &State, ev: &InteractionCreate) -> Result<(), BoxErr> {
  let Some(guild) = ev.guild_id else {
    return reply_ephemeral(http, ev, "Use `/autoreply` in a server.".into()).await;
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "Only administrators can manage auto-replies.".into()).await;
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

pub async fn handle_autoreply_component(http: &Client, state: &State, ev: &InteractionCreate, comp: &MessageComponentInteractionData) -> Result<(), BoxErr> {
  let Some(guild) = ev.guild_id else {
    return Ok(());
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "Only administrators can manage auto-replies.".into()).await;
  }

  match comp.custom_id.as_str() {
    "autoreply-add" => {
      let response = reply_modal("autoreply-add".into(), None);
      http.interaction(ev.application_id)
        .create_response(ev.id, &ev.token, &response)
        .await?;
      Ok(())
    }
    "autoreply-edit" => {
      let Some(name) = comp.values.first() else {
        return Ok(());
      };
      let body = state.autoreplies.read().unwrap().get(&guild).and_then(|m| m.get(name)).cloned();
      let Some(body) = body else {
        return reply_ephemeral(http, ev, "That reply no longer exists. Run `/autoreply` again.".into()).await;
      };
      let response = reply_modal(format!("autoreply-edit:{name}"), Some((name, &body)));
      http.interaction(ev.application_id)
        .create_response(ev.id, &ev.token, &response)
        .await?;
      Ok(())
    }
    "autoreply-remove" => {
      let Some(name) = comp.values.first() else {
        return Ok(());
      };
      {
        let mut map = state.autoreplies.write().unwrap();
        let empty = map
          .get_mut(&guild)
          .map(|m| {
            m.remove(name);
            m.is_empty()
          })
          .unwrap_or(false);
        if empty {
          map.remove(&guild);
        }
        save_autoreplies(&map);
      }
      log_event(http, state, Some(guild), format!("{} removed the auto-reply `!!{name}`.", who(ev))).await;
      send_panel_update(http, state, ev, guild).await
    }
    _ => Ok(()),
  }
}

pub async fn handle_autoreply_modal(http: &Client, state: &State, ev: &InteractionCreate, modal: &ModalInteractionData) -> Result<(), BoxErr> {
  let Some(guild) = ev.guild_id else {
    return Ok(());
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "Only administrators can manage auto-replies.".into()).await;
  }

  let old = modal.custom_id.strip_prefix("autoreply-edit:").map(str::to_string);
  let field = |id: &str| modal.components.iter().find_map(|c| modal_text(c, id)).unwrap_or_default();

  let Some(name) = clean_name(field("name")) else {
    return reply_ephemeral(http, ev, format!("Names can only use letters, numbers, `-` and `_` (up to 32 characters).")).await;
  };
  let body = field("body").trim().to_string();
  if body.is_empty() {
    return reply_ephemeral(http, ev, "The reply can't be empty.".into()).await;
  }

  let outcome: Result<(), String> = {
    let mut map = state.autoreplies.write().unwrap();
    let mut next = map.get(&guild).cloned().unwrap_or_default();
    if let Some(o) = &old {
      next.remove(o);
    }
    if next.contains_key(&name) {
      Err(format!("`!!{name}` already exists. You must pick another name!"))
    } else if next.len() >= 25 {
      Err(format!("You've reached the max limit of 25 auto-replies."))
    } else {
      next.insert(name.clone(), body);
      map.insert(guild, next);
      save_autoreplies(&map);
      Ok(())
    }
  };
  if let Err(msg) = outcome {
    return reply_ephemeral(http, ev, msg).await;
  }

  let thewordthing = if old.is_some() { "edited" } else { "added" };
  log_event(http, state, Some(guild), format!("{} {thewordthing} the auto-reply `!!{name}`.", who(ev))).await;
  send_panel_update(http, state, ev, guild).await
}

pub fn find_reply(state: &State, guild: Id<GuildMarker>, content: &str) -> Option<String> {
  let rest = content.strip_prefix("!!")?;
  let name = rest.split_whitespace().next()?.to_lowercase();
  state.autoreplies.read().unwrap().get(&guild)?.get(&name).cloned()
}

pub async fn send_reply(http: &Client, channel: Id<ChannelMarker>, body: String) -> Result<(), BoxErr> {
  let no_pings = AllowedMentions::default();
  http.create_message(channel)
    .content(&body)
    .allowed_mentions(Some(&no_pings))
    .await?;
  Ok(())
}