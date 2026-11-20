//! GetSlackHuddleState: tells a Slack huddle from other uses of the microphone (e.g. recording a clip).
//! Signal: Slack's pop-out huddle window, whose title starts with "Huddle:" (the same title Wispr's own
//! meeting detection matches for Slack). Nothing else is claimed: no such window answers `unknown`, never
//! `no_huddle`, because an embedded huddle (no pop-out) looks the same, and Wispr treats `unknown` as a call.
// ponytail: X11 titles only (Wayland: always unknown); the title is Wispr's Windows/macOS expectation, not verified against Slack on Linux.

use serde_json::{json, Value};
use std::time::Instant;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

/// (WM_CLASS application class, window title) of every top-level X11 window.
fn windows() -> Option<Vec<(String, String)>> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen].root;
    let atom = |n: &[u8]| conn.intern_atom(false, n).ok()?.reply().ok().map(|r| r.atom);
    let (list, name, utf8) = (atom(b"_NET_CLIENT_LIST")?, atom(b"_NET_WM_NAME")?, atom(b"UTF8_STRING")?);
    let wins = conn.get_property(false, root, list, AtomEnum::WINDOW, 0, 4096).ok()?.reply().ok()?;
    let mut out = Vec::new();
    for w in wins.value32()? {
        let class = conn.get_property(false, w, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256).ok().and_then(|c| c.reply().ok());
        let title = conn.get_property(false, w, name, utf8, 0, 1024).ok().and_then(|c| c.reply().ok());
        if let (Some(c), Some(t)) = (class, title) {
            let class = c.value.split(|b| *b == 0).nth(1).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
            out.push((class, String::from_utf8_lossy(&t.value).into_owned()));
        }
    }
    Some(out)
}

/// (verdict, reason) from the open windows; `None`: windows could not be listed.
fn verdict(wins: Option<&[(String, String)]>) -> (&'static str, &'static str) {
    let Some(wins) = wins else { return ("unknown", "read_error") };
    let huddle = |(class, title): &(String, String)| {
        class.to_lowercase().contains("slack") && title.trim_start().to_lowercase().starts_with("huddle:")
    };
    if wins.iter().any(huddle) { ("huddle", "marker") } else { ("unknown", "no_marker") }
}

pub fn state() -> Value {
    let t = Instant::now();
    let (verdict, reason) = verdict(windows().as_deref());
    json!({ "SlackHuddleState": { "payload": { "verdict": verdict, "reason": reason,
        "elapsedMs": t.elapsed().as_millis() as u64 } } })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn w(class: &str, title: &str) -> (String, String) { (class.into(), title.into()) }

    #[test]
    fn only_a_slack_huddle_window_is_a_huddle() {
        assert_eq!(verdict(Some(&[w("Slack", "Huddle: #general"), w("firefox", "x")])), ("huddle", "marker"));
        assert_eq!(verdict(Some(&[w("Slack", "general - Acme - Slack")])), ("unknown", "no_marker"));
        assert_eq!(verdict(Some(&[w("firefox", "Huddle: notes")])), ("unknown", "no_marker")); // not Slack's window
        assert_eq!(verdict(None), ("unknown", "read_error"));
    }
}
