// SPDX-License-Identifier: GPL-3.0-or-later
//! The key and media names a mouse button can be bound to, and the actions the scroll wheel
//! takes, for pickers in the app and for ggctl.
//!
//! Each name is one `gg-device-mouse` accepts (`ButtonAction::parse`, from rivalcfg's tables),
//! one per key: the tables' aliases are left out. Tests in `gglinuxd` check the two agree.

/// `(name the daemon takes, label to show)`, one per keyboard key, in picker order.
pub const KEYS: &[(&str, &str)] = &[
    ("a", "A"),
    ("b", "B"),
    ("c", "C"),
    ("d", "D"),
    ("e", "E"),
    ("f", "F"),
    ("g", "G"),
    ("h", "H"),
    ("i", "I"),
    ("j", "J"),
    ("k", "K"),
    ("l", "L"),
    ("m", "M"),
    ("n", "N"),
    ("o", "O"),
    ("p", "P"),
    ("q", "Q"),
    ("r", "R"),
    ("s", "S"),
    ("t", "T"),
    ("u", "U"),
    ("v", "V"),
    ("w", "W"),
    ("x", "X"),
    ("y", "Y"),
    ("z", "Z"),
    ("1", "1"),
    ("2", "2"),
    ("3", "3"),
    ("4", "4"),
    ("5", "5"),
    ("6", "6"),
    ("7", "7"),
    ("8", "8"),
    ("9", "9"),
    ("0", "0"),
    ("f1", "F1"),
    ("f2", "F2"),
    ("f3", "F3"),
    ("f4", "F4"),
    ("f5", "F5"),
    ("f6", "F6"),
    ("f7", "F7"),
    ("f8", "F8"),
    ("f9", "F9"),
    ("f10", "F10"),
    ("f11", "F11"),
    ("f12", "F12"),
    ("f13", "F13"),
    ("f14", "F14"),
    ("f15", "F15"),
    ("f16", "F16"),
    ("f17", "F17"),
    ("f18", "F18"),
    ("f19", "F19"),
    ("f20", "F20"),
    ("f21", "F21"),
    ("f22", "F22"),
    ("f23", "F23"),
    ("f24", "F24"),
    ("enter", "Enter"),
    ("escape", "Escape"),
    ("backspace", "Backspace"),
    ("tab", "Tab"),
    ("space", "Space"),
    ("capslock", "Caps Lock"),
    ("leftctrl", "Left Ctrl"),
    ("leftshift", "Left Shift"),
    ("leftalt", "Left Alt"),
    ("leftsuper", "Left Super"),
    ("rightctrl", "Right Ctrl"),
    ("rightshift", "Right Shift"),
    ("rightalt", "Right Alt"),
    ("rightsuper", "Right Super"),
    ("printscreen", "Print Screen"),
    ("scrolllock", "Scroll Lock"),
    ("pausebreak", "Pause"),
    ("insert", "Insert"),
    ("home", "Home"),
    ("pageup", "Page Up"),
    ("delete", "Delete"),
    ("end", "End"),
    ("pagedown", "Page Down"),
    ("right", "Right"),
    ("left", "Left"),
    ("down", "Down"),
    ("up", "Up"),
    ("menu", "Menu"),
    ("-", "-"),
    ("=", "="),
    ("[", "["),
    ("]", "]"),
    ("\\", "\\"),
    (";", ";"),
    ("'", "'"),
    ("`", "`"),
    (",", ","),
    (".", "."),
    ("/", "/"),
    ("\\(inter)", "\\ (ISO)"),
    ("numlock", "Num Lock"),
    ("keypad/", "Keypad /"),
    ("keypad*", "Keypad *"),
    ("keypad-", "Keypad -"),
    ("keypad+", "Keypad +"),
    ("keypadenter", "Keypad Enter"),
    ("keypad1", "Keypad 1"),
    ("keypad2", "Keypad 2"),
    ("keypad3", "Keypad 3"),
    ("keypad4", "Keypad 4"),
    ("keypad5", "Keypad 5"),
    ("keypad6", "Keypad 6"),
    ("keypad7", "Keypad 7"),
    ("keypad8", "Keypad 8"),
    ("keypad9", "Keypad 9"),
    ("keypad0", "Keypad 0"),
    ("keypad.", "Keypad ."),
    ("keypad,", "Keypad ,"),
    ("keypad=", "Keypad ="),
];

/// `(name the daemon takes, label to show)` for the media keys.
pub const MEDIA: &[(&str, &str)] = &[
    ("playpause", "Play / pause"),
    ("next", "Next track"),
    ("previous", "Previous track"),
    ("mute", "Mute"),
    ("volumeup", "Volume up"),
    ("volumedown", "Volume down"),
];

/// `(name the daemon takes, label to show)` for the scroll wheel: only the actions a capture
/// of GG shows on it (`gg_device_mouse::proto::WHEEL_ACTIONS`), in picker order.
pub const WHEEL: &[(&str, &str)] = &[
    ("scrollup", "Scroll up"),
    ("scrolldown", "Scroll down"),
    ("button4", "Back (button 4)"),
];

/// The wheel as it comes from the factory: turned up it scrolls up, turned down it scrolls
/// down. `SetMouseButtons` with these two last behaves as with six actions while the
/// `mouse_wheel` gate is off.
pub const WHEEL_FACTORY: [&str; 2] = ["scrollup", "scrolldown"];

/// The label for a key or media name (any case), if it's one of these.
pub fn label(name: &str) -> Option<&'static str> {
    let name = name.trim().to_ascii_lowercase();
    KEYS.iter()
        .chain(MEDIA)
        .find(|(n, _)| *n == name)
        .map(|(_, l)| *l)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn names_are_unique_lowercase_and_labelled() {
        let mut seen = std::collections::HashSet::new();
        for (n, l) in KEYS.iter().chain(MEDIA) {
            assert!(seen.insert(*n), "{n} twice");
            assert_eq!(*n, n.to_ascii_lowercase());
            assert!(!l.is_empty());
        }
        assert_eq!(label("F13"), Some("F13"));
        assert_eq!(label("playpause"), Some("Play / pause"));
        assert_eq!(label("nope"), None);
    }

    #[test]
    fn the_wheel_offers_its_factory_pair_first() {
        let names: Vec<&str> = WHEEL.iter().map(|(n, _)| *n).collect();
        assert_eq!(names[..2], WHEEL_FACTORY);
        assert!(names.iter().all(|n| *n == n.to_ascii_lowercase()));
    }
}
