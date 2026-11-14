use std::{sync::{Arc, atomic::Ordering::Relaxed}, time::Duration};

use twilight_http::{Client, error::ErrorType};
use twilight_model::{
  application::interaction::{application_command::{CommandData, CommandOptionValue}, message_component::MessageComponentInteractionData}, channel::{
    ChannelType, message::{Component, MessageFlags, component::{ButtonStyle, SelectMenuType}}, permission_overwrite::{
      PermissionOverwrite as ChannelOverwrite, PermissionOverwriteType as ChannelOverwriteType,
    },
  }, gateway::payload::incoming::InteractionCreate, guild::Permissions, http::permission_overwrite::{PermissionOverwrite, PermissionOverwriteType}, id::{
    Id,
    marker::{ChannelMarker, GenericMarker, UserMarker},
  },
};
use twilight_util::builder::{interaction_response::{ChannelMessageBuilder, UpdateMessageBuilder}, message::{ActionRowBuilder, ButtonBuilder, SelectMenuBuilder}};

use crate::{
  persist::{save_vc_categories, save_vcs}, state::{BoxErr, PrivateVc, State}, util::{deferred_ephemeral, is_admin, log_event, now_ms, opt_channel, opt_int, opt_str, opt_user, reply_ephemeral, unit_secs, who},
};

pub fn spawn_vc_timer(http: Arc<Client>, state: Arc<State>, channel: Id<ChannelMarker>, expires_ms: u64) {
  tokio::spawn(async move {
    tokio::time::sleep(Duration::from_millis(expires_ms.saturating_sub(now_ms()))).await;
    let still_tracked = state.vcs.read().unwrap().contains_key(&channel);
    if still_tracked {
      delete_vc(&http, &state, channel).await;
    }
  });
}

async fn delete_vc(http: &Client, state: &State, channel: Id<ChannelMarker>) {
  let removable = match http.delete_channel(channel).await {
    Ok(_) => true,
    Err(err) => {
      let gone = matches!(err.kind(), ErrorType::Response { status, .. } if status.get() == 404);
      if !gone {
        eprintln!("failed to delete private vc {channel}: {err}");
      }
      gone
    }
  };
  if removable {
    let mut map = state.vcs.write().unwrap();
    map.remove(&channel);
    save_vcs(&map);
  }
}

async fn allow_member(http: &Client, channel: Id<ChannelMarker>, user: Id<UserMarker>) -> Result<(), BoxErr> {
  http.update_channel_permission(
    channel,
    &PermissionOverwrite {
      allow: Some(Permissions::VIEW_CHANNEL.union(Permissions::CONNECT).union(Permissions::SPEAK)),
      deny: None,
      id: user.cast(),
      kind: PermissionOverwriteType::Member,
    },
  )
  .await?;
  Ok(())
}

async fn send_dm(http: &Client, user: Id<UserMarker>, text: &str) -> Result<(), BoxErr> {
  let dm = http.create_private_channel(user).await?.model().await?;
  http.create_message(dm.id).content(text).await?;
  Ok(())
}

pub async fn handle_private_vc(http: &Arc<Client>, state: &Arc<State>, ev: &InteractionCreate, cmd: &CommandData) -> Result<(), BoxErr> {
  if cmd.options.first().is_some_and(|o| o.name == "settings") {
    return handle_vc_settings(http, state, ev).await;
  }
  let iclient = http.interaction(ev.application_id);
  let ack = deferred_ephemeral();
  iclient.create_response(ev.id, &ev.token, &ack).await?;

  let text = match run_private_vc(http, state, ev, cmd).await {
    Ok(t) => t,
    Err(err) => {
      eprintln!("private vc error: {err}");
      "Something went wrong. Make sure I have Manage Channels and Manage Roles permissions.".to_string()
    }
  };
  iclient.update_response(&ev.token).content(Some(&text)).await?;
  Ok(())
}

fn settings_view(current: Option<Id<ChannelMarker>>) -> (String, Vec<Component>) {
  let shown = match current {
    Some(c) => format!("<#{c}>"),
    None => "None".to_string(),
  };
  let content = format!(
    "## Private VC settings\n\
    Category: {shown}\n\
    -# New private VCs will be created there. Existing VCs are not affected by this change."
  );
  let pick = SelectMenuBuilder::new("vc-category-pick", SelectMenuType::Channel)
    .channel_types(vec![ChannelType::GuildCategory])
    .placeholder("Choose a category...")
    .build();
  let clear = ButtonBuilder::new(ButtonStyle::Secondary)
    .custom_id("vc-category-clear")
    .label("Use no category")
    .disabled(current.is_none())
    .build();
  let components: Vec<Component> = vec![
    ActionRowBuilder::new().component(pick).build().into(),
    ActionRowBuilder::new().component(clear).build().into(),
  ];
  (content, components)
}

pub async fn handle_vc_settings(http: &Client, state: &State, ev: &InteractionCreate) -> Result<(), BoxErr> {
  let Some(guild) = ev.guild_id else {
    return reply_ephemeral(http, ev, "Use `/create-private-vc settings` in a **server**.".into()).await;
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "You're not an administrator!".into()).await;
  }
  let current = state.vc_categories.read().unwrap().get(&guild).copied();
  let (content, components) = settings_view(current);
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

pub async fn handle_vc_settings_component(http: &Client, state: &State, ev: &InteractionCreate, comp: &MessageComponentInteractionData) -> Result<(), BoxErr> {
  let Some(guild) = ev.guild_id else {
    return Ok(());
  };
  if !is_admin(ev) {
    return reply_ephemeral(http, ev, "You don't have administrator permissions.".into()).await;
  }

  let current: Option<Id<ChannelMarker>> = match comp.custom_id.as_str() {
    "vc-category-pick" => {
      let picked: Option<Id<ChannelMarker>> = comp
        .values
        .first()
        .and_then(|v| v.parse::<u64>().ok())
        .and_then(Id::new_checked);
      let Some(category) = picked else {
        return Ok(());
      };
      {
        let mut map = state.vc_categories.write().unwrap();
        map.insert(guild, category);
        save_vc_categories(&map);
      }
      log_event(http, state, Some(guild), format!("{} set the private VC category to <#{category}>.", who(ev))).await;
      Some(category)
    }
    "vc-category-clear" => {
      {
        let mut map = state.vc_categories.write().unwrap();
        map.remove(&guild);
        save_vc_categories(&map);
      }
      log_event(http, state, Some(guild), format!("{} reset the private VC category.", who(ev))).await;
      None
    }
    _ => return Ok(()),
  };

  let (content, components) = settings_view(current);
  let response = UpdateMessageBuilder::new().content(content).components(components).build();
  http.interaction(ev.application_id)
    .create_response(ev.id, &ev.token, &response)
    .await?;
  Ok(())
}

async fn run_private_vc(http: &Arc<Client>, state: &Arc<State>, ev: &InteractionCreate, cmd: &CommandData) -> Result<String, BoxErr> {
  let (Some(guild_id), Some(author)) = (ev.guild_id, ev.author_id()) else {
    return Ok("Use `/create-private-vc` in a server.".into());
  };
  let Some(top) = cmd.options.first() else {
    return Ok("Missing subcommand.".into());
  };

  match &top.value {
    CommandOptionValue::SubCommand(opts) if top.name == "setup" => {
      let name = opt_str(opts, "name").unwrap_or("private-vc").trim();
      let len = name.chars().count();
      if len == 0 || len > 100 {
        return Ok("The name must be 1-100 characters.".into());
      }
      let secs = match (opt_int(opts, "amount"), opt_str(opts, "unit").and_then(unit_secs)) {
        (Some(n), Some(mult)) if n >= 1 => (n as u64).checked_mul(mult),
        _ => None,
      };
      let Some(secs) = secs else {
        return Ok("Pick an amount of 1 or more and a unit".into());
      };
      if secs > 604800 {
        return Ok("That's too long. The maximum is 7 days.".into());
      }
      let owned = state.vcs.read().unwrap().values().filter(|v| v.owner == author).count();
      if owned >= 3 {
        return Ok(format!("You already own 3 private VCs. Close one first."));
      }

      let everyone: Id<GenericMarker> = guild_id.cast();
      let overwrites = [
        ChannelOverwrite {
          allow: Permissions::empty(),
          deny: Permissions::VIEW_CHANNEL | Permissions::CONNECT,
          id: everyone,
          kind: ChannelOverwriteType::Role,
        },
        ChannelOverwrite {
          allow: Permissions::VIEW_CHANNEL.union(Permissions::CONNECT).union(Permissions::SPEAK),
          deny: Permissions::empty(),
          id: author.cast(),
          kind: ChannelOverwriteType::Member,
        },
        ChannelOverwrite {
          allow: Permissions::VIEW_CHANNEL | Permissions::MANAGE_CHANNELS,
          deny: Permissions::empty(),
          id: state.bot_id.cast(),
          kind: ChannelOverwriteType::Member,
        },
      ];
      let parent = state.vc_categories.read().unwrap().get(&guild_id).copied();
      let mut created = None;
      if let Some(parent) = parent {
        match http
          .create_guild_channel(guild_id, name)
          .kind(ChannelType::GuildVoice)
          .permission_overwrites(&overwrites)
          .parent_id(parent)
          .await
        {
          Ok(resp) => created = Some(resp.model().await?),
          Err(err) => eprintln!("private vc category {parent} unusable, creating without it: {err}"),
        }
      }
      let channel = match created {
        Some(c) => c,
        None => http
          .create_guild_channel(guild_id, name)
          .kind(ChannelType::GuildVoice)
          .permission_overwrites(&overwrites)
          .await?
          .model()
          .await?,
      };
      
      state.stats.vcs.fetch_add(1, Relaxed);
      let expires_ms = now_ms() + secs * 1000;
      {
        let mut map = state.vcs.write().unwrap();
        map.insert(channel.id, PrivateVc { owner: author, expires_ms });
        save_vcs(&map);
      }
      spawn_vc_timer(Arc::clone(http), Arc::clone(state), channel.id, expires_ms);
      log_event(http, state, Some(guild_id), format!("<@{author}> created private VC <#{}>.", channel.id)).await;
      Ok(format!(
        "Created <#{}>. Only you can join. Invite people with `/create-private-vc modify users-add`. It auto-deletes <t:{}:R>.",
        channel.id,
        expires_ms / 1000
      ))
    }
    CommandOptionValue::SubCommandGroup(subs) if top.name == "modify" => {
      let Some(sub) = subs.first() else {
        return Ok("Missing subcommand.".into());
      };
      let CommandOptionValue::SubCommand(opts) = &sub.value else {
        return Ok("Missing subcommand.".into());
      };
      let Some(channel) = opt_channel(opts, "vc") else {
        return Ok("Pick a video channel.".into());
      };
      let vc = state.vcs.read().unwrap().get(&channel).copied();
      let Some(vc) = vc else {
        return Ok("That isn't a private VC I manage.".into());
      };
      if vc.owner != author {
        return Ok("Only the owner of that VC can modify it.".into());
      }

      match sub.name.as_str() {
        "users-add" => {
          let Some(target) = opt_user(opts, "user") else {
            return Ok("Pick a user.".into());
          };
          if target == author || target == state.bot_id {
            return Ok("You can't pick this user.".into());
          }
          allow_member(http, channel, target).await?;
          log_event(http, state, Some(guild_id), format!("<@{author}> invited <@{target}> to <#{channel}>.")).await;
          let dm = format!("You've been invited by <@{author}> to join the private voice channel <#{channel}>.");
          if send_dm(http, target, &dm).await.is_ok() {
            Ok(format!("Added <@{target}> to <#{channel}> and sent them a DM."))
          } else {
            Ok(format!("Added <@{target}> to <#{channel}>, but I couldn't DM them (their DMs may be closed)."))
          }
        }
        "users-transfer" => {
          let Some(target) = opt_user(opts, "user") else {
            return Ok("Pick a user.".into());
          };
          if target == author || target == state.bot_id {
            return Ok("Pick someone else.".into());
          }
          allow_member(http, channel, target).await?;
          {
            let mut map = state.vcs.write().unwrap();
            if let Some(v) = map.get_mut(&channel) {
              v.owner = target;
            }
            save_vcs(&map);
          }
          log_event(http, state, Some(guild_id), format!("<@{author}> transferred <#{channel}> to <@{target}>.")).await;
          Ok(format!("<@{target}> now owns <#{channel}>. You keep access but can no longer modify it."))
        }
        "close" => {
          log_event(http, state, Some(guild_id), format!("<@{author}> closed private VC {channel}.")).await;
          delete_vc(http, state, channel).await;
          Ok("Closed the private VC.".into())
        }
        _ => Ok("Unknown subcommand.".into()),
      }
    }
    _ => Ok("Unknown subcommand.".into()),
  }
}