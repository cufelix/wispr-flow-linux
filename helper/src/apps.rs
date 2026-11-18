//! GetRunningApps: top-level X11 windows (_NET_CLIENT_LIST) by WM_CLASS. Native Wayland windows are
//! invisible here (Xwayland ones show up); ponytail: add a compositor/AT-SPI source if that matters.

use serde_json::{json, Value};
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{AtomEnum, ConnectionExt};

fn x11_classes() -> Option<Vec<String>> {
    let (conn, screen) = x11rb::connect(None).ok()?;
    let root = conn.setup().roots[screen].root;
    let atom = conn.intern_atom(false, b"_NET_CLIENT_LIST").ok()?.reply().ok()?.atom;
    let list = conn.get_property(false, root, atom, AtomEnum::WINDOW, 0, 4096).ok()?.reply().ok()?;
    let mut classes = Vec::new();
    for win in list.value32()? {
        let Some(p) = conn.get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256).ok().and_then(|c| c.reply().ok()) else { continue };
        // "instance\0Class\0": the second is the application class
        if let Some(class) = p.value.split(|b| *b == 0).nth(1).filter(|c| !c.is_empty()) {
            let class = String::from_utf8_lossy(class).into_owned();
            if !classes.contains(&class) { classes.push(class); }
        }
    }
    Some(classes)
}

pub fn running() -> Value {
    let apps: Vec<Value> = x11_classes().unwrap_or_default().into_iter()
        .map(|c| json!({ "bundleId": c.to_lowercase(), "name": c })) // same bundleId convention as ActiveAppInfo
        .collect();
    json!({ "RunningApps": { "payload": { "apps": apps } } })
}
