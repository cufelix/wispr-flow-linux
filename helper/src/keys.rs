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

/// Mouse side buttons, reported as Wispr "mouse" input (BTN_SIDE/BTN_EXTRA).
pub fn evdev_mouse_button(code: u16) -> Option<u16> {
    match code {
        0x113 => Some(4), // mouse4 / back
        0x114 => Some(5), // mouse5 / forward
        _ => None,
    }
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
}
