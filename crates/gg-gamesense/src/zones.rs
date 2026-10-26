//! GameSense zone names → HID codes on the Apex Pro TKL Gen 3, in zone
//! order: left to right, then top to bottom.

use gg_device_keyboard::layout::ANSI_TKL;

/// Whether the TKL has a key with this HID code.
pub fn on_board(code: u8) -> bool {
    ANSI_TKL.iter().any(|k| k.0 == code)
}

/// The keys of a named zone, or a single named key, in zone order. Zones and
/// keys this keyboard lacks (keypad, macro keys, …) are empty.
pub fn named(zone: &str) -> Vec<u8> {
    match zone {
        "function-keys" => ordered(|c, _, _| (0x3A..=0x45).contains(&c)),
        "number-keys" => ordered(|c, _, _| (0x1E..=0x27).contains(&c)),
        "q-row" => vec![0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13],
        "a-row" => vec![0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F],
        "z-row" => vec![0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10],
        // Number row to bottom row, left of the navigation cluster.
        "main-keyboard" => ordered(|_, x, y| y >= 1.5 && x < 15.0),
        "nav-cluster" => ordered(|c, _, _| (0x49..=0x4E).contains(&c)),
        "arrows" => ordered(|c, _, _| (0x4F..=0x52).contains(&c)),
        "all" => ordered(|_, _, _| true),
        _ => key(zone).into_iter().filter(|&c| on_board(c)).collect(),
    }
}

/// Keys that pass `keep(code, x, y)`, top row first, left to right within a row.
fn ordered(keep: impl Fn(u8, f32, f32) -> bool) -> Vec<u8> {
    let mut keys: Vec<(f32, f32, u8)> = ANSI_TKL
        .iter()
        .filter(|&&(c, _, x, y, _)| keep(c, x, y))
        .map(|&(c, _, x, y, _)| (y, x, c))
        .collect();
    keys.sort_by(|a, b| a.0.total_cmp(&b.0).then(a.1.total_cmp(&b.1)));
    keys.into_iter().map(|(_, _, c)| c).collect()
}

/// One key by its GameSense name, as a HID usage code.
fn key(name: &str) -> Option<u8> {
    if let [b] = name.as_bytes() {
        if b.is_ascii_lowercase() {
            return Some(0x04 + (b - b'a'));
        }
    }
    if let Some(digit) = name
        .strip_prefix("keyboard-")
        .and_then(|d| d.parse::<u8>().ok())
    {
        return match digit {
            0 => Some(0x27),
            1..=9 => Some(0x1E + digit - 1),
            _ => None,
        };
    }
    if let Some(f) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=12).contains(&f).then(|| 0x3A + f - 1);
    }
    if name == "win-menu" {
        return ANSI_TKL.iter().find(|k| k.1 == "Menu").map(|k| k.0); // whatever code the layout gives it
    }
    Some(match name {
        "return" => 0x28,
        "escape" => 0x29,
        "backspace" => 0x2A,
        "tab" => 0x2B,
        "spacebar" => 0x2C,
        "dash" => 0x2D,
        "equal" => 0x2E,
        "l-bracket" => 0x2F,
        "r-bracket" => 0x30,
        "backslash" => 0x31,
        "pound" => 0x32,
        "semicolon" => 0x33,
        "quote" => 0x34,
        "backquote" => 0x35,
        "comma" => 0x36,
        "period" => 0x37,
        "slash" => 0x38,
        "caps" => 0x39,
        "printscreen" => 0x46,
        "scrolllock" => 0x47,
        "pause" => 0x48,
        "insert" => 0x49,
        "home" => 0x4A,
        "pageup" => 0x4B,
        "delete" => 0x4C,
        "end" => 0x4D,
        "pagedown" => 0x4E,
        "rightarrow" => 0x4F,
        "leftarrow" => 0x50,
        "downarrow" => 0x51,
        "uparrow" => 0x52,
        "l-ctrl" => 0xE0,
        "l-shift" => 0xE1,
        "l-alt" => 0xE2,
        "l-win" => 0xE3,
        "r-ctrl" => 0xE4,
        "r-shift" => 0xE5,
        "r-alt" => 0xE6,
        _ => return None, // ss-key, m0–m5 and r-win too: not keys on this board
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_keyboard::layout::ANSI_TKL;

    const GROUPS: [&str; 15] = [
        "function-keys",
        "number-keys",
        "q-row",
        "a-row",
        "z-row",
        "main-keyboard",
        "nav-cluster",
        "arrows",
        "all",
        "keypad",
        "keypad-nums",
        "macro-keys",
        "all-macro-keys",
        "logo",
        "esc-not-a-zone",
    ];

    #[test]
    fn every_zone_is_on_the_board_without_repeats() {
        for zone in GROUPS {
            let keys = named(zone);
            assert!(
                keys.iter().all(|&c| on_board(c)),
                "{zone} has a key the TKL lacks"
            );
            let mut sorted = keys.clone();
            sorted.sort_unstable();
            sorted.dedup();
            assert_eq!(sorted.len(), keys.len(), "{zone} repeats a key");
        }
    }

    #[test]
    fn groups_hold_their_keys_in_reading_order() {
        assert_eq!(named("function-keys"), (0x3A..=0x45).collect::<Vec<u8>>());
        assert_eq!(named("number-keys"), (0x1E..=0x27).collect::<Vec<u8>>());
        assert_eq!(
            named("q-row"),
            vec![0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13]
        );
        assert_eq!(
            named("a-row"),
            vec![0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F]
        );
        assert_eq!(
            named("z-row"),
            vec![0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10]
        );
        assert_eq!(
            named("nav-cluster"),
            vec![0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E]
        );
        assert_eq!(
            named("arrows"),
            vec![0x52, 0x50, 0x51, 0x4F],
            "↑ first: it's the higher row"
        );
        assert_eq!(named("all").len(), ANSI_TKL.len());
    }

    #[test]
    fn main_keyboard_is_the_alphanumeric_block() {
        let main = named("main-keyboard");
        assert!(
            main.contains(&0x1A) && main.contains(&0x2C) && main.contains(&0xE0),
            "W, Space, Ctrl"
        );
        for not_main in [0x29, 0x3A, 0x49, 0x52] {
            assert!(
                !main.contains(&not_main),
                "Esc, F1, Ins and ↑ are outside it: {not_main:#04x}"
            );
        }
        assert_eq!(main[0], 0x35, "starts at ` on the number row");
    }

    #[test]
    fn zones_this_keyboard_lacks_are_empty() {
        for zone in [
            "keypad",
            "keypad-nums",
            "macro-keys",
            "all-macro-keys",
            "logo",
            "keypad-5",
            "m0",
            "pound",
            "ss-key",
            "r-win",
            "no-such-zone",
        ] {
            assert!(named(zone).is_empty(), "{zone}");
        }
    }

    #[test]
    fn single_keys_map_to_their_codes() {
        assert_eq!(named("w"), vec![0x1A]);
        assert_eq!(named("keyboard-1"), vec![0x1E]);
        assert_eq!(named("keyboard-0"), vec![0x27]);
        assert_eq!(named("f12"), vec![0x45]);
        assert_eq!(named("spacebar"), vec![0x2C]);
        assert_eq!(named("escape"), vec![0x29]);
        assert_eq!(named("l-shift"), vec![0xE1]);
        assert_eq!(named("uparrow"), vec![0x52]);
        let menu = ANSI_TKL.iter().find(|k| k.1 == "Menu").unwrap().0;
        assert_eq!(
            named("win-menu"),
            vec![menu],
            "the key the layout calls Menu"
        );
    }
}
