use std::{sync::Arc, time::Duration};

use twilight_http::Client;
use twilight_model::{
  application::interaction::{
    application_command::CommandData,
    modal::{ModalInteractionComponent, ModalInteractionData},
  },
  channel::message::{
    MessageFlags,
    component::{ActionRow, Button, ButtonStyle, Component, Label, TextInput, TextInputStyle},
    AllowedMentions,
  },
  gateway::payload::incoming::InteractionCreate,
  http::interaction::{InteractionResponse, InteractionResponseType},
  id::{Id, marker::ChannelMarker},
};
use twilight_util::builder::InteractionResponseDataBuilder;

use crate::{
  persist::save_anchors,
  state::{Anchor, BoxErr, State},
  util::{is_gone, log_event, now_ms, reply_ephemeral, who},
};

async fn repost_anchor(http: &Client, state: &State, channel: Id<ChannelMarker>) -> Result<(), BoxErr> {
  let Some(Anchor { text, last, .. }) = state.anchors.read().unwrap().get(&channel).cloned() else {
    return Ok(());
  };
  let no_pings = AllowedMentions::default();
  let sent = http
    .create_message(channel)
    .content(&text)
    .flags(MessageFlags::SUPPRESS_NOTIFICATIONS)
    .allowed_mentions(Some(&no_pings))
    .await?
    .model()
    .await?;
  {
    if let Err(err) = http.create_pin(channel, sent.id).await {
      eprintln!("couldn't pin anchor in {channel}: {err}");
    }
    let mut map = state.anchors.write().unwrap();
    if let Some(a) = map.get_mut(&channel) {
      a.last = Some(sent.id);
    }
    save_anchors(&map);
  }
  if let Some(old) = last {
    if let Err(err) = http.delete_message(channel, old).await {
      if !is_gone(&err) {
        eprintln!("couldn't delete old anchor in {channel}: {err}");
      }
    }
  }
  Ok(())
}

pub fn bump_anchor(http: Arc<Client>, state: Arc<State>, channel: Id<ChannelMarker>) {
  let now = now_ms();
  let (tick, hushed, stale) = {
    let mut map = state.anchors.write().unwrap();
    let Some(a) = map.get_mut(&channel) else { return };
    a.tick += 1;
    let tick = a.tick;
    let hushed = a.unintrusive && now.saturating_sub(a.last_msg_ms) < 60000;
    a.last_msg_ms = now;
    let stale = if hushed { a.last.take() } else { None };
    if stale.is_some() {
      save_anchors(&map);
    }
    (tick, hushed, stale)
  };
  tokio::spawn(async move {
    if let Some(old) = stale {
      if let Err(err) = http.delete_message(channel, old).await {
        if !is_gone(&err) {
          eprintln!("couldn't pull anchor in {channel}: {err}");
        }
      }
    }
    let wait = if hushed { Duration::from_millis(60000) } else { Duration::from_millis(1500) };
    tokio::time::sleep(wait).await;
    let current = state.anchors.read().unwrap().get(&channel).map(|a| a.tick);
    if current == Some(tick) {
      if let Err(err) = repost_anchor(&http, &state, channel).await {
        eprintln!("anchor repost failed in {channel}: {err}");
      }
    }
  });
}

pub async fn handle_anchor(http: &Arc<Client>, state: &Arc<State>, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
  let channel = ev.channel.as_ref().map(|c| c.id);
  let sub = cmd.options.first();

  let text = match (sub, channel) {
    (Some(sub), Some(_channel)) if sub.name == "setup" => {
      let input = TextInput {
        custom_id: "text".into(),
        id: None,
        label: None,
        style: TextInputStyle::Paragraph,
        placeholder: Some("Powered by Rusty, an open-source and locally hosted Discord bot system powered by Rust!".into()),
        min_length: Some(1),
        max_length: Some(2000),
        required: Some(true),
        value: None,
      };
      let response = InteractionResponse {
        kind: InteractionResponseType::Modal,
        data: Some(
          InteractionResponseDataBuilder::new()
            .custom_id("anchor-setup")
            .title("Set anchor message")
            .components([Component::Label(Label {
              id: None,
              label: "Anchored message".into(),
              description: None,
              component: Box::new(Component::TextInput(input)),
            })])
            .build(),
        ),
      };
      http.interaction(ev.application_id)
        .create_response(ev.id, &ev.token, &response)
        .await?;
      return Ok(());
    }
    (Some(sub), Some(channel)) if sub.name == "off" => {
      let removed = {
        let mut map = state.anchors.write().unwrap();
        let r = map.remove(&channel);
        save_anchors(&map);
        r
      };
      if let Some(Anchor { last: Some(m), .. }) = removed {
        let _ = http.delete_message(channel, m).await;
      }
      log_event(http, state, ev.guild_id, format!("{} removed the anchor in <#{channel}>.", who(ev))).await;
      format!("Anchor removed from <#{channel}>.")
    }
    (Some(sub), Some(channel)) if sub.name == "settings" => {
      let on = state.anchors.read().unwrap().get(&channel).map(|a| a.unintrusive);
      let Some(on) = on else {
        return reply_ephemeral(http, ev, "There's no anchor in this channel yet. Run `/anchor setup` first.".into()).await;
      };
      let (content, components) = anchor_settings_view(on);
      let response = InteractionResponse {
        kind: InteractionResponseType::ChannelMessageWithSource,
        data: Some(
          InteractionResponseDataBuilder::new()
            .content(content)
            .components(components)
            .flags(MessageFlags::EPHEMERAL)
            .build(), 
        ),
      };
      http.interaction(ev.application_id)
        .create_response(ev.id, &ev.token, &response)
        .await?;
      return Ok(());
    }
    _ => "Use `/anchor` in a server channel.".to_string(),
  };

  reply_ephemeral(http, ev, text).await
}

fn modal_text<'a>(c: &'a ModalInteractionComponent, id: &str) -> Option<&'a str> {
  match c {
    ModalInteractionComponent::TextInput(t) if t.custom_id == id => Some(t.value.as_str()),
    ModalInteractionComponent::Label(l) => modal_text(&l.component, id),
    ModalInteractionComponent::ActionRow(r) => r.components.iter().find_map(|c| modal_text(c, id)),
    _ => None,
  }
}

pub async fn handle_anchor_modal(http: &Arc<Client>, state: &Arc<State>, ev: &InteractionCreate, modal: &ModalInteractionData) -> Result<(), BoxErr> {
  let Some(channel) = ev.channel.as_ref().map(|c| c.id) else {
    return Ok(());
  };
  let body = modal
    .components
    .iter()
    .find_map(|c| modal_text(c, "text"))
    .unwrap_or_default()
    .to_string();

  if body.trim().is_empty() {
    return reply_ephemeral(http, ev, "Give me some text to anchor.".into()).await;
  }

  {
    let mut map = state.anchors.write().unwrap();
    let (last, unintrusive) = map.get(&channel).map(|a| (a.last, a.unintrusive)).unwrap_or((None, false)); 
    map.insert(channel, Anchor { text: body, last, tick: 0, unintrusive, last_msg_ms: 0 });
    save_anchors(&map);
  }
  reply_ephemeral(http, ev, format!("Anchored in <#{channel}>. It will stay at the bottom.")).await?;
  log_event(http, state, ev.guild_id, format!("{} set an anchor in <#{channel}>.", who(ev))).await;

  if let Err(err) = repost_anchor(http, state, channel).await {
    eprintln!("initial anchor post failed in {channel}: {err}");
  }
  Ok(())
}

fn anchor_settings_view(on: bool) -> (String, Vec<Component>) {
  let word = if on { "On" } else { "Off" };
  let content = format!("**Anchor settings**\nUnintrusive Mode: **{word}**\n-# The anchor only updates after 1 minute of inactivity.");
  let button = Button {
    custom_id: Some("anchor-unintrusive".into()),
    disabled: false,
    emoji: None,
    id: None,
    label: Some((if on { "Turn off Unintrusive Mode" } else { "Turn on Intrusive Mode" }).to_string()),
    style: if on { ButtonStyle::Danger } else { ButtonStyle::Success },
    url: None,
    sku_id: None,
  };
  let row = ActionRow { id: None, components: vec![Component::Button(button)] };
  (content, vec![Component::ActionRow(row)])
}

pub async fn handle_anchor_toggle(http: &Client, state: &State, ev: &InteractionCreate) -> Result<(), BoxErr> {
  let Some(channel) = ev.channel.as_ref().map(|c| c.id) else {
    return Ok(());
  };
  let flipped = {
    let mut map = state.anchors.write().unwrap();
    let now_on = map.get_mut(&channel).map(|a| {
      a.unintrusive = !a.unintrusive;
      a.unintrusive
    });
    if now_on.is_some() {
      save_anchors(&map);
    }
    now_on
  };
  let Some(on) = flipped else {
    return reply_ephemeral(http, ev, "This channel's anchor is gone.".into()).await;
  };
  let (content, components) = anchor_settings_view(on);
  let response = InteractionResponse {
    kind: InteractionResponseType::UpdateMessage,
    data: Some(
      InteractionResponseDataBuilder::new()
        .content(content)
        .components(components)
        .build(),
    ),
  };
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  let word = if on { "on" } else { "off" };
  log_event(http, state, ev.guild_id, format!("{} turned {word} Unintrusive Mode for the anchor in <#{channel}>", who(ev))).await;
  Ok(())
}