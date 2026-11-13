use chrono::{DateTime, Duration, FixedOffset, NaiveDate, NaiveTime, TimeZone, Utc, offset::LocalResult};
use chrono_tz::{TZ_VARIANTS, Tz};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Zone {
  Named(Tz),
  Fixed(FixedOffset),
}

#[derive(Debug, PartialEq, Eq)]
pub enum WhenError {
  Unreadable,
  NeedsZone,
  Past,
  TooFar,
  NoSuchTime,
}

impl WhenError {
  pub fn message(&self) -> &'static str {
    match self {
      WhenError::Unreadable => {
        "Invalid time syntax. Examples of correct syntax: `30m`, `2h 15m`, `18:30`, `6:30pm`, `tomorrow 9am` or `2026-11-12 18:26`."
      }
      WhenError::NeedsZone => {
        "You must provide a valid timezone. Examples of valid timezones: `Europe/London`, `UTC+2`. This will be saved. Use [this](https://www.google.com/search?q=What's+my+timezone) to view your timezone."
      }
      WhenError::Past => "Invalid time. This time has already passed.",
      WhenError::TooFar => "Invalid time. The maximum is 1 year.",
      WhenError::NoSuchTime => "That clock time doesn't exist in your timezone (it falls in a clock change). Try a minute earlier or later.",
    }
  }
}

impl Zone {
  pub fn label(&self) -> String {
    match self {
      Zone::Named(tz) => tz.name().to_string(),
      Zone::Fixed(off) => {
        let secs = off.local_minus_utc();
        let sign = if secs < 0 { '-' } else { '+' };
        let a = secs.abs();
        format!("UTC{sign}{:02}:{:02}", a / 3600, a / 60 % 60)
      }
    }
  }
}

pub fn parse_zone(input: &str) -> Option<Zone> {
  let t = input.trim();
  if t.is_empty() {
    return None;
  }
  let up = t.to_ascii_uppercase();
  if up == "UTC" || up == "Z" {
    return Some(Zone::Named(Tz::UTC));
  }
  let rest = up.strip_prefix("UTC").or_else(|| up.strip_prefix("GMT")).unwrap_or(&up);
  if rest.starts_with('+') || rest.starts_with('-') {
    return parse_offset(rest).map(Zone::Fixed);
  }
  TZ_VARIANTS.iter().find(|z| z.name().eq_ignore_ascii_case(t)).map(|z| Zone::Named(*z))
}

fn parse_offset(s: &str) -> Option<FixedOffset> {
  let sign = if s.starts_with('-') { -1 } else { 1 };
  let body = &s[1..];
  let (h, m): (i32, i32) = if let Some((h, m)) = body.split_once(':') {
    (h.parse().ok()?, m.parse().ok()?)
  } else if body.len() <= 2 {
    (body.parse().ok()?, 0)
  } else if body.len() <= 4 && body.chars().all(|c| c.is_ascii_digit()) {
    let (h, m) = body.split_at(body.len() - 2);
    (h.parse().ok()?, m.parse().ok()?)
  } else {
    return None;
  };
  if !(0..=14).contains(&h) || !(0..60).contains(&m) {
    return None;
  }
  FixedOffset::east_opt(sign * (h * 3600 + m * 60))
}

fn parse_relative(s: &str) -> Option<u64> {
  let s = s.strip_prefix("in ").unwrap_or(s);
  let chars: Vec<char> = s.chars().collect();
  let (mut i, mut total, mut any) = (0, 0u64, false);
  while i < chars.len() {
    let c = chars[i];
    if c.is_whitespace() || c == ',' {
      i += 1;
    } else if c.is_ascii_digit() {
      let start = i;
      while i < chars.len() && (chars[i].is_ascii_digit() || chars[i] == '.') {
        i += 1;
      }
      let num: f64 = chars[start..i].iter().collect::<String>().parse().ok()?;
      while i < chars.len() && chars[i].is_whitespace() {
        i += 1;
      }
      let ustart = i;
      while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
      }
      let unit: String = chars[ustart..i].iter().collect();
      let secs: u64 = match unit.as_str() {
        "s" | "sec" | "secs" | "second" | "seconds" => 1,
        "m" | "min" | "mins" | "minute" | "minutes" => 60,
        "h" | "hr" | "hrs" | "hour" | "hours" => 3600,
        "d" | "day" | "days" => 86400,
        "w" | "wk" | "wks" | "week" | "weeks" => 604800,
        _ => return None,
      };
      total = total.saturating_add((num * secs as f64 * 1000.0).round() as u64);
      any = true;
    } else if c.is_ascii_alphabetic() {
      let start = i;
      while i < chars.len() && chars[i].is_ascii_alphabetic() {
        i += 1;
      }
      if chars[start..i].iter().collect::<String>() != "and" {
        return None;
      }
    } else {
      return None;
    }
  }
  any.then_some(total)
}

fn parse_clock(s: &str) -> Option<NaiveTime> {
  let s: String = s.chars().filter(|c| !c.is_whitespace()).collect();
  let (body, meridiem) = if let Some(b) = s.strip_suffix("am") {
    (b, Some(false))
  } else if let Some(b) = s.strip_prefix("pm") {
    (b, Some(true))
  } else {
    (s.as_str(), None)
  };
  let (h, m): (u32, u32) = match body.split_once(':') {
    Some((h, m)) => (h.parse().ok()?, m.parse().ok()?),
    None if meridiem.is_some() => (body.parse().ok()?, 0),
    None => return None,
  };
  let h = match meridiem {
    Some(pm) => {
      if !(1..=12).contains(&h) {
        return None;
      }
      match (pm, h) {
        (false, 12) => 0,
        (true, 12) => 12,
        (true, h) => h + 12,
        (false, h) => h,
      }
    }
    None => h,
  };
  NaiveTime::from_hms_opt(h, m, 0)
}

enum DateSpec {
  Today,
  Tomorrow,
  On(NaiveDate),
}

fn resolve<T: TimeZone>(tz: &T, now: DateTime<Utc>, date: Option<DateSpec>, time: NaiveTime) -> Result<DateTime<Utc>, WhenError> {
  let local_now = now.with_timezone(tz).naive_local();
  let today = local_now.date();
  let day = match date {
    None => {
      if today.and_time(time) <= local_now { today + Duration::days(1) } else { today }
    }
    Some(DateSpec::Today) => today,
    Some(DateSpec::Tomorrow) => today + Duration::days(1),
    Some(DateSpec::On(d)) => d,
  };
  match tz.from_local_datetime(&day.and_time(time)) {
    LocalResult::Single(dt) | LocalResult::Ambiguous(dt, _) => Ok(dt.with_timezone(&Utc)),
    LocalResult::None => Err(WhenError::NoSuchTime),
  }
}

fn parse_absolute(s: &str, now: DateTime<Utc>, zone: Option<&Zone>) -> Result<DateTime<Utc>, WhenError> {
  let zone = zone.ok_or(WhenError::NeedsZone)?;
  let mut parts = s.split_whitespace();
  let first = parts.next().ok_or(WhenError::Unreadable)?;
  let (date, time_str) = match first {
    "today" => (Some(DateSpec::Today), parts.collect::<Vec<_>>().join(" ")),
    "tomorrow" => (Some(DateSpec::Tomorrow), parts.collect::<Vec<_>>().join(" ")),
    _ => match NaiveDate::parse_from_str(first, "%Y-%m-%d") {
      Ok(d) => (Some(DateSpec::On(d)), parts.collect::<Vec<_>>().join(" ")),
      Err(_) => (None, s.to_string()),
    },
  };
  let time = parse_clock(&time_str).ok_or(WhenError::Unreadable)?;
  match zone {
    Zone::Named(tz) => resolve(tz, now, date, time),
    Zone::Fixed(off) => resolve(off, now, date, time),
  }
}

pub fn parse_when(input: &str, now_ms: u64, zone: Option<&Zone>) -> Result<u64, WhenError> {
  let s = input.trim().to_lowercase();
  let at_ms = if let Some(delta) = parse_relative(&s) {
    if delta == 0 {
      return Err(WhenError::Unreadable);
    }
    now_ms.saturating_add(delta)
  } else {
    let now = DateTime::<Utc>::from_timestamp_millis(now_ms as i64).ok_or(WhenError::Unreadable)?;
    parse_absolute(&s, now, zone)?.timestamp_millis().max(0) as u64
  };
  if at_ms <= now_ms {
    return Err(WhenError::Past);
  }
  if at_ms - now_ms > 31622400000 {
    return Err(WhenError::TooFar);
  }
  Ok(at_ms)
}

#[cfg(test)]
mod tests {
  use super::*;

  fn ms(y: i32, mo: u32, d: u32, h: u32, mi: u32) -> u64 {
    Utc.with_ymd_and_hms(y, mo, d, h, mi, 0).unwrap().timestamp_millis() as u64
  }
  fn z(s: &str) -> Zone {
    parse_zone(s).unwrap()
  }

  #[test]
  fn relative() {
    assert_eq!(parse_when("30m", 1794398400000, None), Ok(1794400200000));
    assert_eq!(parse_when("2h 15m", 1794398400000, None), Ok(1794406500000));
    assert_eq!(parse_when("in 1 hour and 30 minutes", 1794398400000, None), Ok(1794403800000));
    assert_eq!(parse_when("1.5h", 1794398400000, None), Ok(1794403800000));
    assert_eq!(parse_when("0m", 1794398400000, None), Err(WhenError::Unreadable));
    assert_eq!(parse_when("500d", 1794398400000, None), Err(WhenError::TooFar));
  }

  #[test]
  fn clock_in_zone() {
    assert_eq!(parse_when("18:30", 1794398400000, None), Err(WhenError::NeedsZone));
    assert_eq!(parse_when("18:30", 1794398400000, Some(&z("Europe/London"))), Ok(ms(2026, 11, 11, 18, 30)));
    assert_eq!(parse_when("09:00", 1794398400000, Some(&z("Europe/London"))), Ok(ms(2026, 11, 12, 9, 0)));
    assert_eq!(parse_when("6:30pm", 1794398400000, Some(&z("america/new_york"))), Ok(ms(2026, 11, 11, 23, 30)));
    assert_eq!(parse_when("tomorrow 9am", 1794398400000, Some(&z("UTC+2"))), Ok(ms(2026, 11, 12, 7, 0)));
    assert_eq!(parse_when("2026-11-12 18:26", 1794398400000, Some(&z("UTC"))), Ok(ms(2026, 11, 12, 18, 26)));
    assert_eq!(parse_when("today 9am", 1794398400000, Some(&z("UTC"))), Err(WhenError::Past));
  }

  #[test]
  fn dst() {
    assert_eq!(parse_when("2027-03-28 01:30", 1794398400000, Some(&z("Europe/London"))), Err(WhenError::NoSuchTime));
    assert_eq!(parse_when("2027-06-01 12:00", 1794398400000, Some(&z("Europe/London"))), Ok(ms(2027, 6, 1, 11, 0)));
  }

  #[test]
  fn zones() {
    assert_eq!(parse_zone("UTC+5:30").unwrap().label(), "UTC+05:30");
    assert_eq!(parse_zone("europe/london").unwrap().label(), "Europe/London");
    assert_eq!(parse_zone("UTC+15"), None);
    assert_eq!(parse_zone(&parse_zone("UTC-03:30").unwrap().label()), parse_zone("UTC-03:30"));
  }
}