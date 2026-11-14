use twilight_model::{
  application::command::{Command, CommandType},
  channel::ChannelType,
  guild::Permissions,
};
use twilight_util::builder::command::{
  BooleanBuilder, ChannelBuilder, CommandBuilder, IntegerBuilder, StringBuilder, SubCommandBuilder, SubCommandGroupBuilder, UserBuilder,
};

pub fn definitions() -> Vec<Command> {
  let boom = CommandBuilder::new("boom", "Boom!", CommandType::ChatInput).build();
  let redirect = CommandBuilder::new(
    "person-redirect",
    "Redirect pings of this bot to a person",
    CommandType::ChatInput,
  )
  .default_member_permissions(Permissions::MANAGE_GUILD)
  .option(
    SubCommandBuilder::new("setup", "Ping this person whenever this bot is pinged")
      .option(UserBuilder::new("user", "Who should be pinged").required(true)), 
  )
  .build();
  let disappearing = CommandBuilder::new(
    "disappearing",
    "Auto-delete messages in this channel",
    CommandType::ChatInput,
  )
  .default_member_permissions(Permissions::MANAGE_MESSAGES)
  .option(
    SubCommandBuilder::new("setup", "Delete new messages in this channel after a delay")
      .option(IntegerBuilder::new("amount", "How many units").required(true).min_value(1))
      .option(
        StringBuilder::new("unit", "Unit of time")
          .required(true)
          .choices([
            ("Seconds", "seconds"),
            ("Minutes", "minutes"),
            ("Hours", "hours"),
            ("Days", "days"),
            ("Weeks", "weeks"),
          ]),
      ),
  )
  .option(SubCommandBuilder::new("off", "Stop deleting messages in this channel"))
  .build();
  let vc_opt = || {
    ChannelBuilder::new("vc", "The private voice channel")
      .channel_types([ChannelType::GuildVoice])
      .required(true)
  };
  let private_vc = CommandBuilder::new(
    "create-private-vc",
    "Create and manage temporary private voice channels",
    CommandType::ChatInput,
  )
  .option(
    SubCommandBuilder::new("setup", "Create a private voice channel")
      .option(StringBuilder::new("name", "Channel name").required(true).max_length(100))
      .option(IntegerBuilder::new("amount", "How many units until it auto-deletes").required(true).min_value(1))
      .option(
        StringBuilder::new("unit", "Unit of time")
          .required(true)
          .choices([("Minutes", "minutes"), ("Hours", "hours"), ("Days", "days")]), 
      ),
  )
  .option(
    SubCommandGroupBuilder::new("modify", "Modify one of your private voice channels").subcommands([
      SubCommandBuilder::new("users-add", "Invite a user (they get a DM)")
        .option(vc_opt())
        .option(UserBuilder::new("user", "Who to invite").required(true)),
      SubCommandBuilder::new("users-transfer", "Transfer ownership to another user")
        .option(vc_opt())
        .option(UserBuilder::new("user", "New owner").required(true)),
      SubCommandBuilder::new("close", "Delete the voice channel now").option(vc_opt()),
    ]),
  )
  .option(SubCommandBuilder::new("settings", "Change private VC settings for this server"))
  .build();
  let anchor = CommandBuilder::new(
    "anchor",
    "Keep a message at the bottom of the channel",
    CommandType::ChatInput,
  )
  .default_member_permissions(Permissions::MANAGE_MESSAGES)
  .option(SubCommandBuilder::new("setup", "Anchor a message to this channel"))
  .option(SubCommandBuilder::new("off", "Remove the anchored message from this channel"))
  .option(SubCommandBuilder::new("settings", "Change anchor settings for this channel"))
  .build();
  let log_channel = CommandBuilder::new(
    "log-channel",
    "Choose where the bot logs its actions",
  CommandType::ChatInput,
  )
  .default_member_permissions(Permissions::MANAGE_GUILD)
  .option(
    SubCommandBuilder::new("setup", "Send bot logs to a channel").option(
      ChannelBuilder::new("channel", "Where logs should go")
        .channel_types([ChannelType::GuildText])
        .required(true),
    ),
  )
  .option(SubCommandBuilder::new("off", "Stop logging"))
  .build();
  let usage = CommandBuilder::new("usage", "Show the bot's CPU and RAM usage.", CommandType::ChatInput).build();
  let autoreply = CommandBuilder::new(
    "autoreply",
    "Manage custom !!name auto-replies",
    CommandType::ChatInput,
  )
  .default_member_permissions(Permissions::ADMINISTRATOR)
  .build();
  let remindme = CommandBuilder::new("remindme", "Set a reminder", CommandType::ChatInput)
    .option(StringBuilder::new("text", "What to remind you about").required(true).max_length(500))
    .option(
      StringBuilder::new("when", "e.g. 30m, 2h 15m, 18:30, 6:30pm, tomorrow 9am, 2026-11-12 18:26")
        .required(true)
        .max_length(60),
    )
    .option(BooleanBuilder::new("ping", "Whether I should ping you when the reminder goes off (default: yes)"))
    .option(StringBuilder::new("timezone", "Your timezone, e.g. Europe/London or UTC+2. This saves for your account.").max_length(40))
    .build();

  vec![boom, redirect, disappearing, private_vc, anchor, log_channel, usage, autoreply, remindme]
}