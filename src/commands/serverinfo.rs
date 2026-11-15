use twilight_http::Client;
use twilight_model::{
  channel::{ChannelType, message::AllowedMentions},
  gateway::payload::incoming::InteractionCreate,
  guild::PremiumTier,
  http::interaction::{InteractionResponse, InteractionResponseType},
  id::{Id, marker::GuildMarker},
};

use crate::{
  state::BoxErr,
  util::{reply_ephemeral, snowflake_ms},
};

async fn build_info(http: &Client, guild_id: Id<GuildMarker>) -> Result<String, BoxErr> {
  let guild = http.guild(guild_id).with_counts(true).await?.model().await?;
  let channels = http.guild_channels(guild_id).await?.models().await?;

  let (mut text, mut voice, mut categories) = (0, 0, 0);
  for c in &channels {
    match c.kind {
      ChannelType::GuildText | ChannelType::GuildAnnouncement | ChannelType::GuildForum => text += 1, ChannelType::GuildVoice | ChannelType::GuildStageVoice => voice += 1, ChannelType::GuildCategory => categories += 1,
      _ => {}
    }
  }

  let members = guild
    .approximate_member_count
    .or(guild.member_count)
    .map(|n| n.to_string())
    .unwrap_or_else(|| "unknown".to_string());
  let online = guild.approximate_presence_count.map(|n| format!(" ({n} online)")).unwrap_or_default();
  let created = snowflake_ms(guild.id.get()) / 1000;
  let tier = match guild.premium_tier {
    PremiumTier::None => "0".to_string(),
    PremiumTier::Tier1 => "1".to_string(),
    PremiumTier::Tier2 => "2".to_string(),
    PremiumTier::Tier3 => "3".to_string(),
    PremiumTier::Other(n) => n.to_string(),
    _ => "0".to_string(),
  };
  let desc = guild.description.as_deref().map(|d| format!("{d}\n")).unwrap_or_default();
  let icon = guild
    .icon
    .map(|h| format!("\n[Server icon](https://cdn.discordapp.com/icons/{}/{}.png?size=512)", guild.id, h))
    .unwrap_or_default();

  Ok(format!(
    "## {}\n\
    {desc}\
    **Owner:** <@{}>\n\
    **Created:** <t:{created}:D> (<t:{created}:R>)\n\
    **Members:** {members}{online}\n\
    **channels:** {text} text, {voice} voice, {categories} categories\n\
    **Roles:** {} / **Emojis:** {}\n\
    **Boosts:** {} (level {tier}){icon}\n\
    -# ID `{}`",
    guild.name,
    guild.owner_id,
    guild.roles.len(),
    guild.emojis.len(),
    guild.premium_subscription_count.unwrap_or(0),
    guild.id,
  ))
}

pub async fn handle_serverinfo(http: &Client, ev: &InteractionCreate) -> Result<(), BoxErr> {
  let Some(guild_id) = ev.guild_id else {
    return reply_ephemeral(http, ev, "Use `/serverinfo` in a server.".into()).await;
  };
  let iclient = http.interaction(ev.application_id);
  let ack = InteractionResponse {
    kind: InteractionResponseType::DeferredChannelMessageWithSource,
    data: None,
  };
  iclient.create_response(ev.id, &ev.token, &ack).await?;

  let text = match build_info(http, guild_id).await {
    Ok(t) => t,
    Err(err) => {
      eprintln!("serverinfo failed: {err}");
      "I couldn't load the server info right now.".to_string()
    }
  };
  iclient
    .update_response(&ev.token)
    .content(Some(&text))
    .allowed_mentions(Some(&AllowedMentions::default()))
    .await?;
  Ok(())
}