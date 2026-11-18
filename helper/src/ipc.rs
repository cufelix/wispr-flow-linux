//! Wire format used by Wispr Flow's helper pipe: JSON documents, '+' escaped as "+1",
//! '|' escaped as "+2", each message terminated by '|'.

use serde_json::{json, Value};
use std::fs::File;
use std::io::Write;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// Helper -> Electron messages go to fd 3 (Electron reads stdout only as log noise).
/// Falls back to stdout when run by hand without fd 3.
static OUT: Mutex<Option<File>> = Mutex::new(None);
static NEXT_ID: AtomicU64 = AtomicU64::new(1);

pub fn escape(s: &str) -> String {
    s.replace('+', "+1").replace('|', "+2")
}

pub fn unescape(s: &str) -> String {
    s.replace("+2", "|").replace("+1", "+")
}

/// Must run first in main(), before anything else opens a file descriptor that could become fd 3.
pub fn init() {
    // Electron passes fd 3 as a socket, which cannot be reopened via /proc; adopt it directly.
    if std::fs::metadata("/proc/self/fd/3").is_ok() {
        use std::os::fd::FromRawFd;
        // SAFETY: at startup fd 3 can only be the pipe inherited from Electron; nothing else owns it.
        *OUT.lock().unwrap() = Some(unsafe { File::from_raw_fd(3) });
    }
}

fn write(msg: &Value) {
    let mut frame = escape(&msg.to_string());
    frame.push('|');
    let mut guard = OUT.lock().unwrap();
    match guard.as_mut() {
        Some(fd3) => { let _ = fd3.write_all(frame.as_bytes()); }
        None => { let mut o = std::io::stdout().lock(); let _ = o.write_all(frame.as_bytes()); let _ = o.flush(); }
    }
}

/// Reply to a request from Electron: `body` holds the response variant(s), e.g. {"ACK": true}.
pub fn respond(uuid: &str, mut body: Value) {
    body["uuid"] = json!(uuid);
    write(&json!({ "HelperAPIResponse": body }));
}

/// Send a request to Electron, e.g. request("KeypressEvent", json!({"payload": {...}})).
pub fn request(kind: &str, value: Value) {
    let uuid = format!("linux-{:016x}", NEXT_ID.fetch_add(1, Ordering::Relaxed));
    write(&json!({ "HelperAPIRequest": { "uuid": uuid, kind: value } }));
}

/// The request type is the one key that is not uuid / sentry metadata.
pub fn request_kind(req: &Value) -> Option<&str> {
    req.as_object()?
        .keys()
        .map(String::as_str)
        .find(|k| !matches!(*k, "uuid" | "sentryTrace" | "sentryBaggage"))
}

/// Splits a stdin buffer into complete messages, leaving any partial tail in `buf`.
pub fn drain_messages(buf: &mut String) -> Vec<String> {
    let mut msgs = Vec::new();
    while let Some(pos) = buf.find('|') {
        let raw: String = buf.drain(..=pos).collect();
        msgs.push(unescape(&raw[..raw.len() - 1]));
    }
    msgs
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn escape_roundtrip() {
        let s = r#"{"text":"a+b|c ++ ||"}"#;
        assert_eq!(unescape(&escape(s)), s);
        assert!(!escape(s).contains('|'));
    }

    #[test]
    fn splits_stream() {
        let mut buf = format!("{}|{}|{{\"par", escape("{\"a\":\"x|y\"}"), escape("{}"));
        let msgs = drain_messages(&mut buf);
        assert_eq!(msgs, vec!["{\"a\":\"x|y\"}", "{}"]);
        assert_eq!(buf, "{\"par");
    }

    #[test]
    fn finds_kind() {
        let v = json!({"uuid": "1", "sentryTrace": "t", "PasteText": {}});
        assert_eq!(request_kind(&v), Some("PasteText"));
    }
}
