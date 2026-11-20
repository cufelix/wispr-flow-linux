//! Speaker names for meeting transcripts: who is talking, read from the Google Meet page over AT-SPI.
//! Wispr sends StartSpeakerPolling with the DOM class names of a participant tile and of its "speaking" marker;
//! we find the Meet tab in any non-private browser window, read the tiles and report MeetingSpeakerChange.

use crate::{a11y, ipc};
use serde_json::{json, Value};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const TICK: Duration = Duration::from_millis(500);
/// Electron treats a speaker event older than 1.5 s as stale, so a speaker who keeps talking is re-reported.
const HEARTBEAT: Duration = Duration::from_millis(1000);
const DISCOVERY_EVERY: u32 = 4; // ticks between searches for the Meet tab
const NODE_LIMIT: usize = 4000;
const WALK_BUDGET: Duration = Duration::from_millis(400);
const ROLES_WITHOUT_NAMES: [u32; 3] = [35, 43, 62]; // menu item, push button, toggle button: "Pin", "Mute"... not a person

/// Bumped by every start and stop; a polling thread ends once it is no longer the current one.
static GENERATION: AtomicU64 = AtomicU64::new(0);

struct Config {
    tile_classes: Vec<String>,
    marker_classes: Vec<String>,
}

pub fn start(payload: &Value) {
    let list = |k: &str| payload[k].as_array().into_iter().flatten().filter_map(|v| v.as_str().map(str::to_owned)).collect::<Vec<_>>();
    let platform = payload["platform"].as_str().unwrap_or_default();
    let gen_ = GENERATION.fetch_add(1, Ordering::SeqCst) + 1;
    // ponytail: Meet only; Teams and Slack huddles in a browser use other markers nobody has verified here
    if platform != "google_meet" || payload["collectSpeakerNames"].as_bool() != Some(true) {
        return;
    }
    let cfg = Config { tile_classes: list("tileRootClasses"), marker_classes: list("markerClasses") };
    std::thread::spawn(move || poll(gen_, &cfg));
}

pub fn stop() {
    GENERATION.fetch_add(1, Ordering::SeqCst);
}

#[derive(Clone)]
struct Raw {
    classes: String,
    role: u32,
    name: String,
}

#[derive(Debug, PartialEq)]
struct Tile {
    name: Option<String>,
    speaking: bool,
}

fn has_class(attr: &str, wanted: &[String]) -> bool {
    attr.split_whitespace().any(|c| wanted.iter().any(|w| w == c))
}

/// One participant tile from its nodes (the tile root first, then its descendants in document order).
/// The name is the first named node that is not a button; speaking is any node carrying a marker class.
// ponytail: name = first text under the tile; a Meet layout that puts a label before the name needs a class for it
fn read_tile(nodes: &[Raw], markers: &[String]) -> Tile {
    let name = nodes.iter().skip(1).find(|n| !n.name.trim().is_empty() && !ROLES_WITHOUT_NAMES.contains(&n.role))
        .map(|n| n.name.split_whitespace().collect::<Vec<_>>().join(" "));
    Tile { name, speaking: nodes.iter().any(|n| has_class(&n.classes, markers)) }
}

/// What Wispr calls attribution: one speaking tile is a match, several are ambiguous, none is unknown.
fn attribution(speaking: &[String]) -> (&'static str, Option<&str>) {
    match speaking {
        [] => ("unknown", None),
        [one] => ("matched", Some(one)),
        _ => ("ambiguous", None),
    }
}

fn attributes(node: &a11y::Target) -> std::collections::HashMap<String, String> {
    a11y::call(node, a11y::ACCESSIBLE, "GetAttributes", &()).and_then(|m| m.body().deserialize().ok()).unwrap_or_default()
}

fn raw(node: &a11y::Target, with_name: bool) -> Option<Raw> {
    let role: u32 = a11y::call(node, a11y::ACCESSIBLE, "GetRole", &())?.body().deserialize().ok()?;
    if role == a11y::ROLE_PASSWORD_TEXT {
        return None; // never read password fields
    }
    // name, or the text of a text node (Firefox leaves the name of plain text empty)
    let name = if with_name {
        let name: String = a11y::prop(node, a11y::ACCESSIBLE, "Name").unwrap_or_default();
        if name.trim().is_empty() {
            let count: i32 = a11y::prop(node, "org.a11y.atspi.Text", "CharacterCount").unwrap_or(0);
            let m = a11y::call(node, "org.a11y.atspi.Text", "GetText", &(0i32, count.clamp(0, 200)));
            m.and_then(|m| m.body().deserialize().ok()).unwrap_or_default()
        } else {
            name
        }
    } else {
        String::new()
    };
    let name = name.replace('\u{fffc}', " ");
    Some(Raw { classes: attributes(node).remove("class").unwrap_or_default(), role, name })
}

/// Participant tiles under the Meet document. None when the document is gone.
fn read_tiles(doc: &a11y::Target, cfg: &Config) -> Option<Vec<Tile>> {
    a11y::state_bits(doc)?;
    let deadline = Instant::now() + WALK_BUDGET;
    let (mut stack, mut seen, mut tiles) = (vec![doc.clone()], 0, Vec::new());
    while let Some(node) = stack.pop() {
        seen += 1;
        if seen > NODE_LIMIT || Instant::now() > deadline {
            break;
        }
        let Some(r) = raw(&node, false) else { continue };
        if !has_class(&r.classes, &cfg.tile_classes) {
            stack.extend(a11y::children(&node).into_iter().rev());
            continue;
        }
        // a tile: read its whole subtree, nested tiles included in it
        let (mut nodes, mut sub) = (vec![Raw { name: String::new(), ..r }], a11y::children(&node));
        sub.reverse();
        while let Some(n) = sub.pop() {
            seen += 1;
            if seen > NODE_LIMIT || Instant::now() > deadline {
                break;
            }
            if let Some(r) = raw(&n, true) {
                nodes.push(r);
                let mut kids = a11y::children(&n);
                kids.reverse();
                sub.extend(kids);
            }
        }
        tiles.push(read_tile(&nodes, &cfg.marker_classes));
    }
    Some(tiles)
}

fn now_ms() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_millis() as u64)
}

fn source_report(readable: bool) {
    let sources: &[&str] = if readable { &["main_window"] } else { &[] };
    ipc::request("MeetingSpeakerSourceReport", json!({ "payload": { "availableSources": sources, "platform": "google_meet",
        "readStatus": if readable { "readable" } else { "unreadable" },
        "source": if readable { "main_window" } else { "none" }, "timestamp": now_ms() } }));
}

fn speaker_change(speaking: &[String], participants: &[String]) {
    let (state, name) = attribution(speaking);
    let mut payload = json!({ "attributionState": state, "discoveryTier": "anchored", "source": "meet_tab",
        "speakingNames": speaking, "participantNames": participants, "timestamp": now_ms() });
    if let Some(n) = name {
        payload["speakerName"] = json!(n);
    }
    ipc::request("MeetingSpeakerChange", json!({ "payload": payload }));
}

fn find_meet() -> Option<a11y::Target> {
    // ponytail: any page on meet.google.com counts, including the lobby and the landing page (no tiles there)
    // WISPR_LINUX_MEET_URL is a test hook: point it at a local page that mimics Meet
    let prefix = std::env::var("WISPR_LINUX_MEET_URL").unwrap_or_else(|_| "https://meet.google.com/".into());
    a11y::browser_documents().into_iter().find(|(_, url)| url.strip_prefix(prefix.as_str()).is_some_and(|p| !p.is_empty())).map(|(t, _)| t)
}

fn poll(gen_: u64, cfg: &Config) {
    let (mut doc, mut readable) = (None::<a11y::Target>, None::<bool>);
    let (mut last, mut last_sent) = (None::<(Vec<String>, Vec<String>)>, Instant::now());
    let mut tick = 0u32;
    while GENERATION.load(Ordering::SeqCst) == gen_ {
        if doc.is_none() && tick.is_multiple_of(DISCOVERY_EVERY) {
            doc = find_meet();
        }
        tick = tick.wrapping_add(1);
        let tiles = doc.as_ref().and_then(|d| read_tiles(d, cfg)).filter(|t| !t.is_empty());
        if tiles.is_none() {
            doc = None; // gone, or not in a call: search again
        }
        let now_readable = tiles.is_some();
        if readable != Some(now_readable) {
            readable = Some(now_readable);
            source_report(now_readable);
        }
        if let Some(tiles) = tiles {
            let mut participants: Vec<String> = tiles.iter().filter_map(|t| t.name.clone()).collect();
            let mut speaking: Vec<String> = tiles.iter().filter(|t| t.speaking).filter_map(|t| t.name.clone()).collect();
            participants.sort();
            participants.dedup();
            speaking.sort();
            speaking.dedup();
            let key = (speaking, participants);
            // an unchanged speaker is re-sent, an unchanged silence is not
            if last.as_ref() != Some(&key) || (!key.0.is_empty() && last_sent.elapsed() >= HEARTBEAT) {
                speaker_change(&key.0, &key.1);
                last_sent = Instant::now();
                last = Some(key);
            }
        }
        std::thread::sleep(TICK);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn raw(classes: &str, role: u32, name: &str) -> Raw {
        Raw { classes: classes.into(), role, name: name.into() }
    }
    fn s(v: &[&str]) -> Vec<String> {
        v.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn reads_name_and_speaking_from_a_tile() {
        let markers = s(&["kssMZb"]);
        let tile = [raw("oZRSLe", 0, ""), raw("x", 43, "Pin"), raw("zWGUib", 0, " Anna   Novak "), raw("a kssMZb", 0, "")];
        assert_eq!(read_tile(&tile, &markers), Tile { name: Some("Anna Novak".into()), speaking: true });
        assert!(!read_tile(&tile[..3], &markers).speaking);
        assert_eq!(read_tile(&tile[..2], &markers).name, None);
    }

    #[test]
    fn attributes_one_speaker_only() {
        assert_eq!(attribution(&[]), ("unknown", None));
        assert_eq!(attribution(&s(&["Anna"])), ("matched", Some("Anna")));
        assert_eq!(attribution(&s(&["Anna", "Tom"])), ("ambiguous", None));
    }
}
