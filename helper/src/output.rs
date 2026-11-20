//! Text insertion (clipboard + paste shortcut) and focused-app context.

use anyhow::{bail, Context, Result};
use evdev::{uinput::VirtualDevice, AttributeSet, InputEvent, KeyCode};
use std::time::Duration;
use x11rb::connection::Connection;
use x11rb::protocol::xproto::{Atom, AtomEnum, ConnectionExt, Window};
use x11rb::protocol::xtest::ConnectionExt as _;
use x11rb::rust_connection::RustConnection;

pub const VIRTUAL_KBD_NAME: &str = "wispr-flow-linux-helper";

const TERMINALS: &[&str] = &[
    "terminal", "konsole", "xterm", "urxvt", "alacritty", "kitty", "wezterm", "tilix",
    "terminator", "foot", "ghostty", "termite", "st-256color", "guake", "yakuake", "tabby",
];

pub fn is_terminal(app_class: &str) -> bool {
    let c = app_class.to_lowercase();
    TERMINALS.iter().any(|t| c.contains(t))
}

#[derive(Clone, Default, PartialEq, Debug)]
pub struct AppInfo {
    pub class: String,
    pub title: String,
    pub pid: Option<u32>,
}

pub struct Output {
    clipboard: arboard::Clipboard,
    x11: Option<(RustConnection, Window)>,
    uinput: Option<VirtualDevice>,
}

impl Output {
    pub fn new(wayland: bool) -> Result<Self> {
        let clipboard = arboard::Clipboard::new().context("no clipboard (X11/Wayland data-control)")?;
        let x11 = if wayland { None } else { x11rb::connect(None).ok().map(|(c, s)| { let root = c.setup().roots[s].root; (c, root) }) };
        // ponytail: uinput sends physical keys; on non-QWERTY layouts under Wayland letters may move.
        let uinput = if x11.is_some() { None } else { virtual_keyboard().ok() };
        if x11.is_none() && uinput.is_none() {
            bail!("cannot send keystrokes: no X11 and no /dev/uinput access (add a udev rule, see README)");
        }
        Ok(Self { clipboard, x11, uinput })
    }

    /// Puts `text` on the clipboard and presses Ctrl+V (Ctrl+Shift+V in terminals).
    pub fn paste(&mut self, text: &str) -> Result<()> {
        self.clipboard.set_text(text)?;
        std::thread::sleep(Duration::from_millis(30)); // let the clipboard owner settle
        let shift = is_terminal(&self.active_app().class);
        let mut chord = vec![KeyCode::KEY_LEFTCTRL];
        if shift {
            chord.push(KeyCode::KEY_LEFTSHIFT);
        }
        chord.push(KeyCode::KEY_V);
        self.press_chord(&chord)
    }

    /// Copies the selection and restores the clipboard. Empty when nothing is selected.
    /// This is what Wispr does on Windows for command mode. `app` is the focused app's name.
    pub fn copy_selection(&mut self, app: &str) -> Result<String> {
        let saved = self.clipboard.get_text().ok();
        self.clipboard.clear()?;
        let chord: &[KeyCode] = if app.is_empty() {
            &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_INSERT] // unknown app: never risk Ctrl+C (SIGINT) in a terminal
        } else if is_terminal(app) {
            &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_LEFTSHIFT, KeyCode::KEY_C]
        } else {
            &[KeyCode::KEY_LEFTCTRL, KeyCode::KEY_C]
        };
        let pressed = self.press_chord(chord);
        let mut text = String::new();
        for _ in 0..20 {
            if pressed.is_err() { break; }
            std::thread::sleep(Duration::from_millis(25));
            if let Ok(t) = self.clipboard.get_text() && !t.is_empty() {
                text = t;
                break;
            }
        }
        // ponytail: restores text only; images/files on the clipboard are lost (needs per-MIME copy).
        match saved {
            Some(old) => self.clipboard.set_text(old)?,
            None => self.clipboard.clear()?,
        }
        pressed.map(|_| text)
    }

    pub fn press_chord(&mut self, keys: &[KeyCode]) -> Result<()> {
        if let Some((conn, root)) = &self.x11 {
            // X11 keycode = evdev code + 8; letters follow the active layout (Dvorak, AZERTY...)
            let code = |k: &KeyCode| {
                let sym = crate::keys::evdev_letter(k.code()).map(|c| c as u32);
                sym.and_then(|s| keycode_for_keysym(conn, s).ok().flatten()).unwrap_or((k.code() + 8) as u8)
            };
            for k in keys {
                conn.xtest_fake_input(2, code(k), x11rb::CURRENT_TIME, *root, 0, 0, 0)?;
            }
            for k in keys.iter().rev() {
                conn.xtest_fake_input(3, code(k), x11rb::CURRENT_TIME, *root, 0, 0, 0)?;
            }
            conn.flush()?;
        } else if let Some(dev) = &mut self.uinput {
            let ev = |k: &KeyCode, v| InputEvent::new(evdev::EventType::KEY.0, k.code(), v);
            dev.emit(&keys.iter().map(|k| ev(k, 1)).collect::<Vec<_>>())?;
            dev.emit(&keys.iter().rev().map(|k| ev(k, 0)).collect::<Vec<_>>())?;
        }
        Ok(())
    }

    pub fn active_app(&self) -> AppInfo {
        self.x11.as_ref().and_then(|(c, root)| active_x11_app(c, *root).ok()).unwrap_or_default()
    }
}

fn virtual_keyboard() -> Result<VirtualDevice> {
    let mut keys = AttributeSet::<KeyCode>::new();
    for code in 1..=194 {
        keys.insert(KeyCode::new(code)); // everything SimulateKeyPress may send
    }
    let dev = VirtualDevice::builder()?.name(VIRTUAL_KBD_NAME).with_keys(&keys)?.build()?;
    std::thread::sleep(Duration::from_millis(200)); // compositor needs a moment to pick up a new device
    Ok(dev)
}

fn keycode_for_keysym(conn: &RustConnection, sym: u32) -> Result<Option<u8>> {
    let (min, max) = (conn.setup().min_keycode, conn.setup().max_keycode);
    let map = conn.get_keyboard_mapping(min, max - min + 1)?.reply()?;
    let per = map.keysyms_per_keycode as usize;
    // only the unshifted column, so we never need an extra modifier
    Ok(map.keysyms.chunks(per).position(|syms| syms.first() == Some(&sym)).map(|i| min + i as u8))
}

fn atom(conn: &RustConnection, name: &str) -> Result<Atom> {
    Ok(conn.intern_atom(false, name.as_bytes())?.reply()?.atom)
}

fn active_x11_app(conn: &RustConnection, root: Window) -> Result<AppInfo> {
    let active = conn
        .get_property(false, root, atom(conn, "_NET_ACTIVE_WINDOW")?, AtomEnum::WINDOW, 0, 1)?
        .reply()?
        .value32()
        .and_then(|mut v| v.next())
        .context("no active window")?;
    window_info(conn, active)
}

fn window_info(conn: &RustConnection, win: Window) -> Result<AppInfo> {
    let class_raw = conn.get_property(false, win, AtomEnum::WM_CLASS, AtomEnum::STRING, 0, 256)?.reply()?.value;
    // WM_CLASS = "instance\0Class\0"
    let class = class_raw.split(|&b| b == 0).rfind(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).unwrap_or_default();
    let utf8 = atom(conn, "UTF8_STRING")?;
    let title_raw = conn.get_property(false, win, atom(conn, "_NET_WM_NAME")?, utf8, 0, 1024)?.reply()?.value;
    let pid = conn.get_property(false, win, atom(conn, "_NET_WM_PID")?, AtomEnum::CARDINAL, 0, 1)?
        .reply()?.value32().and_then(|mut v| v.next());
    Ok(AppInfo { class, title: String::from_utf8_lossy(&title_raw).into_owned(), pid })
}

/// Every managed top-level window (window id, class/title/pid), from the window manager's client list.
pub fn client_windows(conn: &RustConnection, root: Window) -> Result<Vec<(Window, AppInfo)>> {
    let ids: Vec<Window> = conn
        .get_property(false, root, atom(conn, "_NET_CLIENT_LIST")?, AtomEnum::WINDOW, 0, 4096)?
        .reply()?
        .value32()
        .context("no _NET_CLIENT_LIST")?
        .collect();
    Ok(ids.into_iter().filter_map(|w| window_info(conn, w).ok().map(|a| (w, a))).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn detects_terminals() {
        assert!(is_terminal("Gnome-terminal"));
        assert!(is_terminal("kitty"));
        assert!(!is_terminal("firefox"));
        assert!(!is_terminal(""));
    }
}
