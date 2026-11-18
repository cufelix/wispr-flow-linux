//! Global key listening. evdev works on X11 and Wayland but needs the `input` group;
//! X11 keymap polling needs no permissions. Both report evdev key codes.

use evdev::{EventType, KeyCode};
use std::time::Duration;
use x11rb::protocol::xproto::ConnectionExt;

/// (evdev code, pressed)
pub type OnKey = std::sync::Arc<dyn Fn(u16, bool) + Send + Sync>;

/// Starts listening; returns the backend name, or None if nothing is usable.
pub fn start(on_key: OnKey, wayland: bool) -> Option<&'static str> {
    if start_evdev(on_key.clone()) {
        return Some("evdev");
    }
    if !wayland && start_x11(on_key) {
        return Some("x11-poll");
    }
    None
}

fn is_keyboard_or_mouse(d: &evdev::Device) -> bool {
    d.supported_keys().is_some_and(|k| k.contains(KeyCode::KEY_LEFTCTRL) || k.contains(KeyCode::BTN_SIDE) || k.contains(KeyCode::BTN_MIDDLE))
        && d.name() != Some(crate::output::VIRTUAL_KBD_NAME)
}

/// Opens every keyboard/mouse now and keeps rescanning, so hot-plugged or reconnected
/// (Bluetooth, resume from suspend) devices are picked up.
fn start_evdev(on_key: OnKey) -> bool {
    let open = std::sync::Arc::new(std::sync::Mutex::new(std::collections::HashSet::new()));
    let scan = {
        let open = open.clone();
        move || {
            for (path, mut dev) in evdev::enumerate() {
                if !is_keyboard_or_mouse(&dev) || !open.lock().unwrap().insert(path.clone()) {
                    continue;
                }
                let (on_key, open) = (on_key.clone(), open.clone());
                std::thread::spawn(move || {
                    while let Ok(events) = dev.fetch_events() {
                        for ev in events {
                            // value: 0 release, 1 press, 2 autorepeat (ignored)
                            if ev.event_type() == EventType::KEY && ev.value() < 2 {
                                on_key(ev.code(), ev.value() == 1);
                            }
                        }
                    }
                    open.lock().unwrap().remove(&path); // unplugged: allow reopening later
                });
            }
        }
    };
    scan();
    if open.lock().unwrap().is_empty() {
        return false; // no permission (not in the `input` group) or no devices
    }
    std::thread::spawn(move || loop {
        std::thread::sleep(Duration::from_secs(3));
        scan();
    });
    true
}

fn start_x11(on_key: OnKey) -> bool {
    let Ok((conn, _)) = x11rb::connect(None) else { return false };
    std::thread::spawn(move || {
        let mut prev = [0u8; 32];
        loop {
            // ponytail: 10 ms polling of the keymap; switch to XInput2 raw events if latency/CPU matters.
            std::thread::sleep(Duration::from_millis(10));
            let Ok(Ok(reply)) = conn.query_keymap().map(|c| c.reply()) else { return };
            for (byte, (&now, &before)) in reply.keys.iter().zip(prev.iter()).enumerate() {
                let changed = now ^ before;
                for bit in (0..8).filter(|b| changed & (1 << b) != 0) {
                    let x_code = (byte * 8 + bit) as u16;
                    if x_code >= 8 {
                        on_key(x_code - 8, now & (1 << bit) != 0);
                    }
                }
            }
            prev = reply.keys;
        }
    });
    true
}
