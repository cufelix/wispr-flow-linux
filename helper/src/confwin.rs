//! Conference window monitor: Wispr's `ControlConferenceWindowMonitor` (arm / read / disarm, carried by
//! `GetConferenceEndState`). It tracks the browser window of a Google Meet call, so that when the microphone is
//! released Wispr can tell the window itself went away (call left, window closed) and stop at once instead of
//! asking first. Only a tracked window that then vanishes from the X11 client list is reported as removed; whatever
//! this cannot tell (title changed, no window found, no X11) is never an end.
//! A window shows only its active tab's title, so the browser's tab strip is read too (AT-SPI): the Meet tab closed
//! in a window that stays open counts as removed, and a Meet tab that is merely in the background as present.
// ponytail: where the tab strip cannot be read (accessibility off), a closed tab in a surviving window stays "unknown".

use crate::{mic, output};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, Instant};
use x11rb::connect;
use x11rb::connection::Connection;
use x11rb::rust_connection::RustConnection;

const POLL: Duration = Duration::from_millis(500);
const MIN_NEEDLE: usize = 8; // a Meet code is 12; shorter text would match unrelated windows

static GENERATION: AtomicU64 = AtomicU64::new(0);
static MONITOR: Mutex<Option<Tracker>> = Mutex::new(None);

type Windows = [(u32, output::AppInfo)];

struct Tracker {
    generation: u64,
    needle: String,
    family: String, // Wispr browser id of the call's browser, e.g. "chrome"
    tracked: Option<u32>,
    tab_seen: bool, // the call's tab was once found in the tab strip
    removed: u64,
    changed: Option<Instant>,
    sampled: Option<Instant>,
    transition: &'static str,
}

impl Tracker {
    fn new(generation: u64, needle: &str, bundle_ids: &[&str]) -> Self {
        let family = bundle_ids.first().map(|b| b.trim_end_matches(".exe").to_lowercase()).unwrap_or_default();
        let known = bundle_ids.len() == 1 && ["chrome", "msedge"].contains(&family.as_str());
        let transition = if known && needle.len() >= MIN_NEEDLE { "arm_no_candidate" } else { "arm_incomplete" };
        Self { generation, needle: needle.to_lowercase(), family, tracked: None, tab_seen: false, removed: 0, changed: None, sampled: None, transition }
    }

    fn is_candidate(&self, a: &output::AppInfo) -> bool {
        mic::wispr_app_id(&a.class.to_lowercase()) == self.family
            && !crate::a11y::is_private_window(&a.title)
            && a.title.to_lowercase().contains(&self.needle)
    }

    /// `tabs`: titles of the tracked browser's tabs, None when its tab strip could not be read.
    fn observe(&mut self, wins: &Windows, tabs: Option<&[String]>, now: Instant) {
        // an empty list is a window manager restart or a failed read, not "every window closed"
        if wins.is_empty() || self.transition == "arm_incomplete" || self.removed > 0 {
            return;
        }
        self.sampled = Some(now);
        match self.tracked {
            Some(id) => match wins.iter().find(|w| w.0 == id) {
                Some((_, a)) => {
                    let in_tabs = tabs.map(|t| t.iter().any(|t| t.to_lowercase().contains(&self.needle)));
                    self.tab_seen |= in_tabs == Some(true);
                    if self.is_candidate(a) || in_tabs == Some(true) {
                        self.transition = "present";
                    } else if in_tabs == Some(false) && self.tab_seen {
                        // the window lives on, the call's tab is gone from a tab strip that used to show it
                        (self.tracked, self.removed, self.changed, self.transition) = (None, 1, Some(now), "window_removed");
                    } else {
                        self.transition = "unknown";
                    }
                }
                None => {
                    (self.tracked, self.removed, self.changed, self.transition) = (None, 1, Some(now), "window_removed");
                }
            },
            None => {
                let found: Vec<u32> = wins.iter().filter(|w| self.is_candidate(&w.1)).map(|w| w.0).collect();
                self.transition = match found.as_slice() {
                    [] => "arm_no_candidate",
                    [id] => { self.tracked = Some(*id); "located" }
                    _ => "arm_ambiguous",
                };
            }
        }
    }

    fn payload(&self, now: Instant) -> Value {
        let age = |t: Instant| now.saturating_duration_since(t).as_millis() as u64;
        let mut p = json!({ "status": "ok", "controls": [], "elapsedMs": 0, "monitorGeneration": self.generation,
            "monitorTransition": self.transition, "monitorWindowRemovedCount": self.removed });
        if let Some(t) = self.changed { p["monitorWindowChangeAgeMs"] = json!(age(t)); }
        if let Some(t) = self.sampled { p["monitorSampleAgeMs"] = json!(age(t)); }
        p
    }
}

fn reply(payload: Value) -> Value {
    json!({ "ConferenceEndState": { "payload": payload } })
}

/// Reply body for a GetConferenceEndState monitor request; None when this is not one or X11 is unavailable (Wayland):
/// the caller then answers like before and Wispr keeps its microphone-only behaviour.
pub fn control(req: &Value) -> Option<Value> {
    let asked_gen = req["monitorGeneration"].as_u64();
    match req["monitorAction"].as_str()? {
        "arm" => {
            let (conn, screen) = connect(None).ok()?;
            let root = conn.setup().roots[screen].root;
            let ids: Vec<&str> = req["bundleIds"].as_array().into_iter().flatten().filter_map(Value::as_str).collect();
            let generation = GENERATION.fetch_add(1, Ordering::SeqCst) + 1; // also stops the previous poller
            let tracker = Tracker::new(generation, req["matches"].as_str().unwrap_or_default(), &ids);
            let first = tracker.payload(Instant::now());
            *MONITOR.lock().unwrap() = Some(tracker);
            std::thread::spawn(move || poll(conn, root, generation));
            Some(reply(first))
        }
        "read" => {
            let guard = MONITOR.lock().unwrap();
            let payload = match guard.as_ref().filter(|t| Some(t.generation) == asked_gen) {
                Some(t) => t.payload(Instant::now()),
                None => json!({ "status": "ok", "controls": [], "elapsedMs": 0, "monitorGeneration": asked_gen }),
            };
            Some(reply(payload))
        }
        "disarm" => {
            let mut guard = MONITOR.lock().unwrap();
            if asked_gen.is_none() || guard.as_ref().is_some_and(|t| Some(t.generation) == asked_gen) {
                GENERATION.fetch_add(1, Ordering::SeqCst);
                *guard = None;
            }
            Some(reply(json!({ "status": "ok", "controls": [], "elapsedMs": 0, "monitorGeneration": asked_gen })))
        }
        _ => None,
    }
}

fn poll(conn: RustConnection, root: u32, generation: u64) {
    while GENERATION.load(Ordering::SeqCst) == generation {
        if let Ok(wins) = output::client_windows(&conn, root)
            && let Some(t) = MONITOR.lock().unwrap().as_mut().filter(|t| t.generation == generation)
        {
            let pid = t.tracked.and_then(|id| wins.iter().find(|w| w.0 == id)).and_then(|w| w.1.pid);
            let tabs = pid.and_then(crate::a11y::tab_titles);
            t.observe(&wins, tabs.as_deref(), Instant::now());
        }
        std::thread::sleep(POLL);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn win(id: u32, class: &str, title: &str) -> (u32, output::AppInfo) {
        (id, output::AppInfo { class: class.into(), title: title.into(), pid: None })
    }
    const MEET: &str = "Meet - abc-defg-hij - Google Chrome";

    #[test]
    fn reports_removal_only_after_the_window_was_seen_and_then_vanished() {
        let mut t = Tracker::new(1, "ABC-defg-hij", &["chrome.exe"]);
        let now = Instant::now();
        let other = win(2, "Google-chrome", "Inbox - Google Chrome");
        t.observe(std::slice::from_ref(&other), None, now);
        assert_eq!((t.transition, t.removed), ("arm_no_candidate", 0)); // never seen: never "removed"
        t.observe(&[other.clone(), win(7, "Google-chrome", MEET)], None, now);
        assert_eq!(t.transition, "located");
        t.observe(&[other.clone(), win(7, "Google-chrome", MEET)], None, now);
        assert_eq!(t.transition, "present");
        t.observe(&[other.clone(), win(7, "Google-chrome", "Docs - Google Chrome")], None, now); // tab switched: cannot tell
        assert_eq!((t.transition, t.removed), ("unknown", 0));
        t.observe(&[], None, now); // window manager hiccup is not a closed window
        assert_eq!(t.removed, 0);
        t.observe(std::slice::from_ref(&other), None, now);
        assert_eq!((t.transition, t.removed), ("window_removed", 1));
        let p = t.payload(now + Duration::from_millis(1200));
        assert_eq!((p["monitorWindowChangeAgeMs"].as_u64(), p["monitorGeneration"].as_u64()), (Some(1200), Some(1)));
    }

    #[test]
    fn a_closed_tab_in_a_surviving_window_is_removed_only_if_the_tab_strip_showed_it() {
        let now = Instant::now();
        let tabs = |t: &[&str]| t.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        let docs = win(7, "Google-chrome", "Docs - Google Chrome");
        let mut t = Tracker::new(1, "abc-defg-hij", &["chrome.exe"]);
        t.observe(&[win(7, "Google-chrome", MEET)], None, now);
        t.observe(std::slice::from_ref(&docs), Some(&tabs(&["Docs"])), now); // strip never showed the call: cannot tell
        assert_eq!((t.transition, t.removed), ("unknown", 0));
        t.observe(std::slice::from_ref(&docs), Some(&tabs(&["Docs", "Meet - abc-defg-hij"])), now); // background tab
        assert_eq!(t.transition, "present");
        t.observe(std::slice::from_ref(&docs), None, now); // unreadable strip
        assert_eq!((t.transition, t.removed), ("unknown", 0));
        t.observe(std::slice::from_ref(&docs), Some(&tabs(&["Docs"])), now);
        assert_eq!((t.transition, t.removed), ("window_removed", 1));
    }

    #[test]
    fn ambiguous_private_foreign_and_incomplete_targets_never_track() {
        let now = Instant::now();
        let mut t = Tracker::new(1, "abc-defg-hij", &["chrome.exe"]);
        t.observe(&[win(1, "Google-chrome", MEET), win(2, "Chromium-browser", MEET)], None, now);
        assert_eq!((t.transition, t.tracked), ("arm_ambiguous", None));
        let mut t = Tracker::new(1, "abc-defg-hij", &["chrome.exe"]);
        t.observe(&[win(1, "Google-chrome", "Meet - abc-defg-hij - Google Chrome (Incognito)"), win(2, "firefox", MEET)], None, now);
        assert_eq!(t.tracked, None);
        for (needle, ids) in [("meet.google.com", vec![]), ("abc", vec!["chrome.exe"]), ("abc-defg-hij", vec!["firefox.exe"])] {
            let mut t = Tracker::new(1, needle, &ids);
            t.observe(&[win(1, "Google-chrome", MEET)], None, now);
            assert_eq!(t.transition, "arm_incomplete");
        }
    }
}
