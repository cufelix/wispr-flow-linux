//! AT-SPI2 text context (focused text box, focused app name) over the accessibility bus.
//! Everything degrades to `None`: no a11y bus, nothing focused yet, object gone, not a Text.

use std::sync::{Mutex, OnceLock};
use std::time::{Duration, Instant};
use zbus::blocking::{Connection, MessageIterator};
use zbus::zvariant::{OwnedValue, Value};

const CALL_TIMEOUT: Duration = Duration::from_millis(100); // per D-Bus call
const BUDGET: Duration = Duration::from_millis(150); // whole text_box() query
const MAX_CHARS: i32 = 20000;
pub const ACCESSIBLE: &str = "org.a11y.atspi.Accessible";
const TEXT: &str = "org.a11y.atspi.Text";
// AtspiStateType indices (bit n of the first u32 returned by GetState)
pub const ROLE_PASSWORD_TEXT: u32 = 40;
const STATE_FOCUSED: u32 = 12;
const STATE_EDITABLE: u32 = 7;

pub struct TextBox {
    pub before: String,
    pub after: String,
    pub contents: String,
    pub selected: String,
}

pub type Target = (String, String); // (bus name, object path)
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

pub fn call<B: zbus::zvariant::DynamicType + zbus::export::serde::Serialize>(
    t: &Target, iface: &str, member: &str, body: &B,
) -> Option<zbus::Message> {
    conn()?.call_method(Some(t.0.as_str()), t.1.as_str(), Some(iface), member, body).ok()
}

pub fn prop<T: TryFrom<OwnedValue>>(t: &Target, iface: &str, name: &str) -> Option<T> {
    let m = call(t, "org.freedesktop.DBus.Properties", "Get", &(iface, name))?;
    T::try_from(m.body().deserialize::<OwnedValue>().ok()?).ok()
}

/// Process that owns the focused object (to check it is the app in the active window).
pub fn focused_pid() -> Option<u32> {
    bus_pid(&focus()?.0)
}

pub fn bus_pid(bus: &str) -> Option<u32> {
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

pub fn state_bits(t: &Target) -> Option<u32> {
    let s: Vec<u32> = call(t, ACCESSIBLE, "GetState", &())?.body().deserialize().ok()?;
    s.first().copied()
}

pub fn children(t: &Target) -> Vec<Target> {
    call(t, ACCESSIBLE, "GetChildren", &())
        .and_then(|m| m.body().deserialize::<Vec<(String, zbus::zvariant::OwnedObjectPath)>>().ok())
        .map(|v| v.into_iter().map(|(bus, path)| (bus, path.to_string())).collect())
        .unwrap_or_default()
}

/// The visible web document under `frame` and its URL. Never descends into page content.
fn document_in(frame: Target, deadline: Instant) -> Option<(Target, String)> {
    let (mut stack, mut seen) = (vec![frame], 0);
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
                    return Some((node, url));
                }
            }
            continue; // never descend into page content
        }
        stack.extend(children(&node).into_iter().rev());
    }
    None
}

fn registry() -> Target {
    ("org.a11y.atspi.Registry".to_string(), "/org/a11y/atspi/accessible/root".to_string())
}

/// URL of the visible page in the active window of browser process `pid` (Firefox; Chrome/Chromium only with
/// --force-renderer-accessibility). Found through the registry, not focus: Chrome sends no focus events.
/// Wispr reads the meeting service (Meet, Teams, Zoom web) from it, as it does from the address bar on Windows.
pub fn active_url(pid: u32) -> Option<String> {
    let deadline = Instant::now() + BUDGET;
    let app = children(&registry()).into_iter().find(|(bus, _)| bus_pid(bus) == Some(pid))?;
    // only the active window, and never a private one: the URL goes to Wispr's servers
    let frame = children(&app).into_iter().find(|f| state_bits(f).is_some_and(|s| s & (1 << STATE_ACTIVE) != 0))?;
    let title: String = prop(&frame, ACCESSIBLE, "Name").unwrap_or_default();
    if is_private_window(&title) {
        return None;
    }
    document_in(frame, deadline).map(|(_, url)| url)
}

const BROWSERS: [&str; 8] = ["firefox", "chrome", "chromium", "brave", "msedge", "vivaldi", "zen", "opera"];

/// Visible web documents (node, URL) of every browser window that is not private, active or not (150 ms per window).
// ponytail: browsers recognized by process name; a renamed binary is not found
pub fn browser_documents() -> Vec<(Target, String)> {
    let is_browser = |bus: &str| {
        let comm = bus_pid(bus).and_then(|p| std::fs::read_to_string(format!("/proc/{p}/comm")).ok()).unwrap_or_default().to_lowercase();
        BROWSERS.iter().any(|b| comm.contains(b))
    };
    let mut docs = Vec::new();
    for app in children(&registry()).into_iter().filter(|(bus, _)| is_browser(bus)) {
        for frame in children(&app) {
            let title: String = prop(&frame, ACCESSIBLE, "Name").unwrap_or_default();
            if !is_private_window(&title) && let Some(doc) = document_in(frame, Instant::now() + BUDGET) {
                docs.push(doc);
            }
        }
    }
    docs
}

/// Identity of the focused object, to notice when focus moves elsewhere.
pub fn focus_id() -> Option<String> {
    focus().map(|(bus, path)| format!("{bus}{path}"))
}

const NEARBY_NODE_LIMIT: usize = 250;
const NEARBY_BUDGET: Duration = Duration::from_millis(300);
const NEARBY_CHARS: usize = 4000;
const NEARBY_LINE: usize = 300;

/// Visible text of the active window of the focused app (labels, headings, messages), one item per line.
/// Wispr uses it to spell the names and terms on screen, as its macOS/Windows helpers do.
/// Password fields and private browser windows are never read.
// ponytail: bounded walk (250 nodes / 300 ms / 4000 chars) from the top of the window; start at the
// focused element and widen outwards if large pages need the text nearest the cursor
pub fn nearby_text() -> Option<String> {
    let focused = focus()?;
    // a focus we can no longer confirm may belong to a window the user already left: read nothing
    if state_bits(&focused).is_none_or(|s| s & (1 << STATE_FOCUSED) == 0) {
        return None;
    }
    let (frame, title) = active_frame(&focused)?;
    if is_private_window(&title) {
        return None;
    }
    let deadline = Instant::now() + NEARBY_BUDGET;
    let (mut stack, mut seen, mut out) = (vec![frame], 0, String::new());
    while let Some(node) = stack.pop() {
        seen += 1;
        if seen > NEARBY_NODE_LIMIT || Instant::now() > deadline || out.len() > NEARBY_CHARS {
            break;
        }
        if state_bits(&node).is_none_or(|s| s & (1 << STATE_SHOWING) == 0) {
            continue;
        }
        // unknown role: it could be a password field, so skip it and everything under it
        let Some(role) = call(&node, ACCESSIBLE, "GetRole", &()).and_then(|m| m.body().deserialize::<u32>().ok()) else { continue };
        if role == ROLE_PASSWORD_TEXT {
            continue;
        }
        let name: String = prop(&node, ACCESSIBLE, "Name").unwrap_or_default();
        // an unnamed node's own text only if it is not an input: other fields (codes, card numbers) stay unread
        let editable = state_bits(&node).is_some_and(|s| s & (1 << STATE_EDITABLE) != 0);
        let text = if !name.trim().is_empty() {
            name
        } else if editable {
            String::new()
        } else {
            call(&node, TEXT, "GetText", &(0i32, NEARBY_LINE as i32)).and_then(|m| m.body().deserialize::<String>().ok()).unwrap_or_default()
        };
        push_line(&mut out, &text.chars().take(NEARBY_LINE).collect::<String>());
        stack.extend(children(&node).into_iter().rev());
    }
    let out: String = out.chars().take(NEARBY_CHARS).collect();
    (!out.is_empty()).then_some(out)
}

/// The active window of the app owning `focused`, with its title.
fn active_frame(focused: &Target) -> Option<(Target, String)> {
    let root = (focused.0.clone(), "/org/a11y/atspi/accessible/root".to_string());
    let frame = children(&root).into_iter().find(|f| state_bits(f).is_some_and(|s| s & (1 << STATE_ACTIVE) != 0))?;
    let title: String = prop(&frame, ACCESSIBLE, "Name").unwrap_or_default();
    Some((frame, title))
}

/// Whether the focused text box sits in a window we may read: one whose title we can see and that is not private.
/// Unknown counts as no.
pub fn focused_window_readable() -> bool {
    focus().and_then(|f| active_frame(&f)).is_some_and(|(_, title)| !title.trim().is_empty() && !is_private_window(&title))
}

/// Appends `text` as one line, skipping blanks, object placeholders and immediate repeats.
fn push_line(out: &mut String, text: &str) {
    let line = text.replace('\u{fffc}', " ").split_whitespace().collect::<Vec<_>>().join(" ");
    if !line.is_empty() && out.lines().last() != Some(line.as_str()) {
        out.push_str(&line);
        out.push('\n');
    }
}

/// Where a pasted text sits in a text box: the text before it and the text after it.
/// None if the box does not end the text before the caret with what was pasted.
pub fn paste_anchor<'a>(tb: &'a TextBox, pasted: &str) -> Option<(&'a str, &'a str)> {
    let pasted = pasted.trim_end();
    let before = tb.before.trim_end();
    (!pasted.is_empty() && before.ends_with(pasted)).then(|| (&tb.before[..before.len() - pasted.len()], tb.after.as_str()))
}

/// The text now between `prefix` and `suffix`: what the pasted text became after the user's edits.
/// None once the text around it changed too, so the region can no longer be told apart.
pub fn edited_region<'a>(contents: &'a str, prefix: &str, suffix: &str) -> Option<&'a str> {
    contents.strip_prefix(prefix)?.strip_suffix(suffix)
}

/// Meeting URL guessed from a browser window title, for browsers that hide their page from AT-SPI (Chrome without
/// --force-renderer-accessibility). The title already goes to Wispr as `windowTitle`.
// ponytail: title heuristic for Meet codes and Teams only; the real URL wins whenever AT-SPI has it
pub fn meeting_url_from_title(title: &str) -> Option<String> {
    if is_private_window(title) {
        return None;
    }
    let is_code = |w: &str| {
        let parts: Vec<&str> = w.split('-').collect();
        parts.len() == 3 && [3, 4, 3].iter().zip(&parts).all(|(n, p)| p.len() == *n && p.bytes().all(|b| b.is_ascii_lowercase()))
    };
    if title.starts_with("Meet")
        && let Some(code) = title.split_whitespace().find(|w| is_code(w))
    {
        return Some(format!("https://meet.google.com/{code}"));
    }
    title.contains("| Microsoft Teams").then(|| "https://teams.microsoft.com/".to_string())
}

/// Private/incognito browser windows, by title: Firefox "… — Private Browsing", Chrome "… (Incognito)", Edge
/// "… InPrivate", Brave "… Private", and the same in other UI languages.
// ponytail: a title word list (browsers expose no private flag over X11/AT-SPI); add words as languages come up
pub fn is_private_window(title: &str) -> bool {
    const MARKS: [&str; 12] = ["private", "privat", "privé", "privad", "privát", "incognito", "inkognito", "incógnito",
        "anonym", "inprivate", "приват", "инкогнито"];
    let title = title.to_lowercase();
    MARKS.iter().any(|m| title.contains(m))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn guesses_meeting_url_from_title() {
        let url = |t| meeting_url_from_title(t);
        assert_eq!(url("Meet - abc-defg-hij - Google Chrome").as_deref(), Some("https://meet.google.com/abc-defg-hij"));
        assert_eq!(url("Chat | Daily | Microsoft Teams - Chromium").as_deref(), Some("https://teams.microsoft.com/"));
        assert_eq!(url("Meet - abc-defg-hij - Google Chrome (Incognito)"), None);
        assert_eq!(url("well-made-pie recipe - Google Chrome"), None);
        assert_eq!(url("Meet the team - Google Chrome"), None);
    }

    #[test]
    fn detects_private_windows() {
        assert!(is_private_window("Meet — Mozilla Firefox Private Browsing"));
        assert!(is_private_window("New Tab - Google Chrome (Incognito)"));
        assert!(!is_private_window("Meet - abc-defg-hij - Google Chrome"));
        assert!(is_private_window("Nová karta – Google Chrome (Anonymní)"));
        assert!(is_private_window("Neuer Tab - Google Chrome (Inkognito)"));
        assert!(is_private_window("New tab - Microsoft Edge InPrivate"));
    }

    #[test]
    fn follows_edits_to_pasted_text() {
        let tb = TextBox { before: "Dear Sam, see you tomorow ".into(), after: "\nBye".into(), contents: String::new(), selected: String::new() };
        let (prefix, suffix) = paste_anchor(&tb, "see you tomorow ").unwrap();
        assert_eq!((prefix, suffix), ("Dear Sam, ", "\nBye"));
        assert_eq!(edited_region("Dear Sam, see you tomorrow \nBye", prefix, suffix), Some("see you tomorrow "));
        assert_eq!(edited_region("Hi Sam, see you tomorrow \nBye", prefix, suffix), None); // text before it changed
        assert!(paste_anchor(&tb, "something else").is_none());
        assert!(paste_anchor(&tb, "  ").is_none());
    }

    #[test]
    fn collects_lines_without_repeats() {
        let mut out = String::new();
        for t in ["Inbox", "  ", "Inbox", "Anna\u{fffc}  Novak", "Inbox"] { push_line(&mut out, t); }
        assert_eq!(out, "Inbox\nAnna Novak\nInbox\n");
    }

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
