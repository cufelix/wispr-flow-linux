//! Dictation sounds plus "pause/mute media while dictating" (prefs shouldPauseAudio / shouldMuteAudio).
//! D-Bus and process work runs on one worker thread so the stdin loop never blocks.

use crate::ipc;
use serde_json::{json, Value};
use std::process::Command;
use std::sync::mpsc::{channel, Sender};
use std::sync::{Mutex, OnceLock};
use std::time::Duration;
use zbus::blocking::{Connection, Proxy};

enum Cmd { Start, Stop }

/// What dictation start changed, so stop restores exactly that.
#[derive(Default)]
struct Saved { players: Vec<String>, muted: bool }

static SAVED: Mutex<Option<Saved>> = Mutex::new(None);
static WORKER: OnceLock<Sender<Cmd>> = OnceLock::new();

const MPRIS_PREFIX: &str = "org.mpris.MediaPlayer2.";

fn worker() -> &'static Sender<Cmd> {
    WORKER.get_or_init(|| {
        let (tx, rx) = channel();
        std::thread::spawn(move || {
            for cmd in rx {
                match cmd {
                    Cmd::Start => start(),
                    Cmd::Stop => {
                        restore();
                        ipc::request("PlayDictationStopSound", json!(true)); // after unmute so it is audible
                    }
                }
            }
        });
        tx
    })
}

/// Electron only plays the sounds (the hub checks enableSounds itself) when the helper pushes these.
pub fn dictation_start() {
    ipc::request("PlayDictationStartSound", json!(true));
    let _ = worker().send(Cmd::Start);
}

pub fn dictation_stop() {
    let _ = worker().send(Cmd::Stop);
}

/// Synchronous restore for helper exit.
pub fn shutdown() {
    restore();
}

fn user_prefs() -> Value {
    let base = std::env::var_os("XDG_CONFIG_HOME")
        .map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::Path::new(&h).join(".config")));
    base.and_then(|b| std::fs::read_to_string(b.join("Wispr Flow/config.json")).ok())
        .and_then(|s| serde_json::from_str::<Value>(&s).ok())
        .map(|v| v["prefs"]["user"].clone())
        .unwrap_or(Value::Null)
}

fn start() {
    restore(); // a Start without a Stop must not lose what the previous one changed
    let prefs = user_prefs();
    let (pause, mute) = (
        prefs["shouldPauseAudio"].as_bool() == Some(true),
        prefs["shouldMuteAudio"].as_bool() == Some(true),
    );
    let mut saved = Saved::default();
    // Media state feeds hub nudges / contextual-nudge suppression, so report it regardless of the toggles.
    let playing = playing_players();
    ipc::request("IsMediaPlayingUpdate", json!({ "payload": {
        "isMediaPlaying": !playing.is_empty(), "isMediaPausable": !playing.is_empty(), "isKnownMediaPlaying": true } }));
    if pause {
        saved.players = pause_players(&playing);
    } else if mute {
        std::thread::sleep(Duration::from_millis(300)); // let the start sound finish before silencing the sink
        saved.muted = sink_muted() == Some(false) && set_sink_mute(true);
    }
    *SAVED.lock().unwrap() = Some(saved);
}

fn restore() {
    let Some(saved) = SAVED.lock().unwrap().take() else { return };
    if saved.muted {
        set_sink_mute(false);
    }
    if saved.players.is_empty() {
        return;
    }
    if let Ok(conn) = Connection::session() {
        for name in &saved.players {
            if let Some(p) = player(&conn, name) {
                let _ = p.call_method("Play", &());
            }
        }
    }
}

fn player<'a>(conn: &'a Connection, name: &str) -> Option<Proxy<'a>> {
    Proxy::new(conn, name.to_owned(), "/org/mpris/MediaPlayer2", "org.mpris.MediaPlayer2.Player").ok()
}

/// MPRIS bus names whose PlaybackStatus is "Playing". WISPR_MPRIS_FILTER (substring) narrows it for tests.
fn playing_players() -> Vec<String> {
    let Ok(conn) = Connection::session() else { return vec![] };
    let Ok(dbus) = zbus::blocking::fdo::DBusProxy::new(&conn) else { return vec![] };
    let filter = std::env::var("WISPR_MPRIS_FILTER").ok();
    dbus.list_names()
        .unwrap_or_default()
        .into_iter()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with(MPRIS_PREFIX) && filter.as_ref().is_none_or(|f| n.contains(f.as_str())))
        .filter(|n| {
            player(&conn, n)
                .and_then(|p| p.get_property::<String>("PlaybackStatus").ok())
                .is_some_and(|s| s == "Playing")
        })
        .collect()
}

fn pause_players(names: &[String]) -> Vec<String> {
    let Ok(conn) = Connection::session() else { return vec![] };
    names.iter().filter(|n| player(&conn, n).is_some_and(|p| p.call_method("Pause", &()).is_ok())).cloned().collect()
}

// ponytail: Command without timeout; wpctl/pactl return in ms unless the audio server is wedged.
pub fn run(cmd: &str, args: &[&str]) -> Option<String> {
    let o = Command::new(cmd).args(args).output().ok()?;
    o.status.success().then(|| String::from_utf8_lossy(&o.stdout).into_owned())
}

fn sink_muted() -> Option<bool> {
    if let Some(s) = run("wpctl", &["get-volume", "@DEFAULT_AUDIO_SINK@"]) {
        return Some(s.contains("[MUTED]"));
    }
    run("pactl", &["get-sink-mute", "@DEFAULT_SINK@"]).map(|s| s.contains("yes"))
}

fn set_sink_mute(mute: bool) -> bool {
    let v = if mute { "1" } else { "0" };
    run("wpctl", &["set-mute", "@DEFAULT_AUDIO_SINK@", v]).is_some()
        || run("pactl", &["set-sink-mute", "@DEFAULT_SINK@", v]).is_some()
}
