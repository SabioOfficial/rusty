use std::{collections::HashMap, error::Error, sync::{Arc, RwLock}};

use tokio::sync::mpsc;
use twilight_model::{id::{
  Id,
  marker::{ChannelMarker, GuildMarker, MessageMarker, UserMarker},
}};

use crate::stats::Stats;

pub type BoxErr = Box<dyn Error + Send + Sync>;

pub type Job = (u64, Id<ChannelMarker>, Id<MessageMarker>);

#[derive(Clone, Copy)]
pub struct PrivateVc {
  pub owner: Id<UserMarker>,
  pub expires_ms: u64,
}

#[derive(Clone, Copy)]
pub struct Disappear {
  pub secs: u64,
  pub since_ms: u64,
}

#[derive(Clone)]
pub struct Anchor {
  pub text: String,
  pub last: Option<Id<MessageMarker>>,
  pub tick: u64,
  pub unintrusive: bool,
  pub last_msg_ms: u64,
  pub quiet_ms: u64,
  pub min_msgs: u64,
  pub pending: u64,
}

pub struct State {
  pub redirects: RwLock<HashMap<Id<GuildMarker>, Id<UserMarker>>>,
  pub disappear: RwLock<HashMap<Id<ChannelMarker>, Disappear>>,
  pub vcs: RwLock<HashMap<Id<ChannelMarker>, PrivateVc>>,
  pub anchors: RwLock<HashMap<Id<ChannelMarker>, Anchor>>,
  pub logs: RwLock<HashMap<Id<GuildMarker>, Id<ChannelMarker>>>,
  pub autoreplies: RwLock<HashMap<Id<GuildMarker>, HashMap<String, String>>>,
  pub tx: mpsc::UnboundedSender<Job>,
  pub bot_id: Id<UserMarker>,
  pub stats: Arc<Stats>,
}