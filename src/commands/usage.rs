use std::time::Duration;

use sysinfo::{Pid, ProcessesToUpdate, System};
use twilight_http::Client;
use twilight_model::{
  channel::message::MessageFlags,
  gateway::payload::incoming::InteractionCreate,
  http::interaction::{InteractionResponse, InteractionResponseType},
};
use twilight_util::builder::InteractionResponseDataBuilder;

use crate::state::{BoxErr, State};

fn fmt_bytes(b: u64) -> String {
  let mib = b as f64 / 1048576.0;
  if mib >= 1024.0 { format!("{:.2} GiB", mib / 1024.0) } else { format!("{mib:.1} MiB") }
}

fn fmt_dur(secs: u64) -> String {
  let (d, h, m, s) = (secs / 86400, secs / 3600 % 24, secs / 60 % 60, secs % 60);
  if d > 0 { format!("{d}d {h}h {m}m") } else if h > 0 { format!("{h}h {m}m {s}s") } else { format!("{m}m {s}s") }
}

pub async fn handle_usage(http: &Client, _state: &State, ev: &InteractionCreate) -> Result<(), BoxErr> {
  let iclient = http.interaction(ev.application_id);
  let ack = InteractionResponse {
    kind: InteractionResponseType::DeferredChannelMessageWithSource,
    data: Some(InteractionResponseDataBuilder::new().flags(MessageFlags::EPHEMERAL).build()),
  };
  iclient.create_response(ev.id, &ev.token, &ack).await?;

  let pid = Pid::from_u32(std::process::id());
  let mut sys = System::new().map_err(BoxErr::from)?;
  sys.refresh_cpu_usage();
  sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
  tokio::time::sleep(Duration::from_millis(500)).await;
  sys.refresh_cpu_usage();
  sys.refresh_processes(ProcessesToUpdate::Some(&[pid]), true);
  sys.refresh_memory();

  let cores = sys.cpus().len().max(1);
  let (proc_cpu, proc_ram, proc_uptime) = sys
    .process(pid)
    .map(|p| (p.cpu_usage(), p.memory(), p.run_time()))
    .unwrap_or_default();

  let text = format!(
    "**Rusty usage**\n\
    -# Bot CPU: {:.1}% of one core ({:.1}% of all {cores} cores)\n\
    -# Bot RAM: {}\n\
    -# Bot uptime: {}\n\
    -# Host CPU: {:.1}%\n\
    -# Host RAM: {} / {}\n\
    -# Host uptime: {}\n",
    proc_cpu,
    proc_cpu / cores as f32,
    fmt_bytes(proc_ram),
    fmt_dur(proc_uptime),
    sys.global_cpu_usage(),
    fmt_bytes(sys.used_memory()),
    fmt_bytes(sys.total_memory()),
    fmt_dur(System::uptime().unwrap_or_default()),
  );

  iclient.update_response(&ev.token).content(Some(&text)).await?;
  Ok(())
}