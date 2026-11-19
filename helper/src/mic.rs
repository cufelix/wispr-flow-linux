//! Microphone holders (meeting auto-detect): apps with an open capture stream, from
//! `pactl -f json list source-outputs` (PipeWire-pulse and PulseAudio >= 16).

use crate::{ipc, media::run};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

static GENERATION: AtomicU64 = AtomicU64::new(0);
/// Electron drops any observation whose sequence number is not newer than the last one it saw
/// (per monitor generation), for polled snapshots and pushed changes alike: one shared counter.
static SEQ: AtomicU64 = AtomicU64::new(0);
static MONITORING: std::sync::atomic::AtomicBool = std::sync::atomic::AtomicBool::new(false);

fn ppid_of(pid: u32) -> Option<u32> {
    let stat = std::fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
    stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok() // fields after "(comm)": state, ppid
}

/// Wispr itself: our parent (Electron main), its children (the audio utility process) or us.
fn is_ours(pid: u32) -> bool {
    let (me, electron) = (std::process::id(), std::os::unix::process::parent_id());
    let mut p = pid;
    for _ in 0..16 {
        if p == me || p == electron {
            return true;
        }
        match ppid_of(p) {
            Some(pp) if pp > 1 => p = pp,
            _ => return false,
        }
    }
    false
}

/// Wispr recognizes meeting apps and browsers by macOS bundle id or Windows exe name (it appends ".exe" to a bare
/// name). Linux binaries whose name differs from the Windows one are renamed to it; the rest pass through.
pub fn wispr_app_id(binary: &str) -> &str {
    match binary.to_lowercase().as_str() {
        "chromium" | "chromium-browser" | "google-chrome" | "google-chrome-stable" => "chrome",
        "microsoft-edge" | "microsoft-edge-stable" => "msedge",
        "vivaldi-bin" => "vivaldi",
        "brave-browser" => "brave",
        "firefox-bin" | "firefox-esr" => "firefox",
        "zen-bin" => "zen",
        "teams-for-linux" => "teams",
        "whatsapp-for-linux" | "wasistlos" => "whatsapp",
        _ => binary,
    }
}

/// Pure part, testable: source-outputs JSON + monitor source indexes -> holders.
fn parse_holders(json: &str, monitor_sources: &[u64]) -> Vec<Value> {
    let Ok(Value::Array(outputs)) = serde_json::from_str::<Value>(json) else { return vec![] };
    let mut holders: Vec<Value> = Vec::new();
    for o in &outputs {
        if o["source"].as_u64().is_some_and(|s| monitor_sources.contains(&s)) {
            continue; // listening to a speaker monitor (meters, loopback), not a microphone
        }
        let props = &o["properties"];
        let s = |k: &str| props[k].as_str().unwrap_or_default();
        let pid = s("application.process.id").parse::<u32>().ok();
        let name = if s("application.name").is_empty() { s("application.process.binary") } else { s("application.name") };
        if name.is_empty() || name.to_lowercase().contains("wispr") || pid.is_some_and(is_ours) {
            continue;
        }
        let mut h = json!({ "name": name, "source": "process_api" });
        if !s("application.process.binary").is_empty() { h["bundleIdOrExePath"] = json!(wispr_app_id(s("application.process.binary"))); }
        if let Some(pid) = pid { h["processId"] = json!(pid); }
        if !holders.contains(&h) { holders.push(h); }
    }
    holders
}

fn monitor_source_indexes() -> Vec<u64> {
    let json = run("pactl", &["-f", "json", "list", "sources", "short"]).unwrap_or_default();
    let Ok(Value::Array(sources)) = serde_json::from_str::<Value>(&json) else { return vec![] };
    sources.iter()
        .filter(|s| s["name"].as_str().is_some_and(|n| n.ends_with(".monitor")))
        .filter_map(|s| s["index"].as_u64())
        .collect()
}

pub fn holders() -> Vec<Value> {
    match run("pactl", &["-f", "json", "list", "source-outputs"]) {
        Some(json) => parse_holders(&json, &monitor_source_indexes()),
        None => vec![],
    }
}

fn payload(holders: Vec<Value>, generation: u64) -> Value {
    let (seq, listening) = (SEQ.fetch_add(1, Ordering::SeqCst) + 1, MONITORING.load(Ordering::SeqCst));
    let now = SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64);
    json!({ "deviceInUse": !holders.is_empty(), "holders": holders, "detectionMethod": "process_api",
        "observedAtMs": now, "listenerActive": listening, "monitorGeneration": generation, "observationSeq": seq })
}

/// Reply body for GetMicrophoneHolders.
pub fn snapshot() -> Value {
    json!({ "MicrophoneHolders": { "payload": payload(holders(), GENERATION.load(Ordering::SeqCst)) } })
}

/// SetMicrophoneHoldersMonitor: poll while enabled, push MicrophoneHoldersChanged when the set changes.
pub fn set_monitor(enabled: bool, cadence_ms: Option<u64>) -> Value {
    let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1; // also stops any previous poller
    MONITORING.store(enabled, Ordering::SeqCst);
    if enabled {
        let cadence = Duration::from_millis(cadence_ms.unwrap_or(2000).max(1000));
        std::thread::spawn(move || {
            let mut last: Option<Vec<Value>> = None;
            while GENERATION.load(Ordering::SeqCst) == generation {
                let now = holders();
                if last.as_ref() != Some(&now) {
                    ipc::request("MicrophoneHoldersChanged", json!({ "payload": payload(now.clone(), generation) }));
                    last = Some(now);
                }
                std::thread::sleep(cadence);
            }
        });
    }
    json!({ "MicrophoneHoldersMonitorStatus": { "payload": { "subscribed": enabled, "supported": true } } })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_and_filters() {
        let j = r#"[
          {"source":60,"properties":{"application.name":"parecord","application.process.id":"1","application.process.binary":"pacat"}},
          {"source":58,"properties":{"application.name":"meter","application.process.id":"2"}},
          {"source":60,"properties":{"application.name":"Wispr Flow","application.process.id":"3"}}]"#;
        let h = parse_holders(j, &[58]);
        assert_eq!(h, vec![json!({"name":"parecord","source":"process_api","bundleIdOrExePath":"pacat","processId":1})]);
        assert!(parse_holders("garbage", &[]).is_empty());
        assert_eq!((wispr_app_id("chromium"), wispr_app_id("vivaldi-bin"), wispr_app_id("teams-for-linux")), ("chrome", "vivaldi", "teams"));
        assert_eq!(wispr_app_id("zoom"), "zoom");
    }

    #[test]
    fn observation_seq_always_increases() {
        // Electron ignores a snapshot whose seq is not above the previous one (this broke meeting detection)
        let seq = |v: Value| v["observationSeq"].as_u64().unwrap();
        let (a, b) = (seq(payload(vec![], 1)), seq(payload(vec![], 1)));
        assert!(a >= 1 && b > a);
    }
}
