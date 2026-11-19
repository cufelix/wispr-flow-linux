//! AT-SPI2 text context (focused text box, focused app name) over the accessibility bus.
//! Everything degrades to `None`: no a11y bus, nothing focused yet, object gone, not a Text.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedValue, Value};

const CALL_TIMEOUT: Duration = Duration::from_millis(100); // per D-Bus call
const BUDGET: Duration = Duration::from_millis(150); // whole text_box() query
const MAX_CHARS: i32 = 20000;
const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const TEXT: &str = "org.a11y.atspi.Text";
// AtspiStateType indices (bit n of the first u32 returned by GetState)
const ROLE_PASSWORD_TEXT: u32 = 40;
const STATE_FOCUSED: u32 = 12;
const STATE_EDITABLE: u32 = 7;

pub struct TextBox {
    pub before: String,
    pub after: String,
    pub contents: String,
    pub selected: String,
}

type Target = (String, String); // (bus name, object path)
static FOCUS: Mutex<Option<Target>> = Mutex::new(None);
static QUERY_CONN: OnceLock<Option<Connection>> = OnceLock::new();

/// Enables toolkit accessibility and starts tracking focus on a background thread.
pub fn start() {
    std::thread::spawn(|| {
        let _ = run();
    });
}

fn run() -> zbus::Result<()> {
    let session = Connection::session()?;
    // toolkits (GTK, Qt, Chromium) only expose their trees once this is set
    let _ = session.call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.freedesktop.DBus.Properties"),
        "Set", &("org.a11y.Status", "IsEnabled", Value::from(true)));
    let addr: String = session
        .call_method(Some("org.a11y.Bus"), "/org/a11y/bus", Some("org.a11y.Bus"), "GetAddress", &())?
        .body().deserialize()?;
    let connect = |timeout: Option<Duration>| -> zbus::Result<Connection> {
        let mut b = zbus::blocking::connection::Builder::address(addr.as_str())?;
        if let Some(t) = timeout {
            b = b.method_timeout(t);
        }
        b.build()
    };
    let _ = QUERY_CONN.set(connect(Some(CALL_TIMEOUT)).ok());
    let listener = connect(None)?;
    let rule = "type='signal',interface='org.a11y.atspi.Event.Object',member='StateChanged'";
    let events = MessageIterator::for_match_rule(zbus::MatchRule::try_from(rule)?, &listener, None)?;
    // tells apps (Chromium in particular) that someone listens for focus changes
    let _ = listener.call_method(Some("org.a11y.atspi.Registry"), "/org/a11y/atspi/registry",
        Some("org.a11y.atspi.Registry"), "RegisterEvent", &("object:state-changed:focused", Vec::<String>::new(), ""));
    for msg in events.flatten() {
        let hdr = msg.header();
        let (Some(sender), Some(path)) = (hdr.sender(), hdr.path()) else { continue };
        // StateChanged body: (detail: s, detail1: i, detail2: i, value: v, props: a{sv})
        let Ok((detail, on, _, _, _)) = msg.body().deserialize::<(String, i32, i32, Value, std::collections::HashMap<String, Value>)>() else { continue };
        if detail == "focused" && on == 1 {
            *FOCUS.lock().unwrap() = Some((sender.to_string(), path.to_string()));
        }
    }
    Ok(())
}

fn focus() -> Option<Target> {
    FOCUS.lock().unwrap().clone()
}

fn conn() -> Option<&'static Connection> {
    QUERY_CONN.get()?.as_ref()
}

fn call<B: zbus::zvariant::DynamicType + zbus::export::serde::Serialize>(
    t: &Target, iface: &str, member: &str, body: &B,
) -> Option<zbus::Message> {
    conn()?.call_method(Some(t.0.as_str()), t.1.as_str(), Some(iface), member, body).ok()
}

fn prop<T: TryFrom<OwnedValue>>(t: &Target, iface: &str, name: &str) -> Option<T> {
    let m = call(t, "org.freedesktop.DBus.Properties", "Get", &(iface, name))?;
    T::try_from(m.body().deserialize::<OwnedValue>().ok()?).ok()
}

/// Process that owns the focused object (to check it is the app in the active window).
pub fn focused_pid() -> Option<u32> {
    bus_pid(&focus()?.0)
}

fn bus_pid(bus: &str) -> Option<u32> {
    let m = conn()?.call_method(Some("org.freedesktop.DBus"), "/org/freedesktop/DBus", Some("org.freedesktop.DBus"),
        "GetConnectionUnixProcessID", &(bus,)).ok()?;
    m.body().deserialize().ok()
}

/// Splits `text` at a caret given in Unicode chars (not bytes).
fn split_at_char(text: &str, caret: usize) -> (&str, &str) {
    let byte = text.char_indices().nth(caret).map_or(text.len(), |(i, _)| i);
    text.split_at(byte)
}

/// Substring by char offsets, clamped to the text.
fn char_slice(text: &str, start: usize, end: usize) -> String {
    text.chars().skip(start).take(end.saturating_sub(start)).collect()
}

/// The focused object, if it is an editable Text. None if anything is missing or too slow.
pub fn text_box() -> Option<TextBox> {
    let t = focus()?;
    let deadline = Instant::now() + BUDGET;
    let ok = || Instant::now() < deadline;

    let state: Vec<u32> = call(&t, ACCESSIBLE, "GetState", &())?.body().deserialize().ok()?;
    // A stale object (its window lost focus to a non-accessible app) loses FOCUSED; never
    // report it, nor non-editable text, or Wispr would refuse to paste.
    let bits = state.first()?;
    let is_editable = bits & (1 << STATE_EDITABLE) != 0;
    if !is_editable || bits & (1 << STATE_FOCUSED) == 0 || !ok() { return None; }
    // never read password fields: Wispr sends text box context to its servers
    let role: u32 = call(&t, ACCESSIBLE, "GetRole", &())?.body().deserialize().ok()?;
    if role == ROLE_PASSWORD_TEXT {
        return Some(TextBox { before: String::new(), after: String::new(), contents: String::new(), selected: String::new() });
    }
    let count: i32 = prop(&t, TEXT, "CharacterCount")?; // also proves the object implements Text
    let caret: i32 = prop(&t, TEXT, "CaretOffset")?;
    if !ok() { return None; }
    let contents: String = call(&t, TEXT, "GetText", &(0i32, count.clamp(0, MAX_CHARS)))?.body().deserialize().ok()?;
    let (mut before, mut after) = split_at_char(&contents, caret.max(0) as usize);
    let mut selected = String::new();
    // Wispr trusts an empty selectedText here, so give up rather than guess
    if !ok() { return None; }
    let n: i32 = call(&t, TEXT, "GetNSelections", &())?.body().deserialize().ok()?;
    if n > 0 {
        let (s, e): (i32, i32) = call(&t, TEXT, "GetSelection", &(0i32,))?.body().deserialize().ok()?;
        let (s, e) = (s.clamp(0, e.max(0)) as usize, e.max(0) as usize);
        selected = char_slice(&contents, s, e);
        // like macOS: before/after surround the selection, not the caret inside it
        before = split_at_char(&contents, s).0;
        after = split_at_char(&contents, e).1;
    }
    Some(TextBox { before: before.into(), after: after.into(), contents: contents.clone(), selected })
}

/// Name of the application owning the last focused object (its accessible root).
pub fn focused_app_name() -> Option<String> {
    let (bus, _) = focus()?;
    let root = (bus, "/org/a11y/atspi/accessible/root".to_string());
    let name: String = prop(&root, ACCESSIBLE, "Name")?;
    (!name.is_empty()).then_some(name)
}

const ROLE_DOCUMENT_WEB: u32 = 95;
const STATE_ACTIVE: u32 = 1;
const STATE_SHOWING: u32 = 25;
const URL_NODE_LIMIT: usize = 300;

fn state_bits(t: &Target) -> Option<u32> {
    let s: Vec<u32> = call(t, ACCESSIBLE, "GetState", &())?.body().deserialize().ok()?;
    s.first().copied()
}

fn children(t: &Target) -> Vec<Target> {
    call(t, ACCESSIBLE, "GetChildren", &())
        .and_then(|m| m.body().deserialize::<Vec<(String, zbus::zvariant::OwnedObjectPath)>>().ok())
        .map(|v| v.into_iter().map(|(bus, path)| (bus, path.to_string())).collect())
        .unwrap_or_default()
}

/// URL of the visible page in the active window of browser process `pid` (Firefox; Chrome/Chromium only with
/// --force-renderer-accessibility). Found through the registry, not focus: Chrome sends no focus events.
/// Wispr reads the meeting service (Meet, Teams, Zoom web) from it, as it does from the address bar on Windows.
pub fn active_url(pid: u32) -> Option<String> {
    let deadline = Instant::now() + BUDGET;
    let registry = ("org.a11y.atspi.Registry".to_string(), "/org/a11y/atspi/accessible/root".to_string());
    let app = children(&registry).into_iter().find(|(bus, _)| bus_pid(bus) == Some(pid))?;
    let frames = children(&app);
    let active: Vec<Target> =
        frames.iter().filter(|f| state_bits(f).is_some_and(|s| s & (1 << STATE_ACTIVE) != 0)).cloned().collect();
    let (mut stack, mut seen) = (if active.is_empty() { frames } else { active }, 0);
    while let Some(node) = stack.pop() {
        seen += 1;
        if seen > URL_NODE_LIMIT || Instant::now() > deadline {
            return None;
        }
        if state_bits(&node).is_none_or(|s| s & (1 << STATE_SHOWING) == 0) {
            continue; // hidden tabs and their subtrees
        }
        let role: Option<u32> = call(&node, ACCESSIBLE, "GetRole", &()).and_then(|m| m.body().deserialize().ok());
        if role == Some(ROLE_DOCUMENT_WEB) {
            for key in ["DocURL", "URI"] {
                let url: Option<String> = call(&node, "org.a11y.atspi.Document", "GetAttributeValue", &(key,))
                    .and_then(|m| m.body().deserialize().ok());
                if let Some(url) = url.filter(|u| u.starts_with("http")) {
                    return Some(url);
                }
            }
            continue; // never descend into page content
        }
        stack.extend(children(&node).into_iter().rev());
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn splits_on_chars_not_bytes() {
        assert_eq!(split_at_char("héllo wörld", 3), ("hél", "lo wörld"));
        assert_eq!(split_at_char("日本語", 1), ("日", "本語"));
        assert_eq!(split_at_char("abc", 3), ("abc", ""));
        assert_eq!(split_at_char("abc", 99), ("abc", ""));
        assert_eq!(split_at_char("", 0), ("", ""));
    }

    #[test]
    fn slices_by_chars() {
        assert_eq!(char_slice("héllo wörld", 6, 11), "wörld");
        assert_eq!(char_slice("abc", 2, 9), "c");
        assert_eq!(char_slice("abc", 2, 1), "");
    }
}
