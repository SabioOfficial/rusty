use std::{
  sync::{
    Arc,
    atomic::{AtomicU64, Ordering::Relaxed},
  },
  time::Duration,
};

use twilight_http::Client;

use crate::{
  state::State,
  util::{log_event, now_ms},
};

pub struct Stats {
  pub day: AtomicU64,
  pub deleted: AtomicU64,
  pub anchors: AtomicU64,
  pub booms: AtomicU64,
  pub vcs: AtomicU64,
}

pub fn load_stats() -> Stats {
  let today = now_ms() / 86400000;
  let mut v = [today, 0, 0, 0, 0];
  if let Ok(text) = std::fs::read_to_string("stats.txt") {
    let parsed: Vec<u64> = text.split_whitespace().filter_map(|s| s.parse().ok()).collect();
    if parsed.len() == 5 {
      v.copy_from_slice(&parsed);
    }
  }
  Stats {
    day: AtomicU64::new(v[0]),
    deleted: AtomicU64::new(v[1]),
    anchors: AtomicU64::new(v[2]),
    booms: AtomicU64::new(v[3]),
    vcs: AtomicU64::new(v[4]),
  }
}

fn save_stats(s: &Stats) {
  let text = format!(
    "{} {} {} {} {}\n",
    s.day.load(Relaxed),
    s.deleted.load(Relaxed),
    s.anchors.load(Relaxed),
    s.booms.load(Relaxed),
    s.vcs.load(Relaxed),
  );
  if let Err(err) = std::fs::write("stats.txt", text) {
    eprintln!("failed to save stats.txt: {err}");
  }
}

pub async fn daily_summary(http: Arc<Client>, state: Arc<State>) {
  let mut tick = tokio::time::interval(Duration::from_secs(60));
  loop {
    tick.tick().await;
    let s = &state.stats;
    let today = now_ms() / 86400000;
    let day = s.day.load(Relaxed);
    if today != day {
      let deleted = s.deleted.swap(0, Relaxed);
      let anchors = s.anchors.swap(0, Relaxed);
      let booms = s.booms.swap(0, Relaxed);
      let vcs = s.vcs.swap(0, Relaxed);
      s.day.store(today, Relaxed);
      let text = format!(
        "## Daily Summary\n\
        **Messages auto-deleted:** {deleted}\n\
        **Anchor messages sent:** {anchors}\n\
        **/boom commands run:** {booms}\n\
        **Private VCs created:** {vcs}\n\
        -# Stats counted for all servers with this bot // <t:{}:D> (UTC)",
        day * 86400
      );
      let guilds: Vec<_> = state.logs.read().unwrap().keys().copied().collect();
      for guild in guilds {
        log_event(&http, &state, Some(guild), text.clone()).await;
      }
    }
    save_stats(s);
  }
}