use std::{sync::{Arc, atomic::Ordering::Relaxed}, time::Duration};

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
  util::{human, is_gone, log_event, now_ms, reply_ephemeral, who},
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
  state.stats.anchors.fetch_add(1, Relaxed);
  {
    if let Err(err) = http.create_pin(channel, sent.id).await {
      eprintln!("couldn't pin anchor in {channel}: {err}");
    }
    let mut map = state.anchors.write().unwrap();
    if let Some(a) = map.get_mut(&channel) {
      a.last = Some(sent.id);
      a.pending = 0;
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
  let (tick, wait_ms, stale) = {
    let mut map = state.anchors.write().unwrap();
    let Some(a) = map.get_mut(&channel) else { return };
    a.tick += 1;
    a.pending += 1;
    let tick = a.tick;
    let due = a.pending >= a.min_msgs;
    let hushed = a.unintrusive && due && now.saturating_sub(a.last_msg_ms) < a.quiet_ms;
    a.last_msg_ms = now;
    let stale = if hushed { a.last.take() } else { None };
    let wait_ms = if a.unintrusive && !hushed { a.quiet_ms.min(1500) } else { a.quiet_ms };
    if stale.is_some() {
      save_anchors(&map);
    }
    (tick, wait_ms, stale)
  };
  tokio::spawn(async move {
    if let Some(old ) = stale {
      if let Err(err) = http.delete_message(channel, old).await {
        if !is_gone(&err) {
          eprintln!("couldn't pull anchor in {channel}: {err}");
        }
      }
    }
    tokio::time::sleep(Duration::from_millis(wait_ms)).await;
    let go = state
      .anchors
      .read()
      .unwrap()
      .get(&channel)
      .map(|a| a.tick == tick && a.pending >= a.min_msgs)
      .unwrap_or(false);
    if go {
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
      let snapshot = state
        .anchors
        .read()
        .unwrap()
        .get(&channel)
        .map(|a| (a.unintrusive, a.quiet_ms, a.min_msgs));
      let Some((on, quiet_ms, min_msgs)) = snapshot else {
        return reply_ephemeral(http, ev, "There's no anchor in this channel yet. Run `/anchor setup` first.".into()).await;
      };
      let (content, components) = anchor_settings_view(on, quiet_ms, min_msgs);
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
    let (last, unintrusive, quiet_ms, min_msgs) = map
      .get(&channel)
      .map(|a| (a.last, a.unintrusive, a.quiet_ms, a.min_msgs))
      .unwrap_or((None, false, 1500, 1));
    map.insert(channel, Anchor { text: body, last, tick: 0, unintrusive, last_msg_ms: 0, quiet_ms, min_msgs, pending: 0 });
    save_anchors(&map);
  }
  reply_ephemeral(http, ev, format!("Anchored in <#{channel}>. It will stay at the bottom.")).await?;
  log_event(http, state, ev.guild_id, format!("{} set an anchor in <#{channel}>.", who(ev))).await;

  if let Err(err) = repost_anchor(http, state, channel).await {
    eprintln!("initial anchor post failed in {channel}: {err}");
  }
  Ok(())
}

fn fmt_wait(ms: u64) -> String {
  if ms % 1000 == 0 { human(ms / 1000) } else { format!("{:.1} seconds", ms as f64 / 1000.0) }
}

fn anchor_settings_view(on: bool, quiet_ms: u64, min_msgs: u64) -> (String, Vec<Component>) {
  let word = if on { "On" } else { "Off" };
  let content = format!(
    "## Anchor settings\n\
    Unintrusive Mode: **{word}**\n\
    Wait: **{}** of quiet before the anchor moves\n\
    Messages: **{min_msgs}** needed before the anchor moves\n\
    -# Unintrusive Mode hides the anchor while chat is active and brings it back after the wait.",
    fmt_wait(quiet_ms)
  );
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
  let timing = Button {
    custom_id: Some("anchor-timing-open".into()),
    disabled: false,
    emoji: None,
    id: None,
    label: Some("Edit timing".into()),
    style: ButtonStyle::Primary,
    url: None,
    sku_id: None,
  };
  let row = ActionRow { id: None, components: vec![Component::Button(button), Component::Button(timing)] };
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
      (a.unintrusive, a.quiet_ms, a.min_msgs)
    });
    if now_on.is_some() {
      save_anchors(&map);
    }
    now_on
  };
  let Some((on, quiet_ms, min_msgs)) = flipped else {
    return reply_ephemeral(http, ev, "This channel's anchor is gone.".into()).await;
  };
  let (content, components) = anchor_settings_view(on, quiet_ms, min_msgs);
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

fn parse_wait(s: &str) -> Option<u64> {
  let s = s.trim().to_lowercase();
  let (num, mult) = if let Some(n) = s.strip_suffix('h') {
    (n, 3600000.0)
  } else if let Some(n) = s.strip_suffix('m') {
    (n, 60000.0)
  } else if let Some(n) = s.strip_suffix('s') {
    (n, 1000.0)
  } else {
    (s.as_str(), 1000.0)
  };
  let v: f64 = num.trim().parse().ok()?;
  if !v.is_finite() || v <= 0.0 {
    return None;
  }
  let ms = (v * mult).round() as u64;
  (100..=86400000).contains(&ms).then_some(ms)
}

fn wait_input(ms: u64) -> String {
  if ms % 3600000 == 0 {
    format!("{}h", ms / 3600000)
  } else if ms % 60000 == 0 {
    format!("{}m", ms / 60000)
  } else {
    format!("{}s", ms as f64 / 1000.0)
  }
}

fn field<'a>(modal: &'a ModalInteractionData, id: &str) -> &'a str {
  modal.components.iter().find_map(|c| modal_text(c, id)).unwrap_or_default()
}

fn text_field(label: &str, description: &str, id: &str, placeholder: &str, max: u16, value: String) -> Component {
  Component::Label(Label {
    id: None,
    label: label.into(),
    description: Some(description.into()),
    component: Box::new(Component::TextInput(TextInput {
      custom_id: id.into(),
      id: None,
      label: None,
      style: TextInputStyle::Short,
      placeholder: Some(placeholder.into()),
      min_length: Some(1),
      max_length: Some(max),
      required: Some(true),
      value: Some(value),
    })),
  })
}

pub async fn handle_anchor_timing_button(http: &Client, state: &State, ev: &InteractionCreate) -> Result<(), BoxErr> {
  let Some(channel) = ev.channel.as_ref().map(|c| c.id) else {
    return Ok(());
  };
  let snapshot = state.anchors.read().unwrap().get(&channel).map(|a| (a.quiet_ms, a.min_msgs));
  let Some((quiet_ms, min_msgs)) = snapshot else {
    return reply_ephemeral(http, ev, "This channel's anchor is gone.".into()).await;
  };
  let response = InteractionResponse {
    kind: InteractionResponseType::Modal,
    data: Some(
      InteractionResponseDataBuilder::new()
        .custom_id("anchor-timing")
        .title("Anchor timing")
        .components([
          text_field(
            "Wait before moving",
            "How long the channel must be quiet. Use s, m or h (max 24h).",
            "wait",
            "e.g. 1.5s, 30s, 5m, 1h",
            10,
            wait_input(quiet_ms),
          ),
          text_field(
            "Messages before moving",
            "How many messages must be sent first (1-100).",
            "messages",
            "e.g. 1, 5, 20",
            3,
            min_msgs.to_string(),
          ),
        ])
        .build(),
    ),
  };
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  Ok(())
}

pub async fn handle_anchor_timing_modal(http: &Client, state: &State, ev: &InteractionCreate, modal: &ModalInteractionData) -> Result<(), BoxErr> {
  let Some(channel) = ev.channel.as_ref().map(|c| c.id) else {
    return Ok(());
  };
  let Some(wait_ms) = parse_wait(field(modal, "wait")) else {
    return reply_ephemeral(http, ev, "Invalid syntax provided for 'wait'! Use a number with s, m or h, like `30s`, `5m` or `1h` (0.1s to 24h).".into()).await;
  };
  let Some(min_msgs) = field(modal, "messages")
    .trim()
    .parse::<u64>()
    .ok()
    .filter(|n| (1..=100).contains(n))
  else {
    return reply_ephemeral(http, ev, "Invalid syntax provided for 'messages'! It must be a whole number from 1 to 100.".into()).await;
  };

  let snapshot = {
    let mut map = state.anchors.write().unwrap();
    let on = map.get_mut(&channel).map(|a| {
      a.quiet_ms = wait_ms;
      a.min_msgs = min_msgs;
      a.unintrusive
    });
    if on.is_some() {
      save_anchors(&map);
    }
    on
  };
  let Some(on) = snapshot else {
    return reply_ephemeral(http, ev, "This channel's anchor is gone.".into()).await;
  };

  let (content, components) = anchor_settings_view(on, wait_ms, min_msgs);
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
  log_event(http, state, ev.guild_id, format!("{} changed anchor timing in <#{channel}>: wait {}, {min_msgs} message(s).", who(ev), fmt_wait(wait_ms))).await;
  Ok(())
}