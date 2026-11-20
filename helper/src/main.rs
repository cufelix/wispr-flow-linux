//! Linux replacement for Wispr Flow's native macOS/Windows helper.
//! Electron spawns it and talks over stdin/stdout (see ipc.rs). It reports every key press
//! (Electron matches shortcuts itself), pastes dictated text and reports the focused app.

mod a11y;
mod apps;
mod ble;
mod cursor;
mod input;
mod ipc;
mod keys;
mod layout;
mod media;
mod mic;
mod output;
mod slack;

use serde_json::{json, Value};
use std::io::Read;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{channel, Sender};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

fn log(msg: &str) {
    eprintln!("[linux-helper] {msg}"); // Electron forwards helper stderr into its log
}

fn main() {
    if std::env::args().nth(1).as_deref() == Some("--cursor") {
        if let Err(e) = cursor::run() { log(&format!("cursor tracker stopped: {e:#}")); }
        return;
    }
    ipc::init();
    let started = Instant::now();
    let wayland = std::env::var("XDG_SESSION_TYPE").is_ok_and(|s| s == "wayland")
        || (std::env::var_os("WAYLAND_DISPLAY").is_some() && std::env::var_os("DISPLAY").is_none());

    let out = match output::Output::new(wayland) {
        Ok(o) => Some(Arc::new(Mutex::new(o))),
        Err(e) => { log(&format!("text insertion unavailable: {e:#}")); None }
    };

    let index = Arc::new(AtomicU64::new(0));
    let on_key: input::OnKey = {
        let index = index.clone();
        Arc::new(move |code, pressed| {
            let (key, input_type) = match (keys::evdev_to_vk(code), keys::evdev_mouse_button(code)) {
                (Some(vk), _) => (vk, "keyboard"),
                (None, Some(btn)) => (btn, "mouse"),
                _ => return,
            };
            let key = if input_type == "keyboard" {
                // a release reports the code its press did, even if the layout changed in between
                let mut held = KEYS_HELD.lock().unwrap();
                if pressed {
                    let vk = if wayland { key } else { layout::vk(code, key) };
                    held.insert(code, vk);
                    vk
                } else {
                    held.remove(&code).unwrap_or(key)
                }
            } else {
                key
            };
            let prefix = if input_type == "mouse" { "mouse_event" } else { "key_event" };
            let event = format!("{prefix}_{}", if pressed { "press" } else { "release" });
            let i = index.fetch_add(1, Ordering::SeqCst) + 1;
            ipc::request("KeypressEvent", json!({ "payload": {
                "eventType": event, "index": i, "inputType": input_type, "key": key } }));
        })
    };
    if !wayland {
        layout::init();
    }
    match input::start(on_key, wayland) {
        Some(backend) => log(&format!("key input via {backend}")),
        None => log("no key input: add yourself to the 'input' group (see README)"),
    }

    a11y::start();
    if let Some(out) = &out {
        watch_active_app(out.clone());
    }
    let paster = spawn_paster(out.clone());

    let mut stdin = std::io::stdin();
    let mut buf = String::new();
    let mut chunk = [0u8; 65536];
    let mut pending = Vec::new();
    loop {
        let n = match stdin.read(&mut chunk) {
            Ok(0) | Err(_) => { media::shutdown(); return } // Electron went away
            Ok(n) => n,
        };
        pending.extend_from_slice(&chunk[..n]);
        // only decode complete UTF-8; keep a split multi-byte char for the next read
        let (valid, bad) = match std::str::from_utf8(&pending) {
            Ok(s) => (s.len(), 0),
            Err(e) => (e.valid_up_to(), e.error_len().unwrap_or(0)), // error_len None = incomplete tail
        };
        buf.push_str(std::str::from_utf8(&pending[..valid]).unwrap());
        if bad > 0 {
            buf.push('\u{FFFD}'); // genuinely invalid bytes: replace instead of stalling forever
        }
        pending.drain(..valid + bad);
        for msg in ipc::drain_messages(&mut buf) {
            match serde_json::from_str::<Value>(&msg) {
                Ok(v) => {
                    if let Some(req) = v.get("HelperAPIRequest") {
                        handle_request(req, out.as_ref(), &paster, started);
                    }
                }
                Err(e) => log(&format!("bad message: {e}")),
            }
        }
    }
}

/// Work that types into or reads from the focused app, done in order on one thread.
enum Job {
    Paste(String, Value), // text, pasteId
    Keys(Vec<evdev::KeyCode>),
    CopySelection(String), // request uuid
}

/// Prefers AT-SPI; without it we cannot read the text box, but there always is one to paste
/// into; `accessibilityIsFunctioning: false` makes Wispr fetch selected text via GetSelectedTextViaCopy.
/// `x11`: the active X11 window, if any. AT-SPI focus that isn't in that window's process is stale
/// (e.g. you switched to an app without accessibility); on Wayland there is nothing to check against.
fn text_box_info(x11: Option<output::AppInfo>) -> Value {
    let stale = x11.filter(|a| !a.class.is_empty()).is_some_and(|a| a.pid.is_none() || a.pid != a11y::focused_pid());
    if let Some(t) = a11y::text_box().filter(|_| !stale) {
        return json!({ "payload": { "accessibilityIsFunctioning": true, "couldNotGetTextBoxInfo": false,
            "isEditable": true, "beforeText": t.before, "afterText": t.after,
            "contents": t.contents, "selectedText": t.selected } });
    }
    json!({ "payload": { "accessibilityIsFunctioning": false, "couldNotGetTextBoxInfo": false, "isEditable": true,
        "beforeText": "", "afterText": "", "contents": "", "selectedText": "" } })
}

/// Keyboard keys that are physically down right now: evdev code -> the Windows VK code reported for it.
static KEYS_HELD: Mutex<std::collections::BTreeMap<u16, u16>> = Mutex::new(std::collections::BTreeMap::new());

/// Of the keys Wispr believes are held, those that are not: it drops them, so a missed release
/// cannot leave a shortcut (and a dictation) stuck.
fn stale_keys(asked: &Value, down: &std::collections::BTreeSet<u16>) -> Vec<u64> {
    asked.as_array().into_iter().flatten().filter_map(Value::as_u64).filter(|k| !down.contains(&(*k as u16))).collect()
}

fn handle_request(req: &Value, out: Option<&Arc<Mutex<output::Output>>>, paster: &Sender<Job>, started: Instant) {
    let uuid = req["uuid"].as_str().unwrap_or_default().to_string();
    let Some(kind) = ipc::request_kind(req) else { return };
    let app = || with_a11y_name(out.map(|o| o.lock().unwrap().active_app()).unwrap_or_default());
    let body = match kind {
        "IsReady" => json!({ "ACK": true, "TapState": { "payload": { "tapEnabled": true } },
            "HelperLaunchTiming": { "helperMainEntryMs": started.elapsed().as_secs_f64() * 1000.0 } }),
        "GetAccessibilityStatus" => json!({ "AccessibilityStatus": { "payload": { "status": true } } }),
        "GetActiveAppInfo" => json!({ "ActiveAppInfo": { "payload": app_payload(&app()) } }),
        "GetAppInfo" => json!({ "AppInfo": { "payload": app_payload(&app()) } }),
        "GetHardwareInfo" => json!({ "HardwareInfo": { "payload": { "hasAppleFnKey": false, "isClamshellMode": false } } }),
        "GetRunningApps" => apps::running(),
        "GetDeviceInfo" => json!({ "DeviceInfo": { "payload": { "status": "missing" } } }),
        "GetMicrophoneHolders" => mic::snapshot(),
        "GetSlackHuddleState" => slack::state(),
        // Posting into a meeting chat needs the chat input positively identified; Linux cannot do that, so
        // type nothing and say so: Wispr then asks the user to post the notice by hand.
        // ponytail: Wispr never sends this on Linux (speaker polling is off); this only spares a patched build a 4 minute wait.
        "PostConsentChatMessage" => {
            ipc::respond(&uuid, json!({ "ACK": true }));
            let p = &req[kind]["payload"];
            ipc::request("ConsentChatMessageOutcome", json!({ "payload": { "success": false, "failureReason": "unsupported_platform",
                "platform": p["platform"], "sessionId": p["sessionId"] } }));
            return;
        }
        "SetMicrophoneHoldersMonitor" => {
            let p = &req["SetMicrophoneHoldersMonitor"]["payload"];
            mic::set_monitor(p["enabled"].as_bool().unwrap_or(false), p["cadenceMs"].as_u64())
        }
        "BleStartPairing" => {
            ble::start_pairing(req["BleStartPairing"]["payload"]["scanDurationSeconds"].as_u64().unwrap_or(30));
            json!({ "ACK": true })
        }
        "BleSetPairedDevice" => {
            ble::set_paired(req["BleSetPairedDevice"]["payload"]["deviceId"].as_str().map(str::to_owned));
            json!({ "ACK": true })
        }
        "DictationStop" => { ipc::respond(&uuid, json!({ "ACK": true })); media::dictation_stop(); return; }
        "GetTextBoxInfo" => json!({ "TextBoxInfo": text_box_info(out.map(|o| o.lock().unwrap().active_app())) }),
        "DictationStart" => {
            ipc::respond(&uuid, json!({ "ACK": true }));
            media::dictation_start();
            let x11 = out.map(|o| o.lock().unwrap().active_app());
            // AT-SPI focus that is not in the active X11 window is stale: do not read that app
            let stale = x11.as_ref().filter(|a| !a.class.is_empty()).is_some_and(|a| a.pid.is_none() || a.pid != a11y::focused_pid());
            ipc::request("TextBoxInfo", text_box_info(x11));
            if !stale && context_awareness_on() {
                std::thread::spawn(|| {
                    let t = Instant::now();
                    if let Some(text) = a11y::nearby_text() {
                        ipc::request("AppContextUpdate", json!({ "payload": { "conversationId": "", "nearestTexts": text,
                            "nearestTextDurationMs": t.elapsed().as_secs_f64() * 1000.0 } }));
                    }
                });
            }
            let a = app();
            if !a.class.is_empty() {
                ipc::request("AppInfoUpdate", json!({ "payload": app_payload(&a) }));
            }
            return;
        }
        "PasteText" => {
            ipc::respond(&uuid, json!({ "ACK": true }));
            let p = &req["PasteText"]["payload"];
            let (text, paste_id) = (p["text"].as_str().unwrap_or_default().to_string(), p["pasteId"].clone());
            let _ = paster.send(Job::Paste(text, paste_id));
            return;
        }
        "GetSelectedTextViaCopy" => {
            let _ = paster.send(Job::CopySelection(uuid)); // answered by the worker
            return;
        }
        "SimulateKeyPress" => {
            ipc::respond(&uuid, json!({ "ACK": true }));
            let p = &req["SimulateKeyPress"]["payload"];
            let mods = p["flags"].as_array().into_iter().flatten().filter_map(|f| keys::modifier_to_evdev(f.as_str()?));
            let key = p["keycode"].as_u64().and_then(|k| keys::vk_to_evdev(k as u16));
            match key {
                Some(k) => { let _ = paster.send(Job::Keys(mods.chain([k]).map(evdev::KeyCode::new).collect())); }
                None => log(&format!("SimulateKeyPress: unknown keycode {}", p["keycode"])),
            }
            return;
        }
        "HelperAppShutdown" => {
            ipc::respond(&uuid, json!({ "ACK": true }));
            media::shutdown();
            std::process::exit(0);
        }
        "CheckStaleKeys" => json!({ "StaleKeysResponse": { "payload": {
            "staleKeys": stale_keys(&req[kind]["payload"]["keycodes"], &KEYS_HELD.lock().unwrap().values().copied().collect()) } } }),
        // Everything else is macOS/Windows integration (meetings, BLE, accessibility trees...): acknowledge.
        _ => json!({ "ACK": true }),
    };
    ipc::respond(&uuid, body);
}

/// One worker so consecutive dictations are pasted in order.
fn spawn_paster(out: Option<Arc<Mutex<output::Output>>>) -> Sender<Job> {
    let (tx, rx) = channel::<Job>();
    std::thread::spawn(move || {
        for job in rx {
            match job {
                Job::Paste(text, paste_id) => paste(out.as_deref(), &text, paste_id),
                Job::Keys(chord) => {
                    if let Some(Err(e)) = out.as_deref().map(|o| o.lock().unwrap().press_chord(&chord)) {
                        log(&format!("SimulateKeyPress failed: {e:#}"));
                    }
                }
                Job::CopySelection(uuid) => {
                    let text = match out.as_deref().map(|o| {
                        let app = with_a11y_name(o.lock().unwrap().active_app()).class;
                        o.lock().unwrap().copy_selection(&app)
                    }) {
                        Some(Ok(t)) => t,
                        Some(Err(e)) => { log(&format!("copy selection failed: {e:#}")); String::new() }
                        None => String::new(),
                    };
                    ipc::respond(&uuid, json!({ "SelectedTextViaCopy": { "payload": {
                        "selectedText": text, "contents": text, "beforeText": "", "afterText": "" } } }));
                }
            }
        }
    });
    tx
}

fn paste(out: Option<&Mutex<output::Output>>, text: &str, paste_id: Value) {
    let t = Instant::now();
    let result = match out {
        Some(o) => o.lock().unwrap().paste(text),
        None => Err(anyhow::anyhow!("text insertion unavailable (no clipboard or keystroke access)")),
    };
    if let Err(e) = &result {
        log(&format!("paste failed: {e:#}"));
    }
    let mut payload = json!({ "success": result.is_ok(), "timeElapsedMs": t.elapsed().as_secs_f64() * 1000.0 });
    if paste_id.is_string() {
        payload["pasteId"] = paste_id;
    }
    if let Err(e) = result {
        payload["reason"] = json!(e.to_string());
    }
    let observe = payload["success"] == true;
    let id = payload["pasteId"].as_str().map(str::to_owned);
    ipc::request("PasteOutcome", json!({ "payload": payload }));
    if let (true, Some(id)) = (observe, id) {
        watch_edits(id, text.to_string());
    }
}

static PASTE_SEQ: AtomicU64 = AtomicU64::new(0);

/// After a paste, follows what the user does to the pasted text and reports the result once
/// (focus left the text box, the next paste, or a minute passed). Wispr learns corrections from it,
/// e.g. "Auto-add to dictionary". Needs AT-SPI; without it nothing is reported.
// ponytail: 1 s polling for at most 60 s; switch to AT-SPI text-changed events if that proves too coarse
fn watch_edits(paste_id: String, pasted: String) {
    let seq = PASTE_SEQ.fetch_add(1, Ordering::SeqCst) + 1;
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(400)); // let the app insert the text
        let (Some(focus), Some(tb)) = (a11y::focus_id(), a11y::text_box()) else { return };
        let Some((prefix, suffix)) = a11y::paste_anchor(&tb, &pasted).map(|(p, s)| (p.to_string(), s.to_string())) else { return };
        let mut edited = a11y::edited_region(&tb.contents, &prefix, &suffix).unwrap_or(&pasted).to_string();
        let mut reason = "timeout";
        for _ in 0..60 {
            std::thread::sleep(Duration::from_secs(1));
            if PASTE_SEQ.load(Ordering::SeqCst) != seq { reason = "new_paste"; break; }
            let tb = a11y::text_box().filter(|_| a11y::focus_id().as_deref() == Some(focus.as_str()));
            let Some(tb) = tb else { reason = "focus_lost"; break };
            if let Some(now) = a11y::edited_region(&tb.contents, &prefix, &suffix) {
                edited = now.to_string();
            }
        }
        ipc::request("UpdateEditedText", json!({ "payload": { "pasteId": paste_id, "pastedText": pasted,
            "editedText": edited, "contentObservationEndReason": reason } }));
    });
}

/// Wispr's "Context awareness" setting (Settings > Data and Privacy). Off, or unreadable: read nothing extra.
fn context_awareness_on() -> bool {
    let dir = std::env::var_os("XDG_CONFIG_HOME").map(std::path::PathBuf::from)
        .or_else(|| std::env::var_os("HOME").map(|h| std::path::PathBuf::from(h).join(".config")));
    let read = || -> Option<bool> {
        let v: Value = serde_json::from_slice(&std::fs::read(dir?.join("Wispr Flow/config.json")).ok()?).ok()?;
        v["prefs"]["user"]["useAxContext"].as_bool()
    };
    read().unwrap_or(false)
}

/// Wayland (or X11 without a class): fall back to the focused AT-SPI application's name.
const BROWSERS: [&str; 7] = ["chrome", "msedge", "vivaldi", "brave", "firefox", "opera", "zen"];

/// App info as Wispr expects it. `bundleId` uses the Windows exe name for apps Wispr knows (chrome, teams...);
/// `url` is the visible page of the active browser window, read over AT-SPI.
fn app_payload(a: &output::AppInfo) -> Value {
    let id = mic::wispr_app_id(&a.class.to_lowercase()).to_string();
    let browser = BROWSERS.contains(&id.as_str());
    let url = a.pid.filter(|_| browser).and_then(a11y::active_url);
    let url = url.or_else(|| browser.then(|| a11y::meeting_url_from_title(&a.title)).flatten()).unwrap_or_default();
    json!({ "appName": a.class, "bundleId": id, "url": url, "windowTitle": a.title })
}

fn with_a11y_name(mut a: output::AppInfo) -> output::AppInfo {
    if a.class.is_empty() {
        a.class = a11y::focused_app_name().unwrap_or_default();
    }
    a
}

/// Tells Electron which app is focused (Wispr uses it for per-app style and context).
fn watch_active_app(out: Arc<Mutex<output::Output>>) {
    std::thread::spawn(move || {
        let mut last = Value::Null;
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = with_a11y_name(out.lock().unwrap().active_app());
            if now.class.is_empty() {
                continue;
            }
            let payload = app_payload(&now);
            // a new app, or a new page in the same browser (tab switch, joining a meeting)
            if payload["bundleId"] != last["bundleId"] || payload["url"] != last["url"] {
                ipc::request("AppInfoUpdate", json!({ "payload": payload.clone() }));
                last = payload;
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reports_keys_that_are_no_longer_held() {
        let down = [162u16, 32].into_iter().collect();
        assert_eq!(stale_keys(&json!([162, 32, 91]), &down), vec![91]);
        assert_eq!(stale_keys(&json!([162]), &down), Vec::<u64>::new());
        assert_eq!(stale_keys(&Value::Null, &down), Vec::<u64>::new());
    }
}
