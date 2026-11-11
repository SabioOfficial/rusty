use std::{collections::HashMap};

use twilight_model::id::{
  Id,
  marker::{ChannelMarker, GuildMarker, UserMarker},
};

use crate::state::{Anchor, Disappear, PrivateVc};

pub fn load_redirects() -> HashMap<Id<GuildMarker>, Id<UserMarker>> {
  let mut map = HashMap::new();
  if let Ok(text)= std::fs::read_to_string("redirects.txt") {
    for line in text.lines() {
      let mut parts = line.split_whitespace();
      if let (Some(g), Some(u))= (parts.next(), parts.next()) {
        if let (Ok(g), Ok(u)) = (g.parse::<u64>(), u.parse::<u64>()) {
          if let (Some(g), Some(u)) = (Id::new_checked(g), Id::new_checked(u)){
            map.insert(g, u);
          }
        }
      }
    }
  }
  map
}

pub fn save_redirects(map: &HashMap<Id<GuildMarker>, Id<UserMarker>>) {
  let text: String = map
    .iter()
    .map(|(g, u)| format!("{} {}\n", g.get(), u.get()))
    .collect();
  if let Err(err)= std::fs::write("redirects.txt", text) {
    eprintln!("failed to save redirects.txt: {err}");
  }
}

pub fn load_disappear() -> HashMap<Id<ChannelMarker>, Disappear> {
  let mut map = HashMap::new();
  if let Ok(text) = std::fs::read_to_string("disappear.txt") {
    for line in text.lines() {
      let mut p = line.split_whitespace();
      if let (Some(c), Some(s), Some(t)) = (p.next(), p.next(), p.next()) {
        if let (Ok(c), Ok(secs), Ok(since_ms)) = (c.parse::<u64>(), s.parse::<u64>(), t.parse::<u64>()) {
          if let Some(c) = Id::new_checked(c) {
            map.insert(c, Disappear { secs, since_ms });
          }
        }
      }
    }
  }
  map
}

pub fn save_disappear(map: &HashMap<Id<ChannelMarker>, Disappear>) {
  let text: String = map
    .iter()
    .map(|(c, d)| format!("{} {} {}\n", c.get(), d.secs, d.since_ms))
    .collect();
  if let Err(err) = std::fs::write("disappear.txt", text) {
    eprintln!("failed to save disappear.txt: {err}");
  }
}

pub fn unescape(s: &str) -> String {
  let mut out = String::new();
  let mut it = s.chars();
  while let Some(ch) = it.next() {
    if ch == '\\' {
      match it.next() {
        Some('n') => out.push('\n'),
        Some(other) => out.push(other),
        None => {}
      }
    } else {
      out.push(ch);
    }
  }
  out
}

pub fn load_anchors() -> HashMap<Id<ChannelMarker>, Anchor> {
  let mut map = HashMap::new();
  if let Ok(text) = std::fs::read_to_string("anchors.txt") {
    for line in text.lines() {
      let mut p = line.splitn(3, ' ');
      if let (Some(c), Some(l), Some(body)) = (p.next(), p.next(), p.next()) {
        let mut f = l.split(':');
        let l = f.next().unwrap_or("0");
        let flag = f.next().unwrap_or("0");
        let quiet_ms = f
          .next()
          .and_then(|s| s.parse::<u64>().ok())
          .unwrap_or(if flag == "1" { 60000 } else { 1500 });
        let min_msgs = f.next().and_then(|s| s.parse::<u64>().ok()).unwrap_or(1);
        if let (Ok(c), Ok(l)) = (c.parse::<u64>(), l.parse::<u64>()) {
          if let Some(c) = Id::new_checked(c) {
            map.insert(c, Anchor { text: unescape(body), last: Id::new_checked(l), tick: 0, unintrusive: flag == "1", last_msg_ms: 0, quiet_ms, min_msgs, pending: 0 });
          }
        }
      }
    }
  }
  map
}

pub fn save_anchors(map: &HashMap<Id<ChannelMarker>, Anchor>) {
  let text: String = map
    .iter()
    .map(|(c, a)| {
      let body = a.text.replace('\\', "\\\\").replace('\n', "\\n");
      format!("{} {}:{}:{}:{} {}\n", c.get(), a.last.map(|m| m.get()).unwrap_or(0), a.unintrusive as u8, a.quiet_ms, a.min_msgs, body)
    })
    .collect();
  if let Err(err) = std::fs::write("anchors.txt", text) {
    eprintln!("failed to save anchors.txt: {err}");
  }
}

pub fn load_logs() -> HashMap<Id<GuildMarker>, Id<ChannelMarker>> {
  let mut map = HashMap::new();
  if let Ok(text) = std::fs::read_to_string("log_channels.txt") {
    for line in text.lines() {
      let mut p = line.split_whitespace();
      if let (Some(g), Some(c)) = (p.next(), p.next()) {
        if let (Ok(g), Ok(c)) = (g.parse::<u64>(), c.parse::<u64>()) {
          if let (Some(g), Some(c)) = (Id::new_checked(g), Id::new_checked(c)) {
            map.insert(g, c);
          }
        }
      }
    }
  }
  map
}

pub fn save_logs(map: &HashMap<Id<GuildMarker>, Id<ChannelMarker>>) {
  let text: String = map
    .iter()
    .map(|(g, c)| format!("{} {}\n", g.get(), c.get()))
    .collect();
  if let Err(err) = std::fs::write("log_channels.txt", text) {
    eprintln!("failed to save log_channels.txt: {err}");
  }
}

pub fn load_vcs() -> HashMap<Id<ChannelMarker>, PrivateVc> {
  let mut map = HashMap::new();
  if let Ok(text) = std::fs::read_to_string("private_vcs.txt") {
    for line in text.lines() {
      let mut p = line.split_whitespace();
      if let (Some(c), Some(o), Some(t)) = (p.next(), p.next(), p.next()) {
        if let (Ok(c), Ok(o), Ok(expires_ms)) = (c.parse::<u64>(), o.parse::<u64>(), t.parse::<u64>()) {
          if let (Some(c), Some(owner)) = (Id::new_checked(c), Id::new_checked(o)) {
            map.insert(c, PrivateVc { owner, expires_ms });
          }
        }
      }
    }
  }
  map
}

pub fn save_vcs(map: &HashMap<Id<ChannelMarker>, PrivateVc>) {
  let text: String = map
    .iter()
    .map(|(c, v)| format!("{} {} {}\n", c.get(), v.owner.get(), v.expires_ms))
    .collect();
  if let Err(err) = std::fs::write("private_vcs.txt", text) {
    eprintln!("failed to save private_vcs.txt: {err}");
  }
}

pub fn load_autoreplies() -> HashMap<Id<GuildMarker>, HashMap<String, String>> {
  let mut map: HashMap<Id<GuildMarker>, HashMap<String, String>> = HashMap::new();
  if let Ok(text) = std::fs::read_to_string("autoreplies.txt") {
    for line in text.lines() {
      let mut p = line.splitn(3, ' ');
      if let (Some(g), Some(name), Some(body)) = (p.next(), p.next(), p.next()) {
        if let Ok(g) = g.parse::<u64>() {
          if let Some(g) = Id::new_checked(g) {
            map.entry(g).or_default().insert(name.to_string(), unescape(body));
          }
        }
      }
    }
  }
  map
}

pub fn save_autoreplies(map: &HashMap<Id<GuildMarker>, HashMap<String, String>>) {
  let mut text = String::new();
  for (g, replies) in map {
    for (name, body) in replies {
      let body = body.replace('\\', "\\\\").replace('\r', "").replace('\n', "\\n");
      text.push_str(&format!("{} {} {}\n", g.get(), name, body));
    }
  }
  if let Err(err) = std::fs::write("autoreplies.txt", text) {
    eprintln!("failed to save autoreplies.txt: {err}");
  }
}