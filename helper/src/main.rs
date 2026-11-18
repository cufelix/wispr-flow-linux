//! Linux replacement for Wispr Flow's native macOS/Windows helper.
//! Electron spawns it and talks over stdin/stdout (see ipc.rs). It reports every key press
//! (Electron matches shortcuts itself), pastes dictated text and reports the focused app.

mod a11y;
mod input;
mod ipc;
mod keys;
mod output;

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
            let prefix = if input_type == "mouse" { "mouse_event" } else { "key_event" };
            let event = format!("{prefix}_{}", if pressed { "press" } else { "release" });
            let i = index.fetch_add(1, Ordering::SeqCst) + 1;
            ipc::request("KeypressEvent", json!({ "payload": {
                "eventType": event, "index": i, "inputType": input_type, "key": key } }));
        })
    };
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
            Ok(0) | Err(_) => return, // Electron went away
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

fn handle_request(req: &Value, out: Option<&Arc<Mutex<output::Output>>>, paster: &Sender<Job>, started: Instant) {
    let uuid = req["uuid"].as_str().unwrap_or_default().to_string();
    let Some(kind) = ipc::request_kind(req) else { return };
    let app = || with_a11y_name(out.map(|o| o.lock().unwrap().active_app()).unwrap_or_default());
    let body = match kind {
        "IsReady" => json!({ "ACK": true, "TapState": { "payload": { "tapEnabled": true } },
            "HelperLaunchTiming": { "helperMainEntryMs": started.elapsed().as_secs_f64() * 1000.0 } }),
        "GetAccessibilityStatus" => json!({ "AccessibilityStatus": { "payload": { "status": true } } }),
        "GetActiveAppInfo" => { let a = app(); json!({ "ActiveAppInfo": { "payload": {
            "appName": a.class, "bundleId": a.class.to_lowercase(), "url": "", "windowTitle": a.title } } }) }
        "GetAppInfo" => { let a = app(); json!({ "AppInfo": { "payload": {
            "appName": a.class, "bundleId": a.class.to_lowercase(), "url": "" } } }) }
        "GetHardwareInfo" => json!({ "HardwareInfo": { "payload": { "hasAppleFnKey": false, "isClamshellMode": false } } }),
        "GetRunningApps" => json!({ "RunningApps": { "payload": { "apps": [] } } }),
        "GetTextBoxInfo" => json!({ "TextBoxInfo": text_box_info(out.map(|o| o.lock().unwrap().active_app())) }),
        "DictationStart" => {
            ipc::respond(&uuid, json!({ "ACK": true }));
            ipc::request("TextBoxInfo", text_box_info(out.map(|o| o.lock().unwrap().active_app())));
            let a = app();
            if !a.class.is_empty() {
                ipc::request("AppInfoUpdate", json!({ "payload": {
                    "appName": a.class, "bundleId": a.class.to_lowercase(), "url": "", "windowTitle": a.title } }));
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
            std::process::exit(0);
        }
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
    ipc::request("PasteOutcome", json!({ "payload": payload }));
}

/// Wayland (or X11 without a class): fall back to the focused AT-SPI application's name.
fn with_a11y_name(mut a: output::AppInfo) -> output::AppInfo {
    if a.class.is_empty() {
        a.class = a11y::focused_app_name().unwrap_or_default();
    }
    a
}

/// Tells Electron which app is focused (Wispr uses it for per-app style and context).
fn watch_active_app(out: Arc<Mutex<output::Output>>) {
    std::thread::spawn(move || {
        let mut last = output::AppInfo::default();
        loop {
            std::thread::sleep(Duration::from_millis(500));
            let now = with_a11y_name(out.lock().unwrap().active_app());
            if now.class != last.class && !now.class.is_empty() {
                ipc::request("AppInfoUpdate", json!({ "payload": {
                    "appName": now.class, "bundleId": now.class.to_lowercase(), "url": "", "windowTitle": now.title } }));
                last = now;
            }
        }
    });
}
