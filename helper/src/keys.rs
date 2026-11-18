//! Linux evdev key codes -> Windows virtual-key codes.
//! Wispr Flow's Electron side uses Windows VK codes on every non-macOS platform.
//! X11 keycodes are evdev codes + 8.

pub fn evdev_to_vk(code: u16) -> Option<u16> {
    let vk = match code {
        1 => 27,                       // Esc
        2..=10 => 49 + (code - 2),     // 1..9
        11 => 48,                      // 0
        12 => 189, 13 => 187,          // - =
        14 => 8, 15 => 9,              // Backspace Tab
        16 => 81, 17 => 87, 18 => 69, 19 => 82, 20 => 84, 21 => 89, 22 => 85, 23 => 73, 24 => 79, 25 => 80, // q..p
        26 => 219, 27 => 221, 28 => 13, // [ ] Enter
        29 => 162,                     // Left Ctrl
        30 => 65, 31 => 83, 32 => 68, 33 => 70, 34 => 71, 35 => 72, 36 => 74, 37 => 75, 38 => 76, // a..l
        39 => 186, 40 => 222, 41 => 192, // ; ' `
        42 => 160, 43 => 220,          // Left Shift, backslash
        44 => 90, 45 => 88, 46 => 67, 47 => 86, 48 => 66, 49 => 78, 50 => 77, // z..m
        51 => 188, 52 => 190, 53 => 191, // , . /
        54 => 161, 55 => 106, 56 => 164, 57 => 32, 58 => 20, // RShift, KP*, LAlt, Space, CapsLock
        59..=68 => 112 + (code - 59),  // F1..F10
        69 => 144, 70 => 145,          // NumLock ScrollLock
        71 => 103, 72 => 104, 73 => 105, 74 => 109, 75 => 100, 76 => 101, 77 => 102, 78 => 107,
        79 => 97, 80 => 98, 81 => 99, 82 => 96, 83 => 110, // keypad
        86 => 226,                     // ISO < >
        87 => 122, 88 => 123,          // F11 F12
        96 => 13, 97 => 163, 98 => 111, 99 => 44, 100 => 165, // KPEnter RCtrl KP/ PrintScr RAlt
        102 => 36, 103 => 38, 104 => 33, 105 => 37, 106 => 39, 107 => 35, 108 => 40, 109 => 34,
        110 => 45, 111 => 46,          // Insert Delete
        119 => 19,                     // Pause
        125 => 91, 126 => 92, 127 => 93, // LWin RWin Menu
        183..=194 => 124 + (code - 183), // F13..F24
        _ => return None,
    };
    Some(vk)
}

/// Windows virtual-key code -> evdev code (for SimulateKeyPress). Generic Ctrl/Shift/Alt map to the left key.
pub fn vk_to_evdev(vk: u16) -> Option<u16> {
    match vk {
        16 => Some(42),
        17 => Some(29),
        18 => Some(56),
        _ => (1..=194).find(|&c| evdev_to_vk(c) == Some(vk)),
    }
}

/// SimulateKeyPress modifier names -> evdev code.
pub fn modifier_to_evdev(flag: &str) -> Option<u16> {
    match flag.to_ascii_lowercase().as_str() {
        "control" | "ctrl" => Some(29),
        "shift" => Some(42),
        "alt" | "option" => Some(56),
        "command" | "meta" | "super" | "win" => Some(125),
        _ => None,
    }
}

/// The lowercase ASCII letter an evdev letter key types on QWERTY (its X keysym).
pub fn evdev_letter(code: u16) -> Option<char> {
    let vk = evdev_to_vk(code)?;
    (65..=90).contains(&vk).then(|| (vk as u8 + 32) as char)
}

/// Mouse buttons beyond left/right as Wispr "mouse" input. Wispr's table (OS button number -> shortcut key):
/// 2 middle, 3 mouse4 (back), 4 mouse5 (forward), 5..9 mouse6..10. evdev BTN_MIDDLE..BTN_TASK+ are consecutive.
pub fn evdev_mouse_button(code: u16) -> Option<u16> {
    (0x112..=0x119).contains(&code).then(|| code - 0x110)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn maps_default_shortcut_keys() {
        assert_eq!(evdev_to_vk(29), Some(162)); // ctrl
        assert_eq!(evdev_to_vk(125), Some(91)); // win
        assert_eq!(evdev_to_vk(57), Some(32)); // space
        assert_eq!(evdev_to_vk(30), Some(65)); // a
        assert_eq!(evdev_to_vk(11), Some(48)); // 0
        assert_eq!(evdev_to_vk(68), Some(121)); // F10
        assert_eq!(evdev_to_vk(194), Some(135)); // F24
        assert_eq!(evdev_to_vk(240), None);
    }

    #[test]
    fn maps_mouse_buttons() {
        assert_eq!(evdev_mouse_button(0x112), Some(2)); // middle
        assert_eq!(evdev_mouse_button(0x113), Some(3)); // BTN_SIDE = mouse4
        assert_eq!(evdev_mouse_button(0x114), Some(4)); // BTN_EXTRA = mouse5
        assert_eq!(evdev_mouse_button(0x119), Some(9));
        assert_eq!(evdev_mouse_button(0x110), None); // left click
        assert_eq!(evdev_mouse_button(0x11a), None);
    }

    #[test]
    fn maps_simulated_keys_back() {
        assert_eq!(vk_to_evdev(13), Some(28)); // Enter, not keypad Enter
        assert_eq!(vk_to_evdev(37), Some(105)); // left
        assert_eq!(vk_to_evdev(38), Some(103)); // up
        assert_eq!(vk_to_evdev(65), Some(30)); // a
        assert_eq!(vk_to_evdev(17), Some(29)); // ctrl
        assert_eq!(modifier_to_evdev("Control"), Some(29));
        assert_eq!(modifier_to_evdev("Shift"), Some(42));
        assert_eq!(evdev_letter(44), Some('z'));
        assert_eq!(evdev_letter(28), None);
    }
}
