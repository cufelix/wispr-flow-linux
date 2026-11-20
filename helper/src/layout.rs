//! Letter keys by the active keyboard layout (X11 only), as Windows reports them: on QWERTZ the key
//! labelled Z is VK_Z, on AZERTY the key labelled A is VK_A. Wayland has no way to ask; keys.rs's
//! US positions are used there.

use std::sync::Mutex;
use std::time::{Duration, Instant};
use x11rb::protocol::xkb::{ConnectionExt, ID, KeySymMap, MapPart, VMod};
use x11rb::rust_connection::RustConnection;

struct Layout {
    conn: RustConnection,
    first: u8,
    syms: Vec<KeySymMap>,
    fetched: Option<Instant>,
}

static LAYOUT: Mutex<Option<Layout>> = Mutex::new(None);
const REFRESH: Duration = Duration::from_secs(5); // layouts can be added or removed while we run

pub fn init() {
    let Ok((conn, _)) = x11rb::connect(None) else { return };
    if conn.xkb_use_extension(1, 0).ok().and_then(|c| c.reply().ok()).is_some_and(|r| r.supported) {
        *LAYOUT.lock().unwrap() = Some(Layout { conn, first: 0, syms: Vec::new(), fetched: None });
    }
}

/// The VK code for evdev key `code` under the active layout, if the layout moves it. `position_vk` is its
/// VK on a US keyboard.
pub fn vk(code: u16, position_vk: u16) -> u16 {
    if !moves(position_vk) {
        return position_vk; // modifiers, space, digits...: no X round trip on the key thread
    }
    let mut guard = LAYOUT.lock().unwrap();
    let Some(l) = guard.as_mut() else { return position_vk };
    if l.fetched.is_none_or(|t| t.elapsed() > REFRESH) {
        let dev = u16::from(ID::USE_CORE_KBD);
        let reply = l.conn.xkb_get_map(dev, MapPart::KEY_SYMS, MapPart::from(0u16), 0, 0, 0, 0, 0, 0, 0, 0, VMod::from(0u16), 0, 0, 0, 0, 0, 0)
            .ok().and_then(|c| c.reply().ok());
        if let Some(r) = reply {
            l.first = r.first_key_sym;
            l.syms = r.map.syms_rtrn.unwrap_or_default();
        }
        l.fetched = Some(Instant::now());
    }
    let group = l.conn.xkb_get_state(u16::from(ID::USE_CORE_KBD)).ok().and_then(|c| c.reply().ok()).map_or(0, |s| u8::from(s.group));
    let sym = (code + 8).checked_sub(l.first as u16).and_then(|i| l.syms.get(i as usize)).and_then(|m| level0(m, group));
    sym.map_or(position_vk, |s| translate(s, position_vk))
}

/// The unshifted keysym of a key in `group` (groups a key does not have fall back to its first).
fn level0(map: &KeySymMap, group: u8) -> Option<u32> {
    let groups = map.group_info & 0x0f;
    let g = if group < groups { group } else { 0 };
    (groups > 0).then(|| map.syms.get(g as usize * map.width as usize).copied()).flatten()
}

/// Keys whose VK code depends on the layout: letters and punctuation.
fn moves(position_vk: u16) -> bool {
    (65..=90).contains(&position_vk) || matches!(position_vk, 186..=192 | 219..=222)
}

/// Latin letters and the punctuation Windows has VK codes for follow the layout; everything else
/// (digits row, Cyrillic, Greek...) keeps its US position, which is also what Windows does for non-Latin layouts.
fn translate(sym: u32, position_vk: u16) -> u16 {
    if !moves(position_vk) {
        return position_vk;
    }
    match char::from_u32(sym) {
        Some(c @ 'a'..='z') => c as u16 - 32,
        Some(';') => 186, Some('=') => 187, Some(',') => 188, Some('-') => 189, Some('.') => 190, Some('/') => 191,
        Some('`') => 192, Some('[') => 219, Some('\\') => 220, Some(']') => 221, Some('\'') => 222,
        _ => position_vk,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn letters_follow_the_layout() {
        assert_eq!(translate('z' as u32, 89), 90); // QWERTZ: the US Y position types z
        assert_eq!(translate('a' as u32, 81), 65); // AZERTY: the US Q position types a
        assert_eq!(translate('m' as u32, 186), 77); // AZERTY: m sits on the US ; key
        assert_eq!(translate(',' as u32, 77), 188); // ...and , on the US M key
        assert_eq!(translate(0x6c6, 65), 65); // Cyrillic ef on the A key: keeps the position
        assert_eq!(translate('z' as u32, 32), 32); // space never moves
    }

    #[test]
    fn picks_the_active_group() {
        let key = KeySymMap { kt_index: [0; 4], group_info: 3, width: 2, syms: vec![121, 89, 121, 89, 122, 90] };
        assert_eq!(level0(&key, 0), Some(121)); // us: y
        assert_eq!(level0(&key, 2), Some(122)); // de: z
        assert_eq!(level0(&key, 3), Some(121)); // a group this key lacks
        assert_eq!(level0(&KeySymMap { kt_index: [0; 4], group_info: 0, width: 0, syms: vec![] }, 0), None);
    }
}
