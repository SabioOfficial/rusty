use std::{
  cmp::Reverse,
  collections::{BinaryHeap, HashMap},
  sync::{Arc, atomic::Ordering::Relaxed},
  time::Duration,
};

use tokio::sync::mpsc;
use twilight_http::{Client, error::ErrorType};
use twilight_model::id::{
  Id,
  marker::{ChannelMarker, MessageMarker},
};

use crate::{
  state::{Disappear, Job},
  stats::Stats,
  util::{now_ms, snowflake_ms},
};

pub async fn scheduler(http: Arc<Client>, mut rx: mpsc::UnboundedReceiver<Job>, stats: Arc<Stats>) {
  let mut heap: BinaryHeap<Reverse<Job>> = BinaryHeap::new();
  loop {
    let wait = match heap.peek() {
      Some(Reverse((at, _, _))) => Duration::from_millis(at.saturating_sub(now_ms())),
      None => Duration::from_secs(3600),
    };
    tokio::select! {
      job = rx.recv() => match job {
        Some(job) => heap.push(Reverse(job)),
        None => return,
      },
      _ = tokio::time::sleep(wait) => {
        let now = now_ms();
        let mut due = Vec::new();
        while let Some(Reverse((at, _, _))) = heap.peek() {
          if *at > now {
            break;
          }
          let Reverse((_, c, m)) = heap.pop().unwrap();
          due.push((c, m));
        }
        if !due.is_empty() {
          tokio::spawn(delete_all(Arc::clone(&http), Arc::clone(&stats), due));
        }
      }
    }
  }
}

pub async fn delete_all(http: Arc<Client>, stats: Arc<Stats>, due: Vec<(Id<ChannelMarker>, Id<MessageMarker>)>) {
  let cutoff = now_ms().saturating_sub(1123200000);
  let mut by_channel: HashMap<Id<ChannelMarker>, Vec<Id<MessageMarker>>> = HashMap::new();
  for (c, m) in due {
    by_channel.entry(c).or_default().push(m);
  }
  for (channel, ids) in by_channel {
    let (fresh, old): (Vec<_>, Vec<_>) = ids
      .into_iter()
      .partition(|m| snowflake_ms(m.get()) > cutoff);
    for chunk in fresh.chunks(100) {
      if chunk.len() > 1 && http.delete_messages(channel, chunk).await.is_ok() {
        stats.deleted.fetch_add(chunk.len() as u64, Relaxed);
        continue;
      }
      delete_each(&http, &stats, channel, chunk).await;
    }
    delete_each(&http, &stats, channel, &old).await;
  }
}

pub async fn delete_each(http: &Client, stats: &Stats, channel: Id<ChannelMarker>, ids: &[Id<MessageMarker>]) {
  for &message in ids {
    match http.delete_message(channel, message).await {
      Ok(_) => {
        stats.deleted.fetch_add(1, Relaxed);
      }
      Err(err) => {
        let gone = matches!(err.kind(), ErrorType::Response { status, .. } if status.get() == 404);
        if !gone {
          eprintln!("delete failed: {err}");
        }
      }
    }
  }
}

pub async fn catch_up(http: Arc<Client>, tx: mpsc::UnboundedSender<Job>, channel: Id<ChannelMarker>, cfg: Disappear) {
  let mut before: Option<Id<MessageMarker>> = None;
  loop {
    let resp = match before {
      Some(b) => http.channel_messages(channel).before(b).limit(100).await,
      None => http.channel_messages(channel).limit(100).await,
    };
    let msgs = match resp {
      Ok(resp) => match resp.models().await {
        Ok(m) => m,
        Err(err) => {
          eprintln!("catch-up decode error in {channel}: {err}");
          return;
        }
      },
      Err(err) => {
        eprintln!("catch-up fetch error in {channel}: {err}");
        return;
      }
    };
    if msgs.is_empty() {
      return;
    }
    before = msgs.last().map(|m| m.id);

    let mut reached_setup = false;
    for m in &msgs {
      let created = snowflake_ms(m.id.get());
      if created < cfg.since_ms {
        reached_setup = true;
        continue;
      }
      if m.pinned {
        continue;
      }
      let _ = tx.send((created + cfg.secs * 1000, channel, m.id));
    }
    if reached_setup || msgs.len() < 100 {
      return;
    }
  }
}