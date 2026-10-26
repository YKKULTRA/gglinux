# GameSense server (M6a) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** gglinux speaks the SteelSeries GameSense SDK: games (including Proton ones, via
`coreProps.json` in every prefix) drive the Apex's key zones, taking the keyboard over when
gglinux lighting is off.

**Architecture:**
- **`gg-gamesense`** (new, pure crate): requests, handler parsing, zones, painting, the
  registry, and the Proton-prefix file helpers.
- **Daemon:** an axum server on loopback feeds the registry. The keyboard worker asks a
  generic `Overlay` for a layer every frame. A GameSense controller task owns the switch, the
  server lifecycle, the prefix scan, persistence, IPC and the built-in Test game.
- **UI:** a Games page.

**Tech Stack:** Rust 2021; axum 0.8 on the existing tokio runtime; tower 0.5 and http-body-util
0.1 for tests only; serde_json; Svelte 5.

**Spec:** `docs/superpowers/specs/2026-09-29-gamesense-server-design.md`

**Branch:** `m6a-gamesense` (exists; the spec is `52a2416`).

## Global Constraints

- **Server:** loopback only (`127.0.0.1`).
  - A request with an `Origin` header → 403.
  - A POST without `Content-Type: application/json` → 415.
  - A body over 64 KiB → 413.
  - Success → 200 `{}`; an error → 400 `{"error": "<message>"}`.
- **Names:** `^[A-Z0-9_-]{1,128}$` for `game` and `event`.
- **Limits:** 64 games, 128 events per game, 32 handlers per event.
- **Deinitialize timer:** default 15000 ms, clamped to 1000–60000.
- **Supported device types:** `keyboard` and `rgb-per-key-zones` only. Any other device type
  parses as `Target::Unsupported` and never paints.
- **Zone order:** left to right, then top to bottom, on `layout::ANSI_TKL`.
- **Colour rules:**
  - `pct = clamp((v − min)/(max − min)·100, 0, 100)`; a gradient uses `pct`;
  - ranges compare the raw value, with inclusive bounds;
  - a flash is on for the first half of each `1/f` period, counted from when the value arrived;
  - `repeat_limit` 0 or absent means unlimited; after the limit the colour stays on.
- **Registry:**
  - the active game is the most recent enabled, not-stopped game within its timer;
  - where events overlap, the most recently changed wins;
  - a repeated identical value doesn't reset `changed_at` unless `value_optional`;
  - an empty layer counts as inactive (`None`).
- **Settings:** `~/.config/gglinux/gamesense.json` holds `{enabled, port, games}`, written
  atomically. The switch defaults to **off**.
- **Prefix files:**
  - `ProgramData/SteelSeries/SteelSeries Engine 3/coreProps.json` = `{"address":"127.0.0.1:<port>"}`;
  - never overwrite a file that isn't ours ("ours" = parses as JSON with only an `address` key
    whose value starts with `127.0.0.1:`);
  - scan every 1 s while enabled, re-reading `libraryfolders.vdf` every 30 s;
  - on disable, remove our files and the two directories if empty.
- **Keyboard composition:**
  - order: base effect → reactive → overlay keys (they replace the colour);
  - brightness applies to overlay colours too;
  - frames are drawn when `lighting.enabled || overlay active`;
  - hand back when the worker holds the keyboard but neither is true;
  - 30 fps while an overlay is active;
  - `Wake` forces a frame only if the last one is at least one `FRAME` (33 ms) old.
- **Test game:** `GGLINUX_TEST` ("gglinux test", developer "gglinux"), event `HEALTH`:
  - `percent` on `function-keys`, gradient red `{255,0,0}` at 0 → green `{0,255,0}` at 100;
  - `rate.frequency` `[{low:0,high:20,frequency:4}]`;
  - values 100 → 0 → 100, one every 100 ms (201 values), then `stop_game`.
- **Checks:** `cargo test --workspace --all-targets`, `cargo clippy --workspace --all-targets` (no
  warnings), `cd ui && npm run check`.
- **Style:** hand-formatted, matching the surrounding code; don't run `cargo fmt`.
- **Hardware:** no hardware writes beyond the existing keyboard lighting protocol. Never run `sudo`.

## Review Focus

1. **A game sending 100+ events a second.** Every event wakes the keyboard. Frames must stay
   capped at 30 fps rather than one USB frame per event. Pinned by the Task 7 test
   `wake_forces_a_frame_at_most_once_per_frame_interval`.
2. **A prefix gglinux can't write** (another user's, or read-only). The scan fails for it every
   second; the warning must be logged once, not every second. Pinned by the Task 9 test
   `a_failing_prefix_is_reported_once`.
3. **No Steam installed, or no `libraryfolders.vdf`.** No prefixes and no errors; the server still
   works. Pinned by the Task 5 test `no_steam_means_no_libraries_and_no_error`.
4. **A corrupted `gamesense.json`.** The daemon starts with defaults (GameSense off) and logs a
   warning, like `lighting.json`. Pinned by the Task 9 test
   `unreadable_settings_fall_back_to_off`.
5. **The saved port taken by another program.** The server starts on a new port, and the prefixes
   are rewritten to it. Pinned by the Task 9 test `a_taken_port_falls_back_to_a_free_one`.

---

### Task 1: `gg-gamesense` crate and zones

**Files:**
- Create: `crates/gg-gamesense/Cargo.toml`
- Create: `crates/gg-gamesense/src/lib.rs`
- Create: `crates/gg-gamesense/src/zones.rs`
- Modify: `Cargo.toml` (workspace `members`; `[workspace.dependencies]`)

**Interfaces:**
- Produces:
  - `gg_gamesense::zones::named(zone: &str) -> Vec<u8>` — HID codes in zone order; empty for
    zones or keys not on this keyboard;
  - `gg_gamesense::zones::on_board(code: u8) -> bool`.

- [ ] **Step 1: Scaffold the crate**

`crates/gg-gamesense/Cargo.toml`:

```toml
[package]
name = "gg-gamesense"
description = "SteelSeries GameSense: requests, handlers, zones and the game registry"
version.workspace = true
edition.workspace = true
license.workspace = true
repository.workspace = true
rust-version.workspace = true

[dependencies]
serde.workspace = true
serde_json.workspace = true
gg-device-keyboard = { path = "../gg-device-keyboard" }
```

`crates/gg-gamesense/src/lib.rs`:

```rust
//! SteelSeries **GameSense**: what games send, and what it paints on the
//! Apex Pro TKL Gen 3. Pure — the HTTP server and the clock live in the daemon.

pub mod zones;
```

In the root `Cargo.toml`, add `"crates/gg-gamesense"` to `members` (after
`"crates/gg-device-keyboard"`), and add to `[workspace.dependencies]`:

```toml
axum = "0.8"
tower = { version = "0.5", features = ["util"] }
http-body-util = "0.1"
```

- [ ] **Step 2: Write the failing tests**

`crates/gg-gamesense/src/zones.rs`, starting with its tests and a stub:

```rust
//! GameSense zone names → HID codes on the Apex Pro TKL Gen 3, in zone
//! order: left to right, then top to bottom.

pub fn named(_zone: &str) -> Vec<u8> {
    Vec::new()
}

pub fn on_board(_code: u8) -> bool {
    false
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_keyboard::layout::ANSI_TKL;

    const GROUPS: [&str; 15] = [
        "function-keys", "number-keys", "q-row", "a-row", "z-row", "main-keyboard", "nav-cluster",
        "arrows", "all", "keypad", "keypad-nums", "macro-keys", "all-macro-keys", "logo", "esc-not-a-zone",
    ];

    #[test]
    fn every_zone_is_on_the_board_without_repeats() {
        for zone in GROUPS {
            let keys = named(zone);
            assert!(keys.iter().all(|&c| on_board(c)), "{zone} has a key the TKL lacks");
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
        assert_eq!(named("q-row"), vec![0x14, 0x1A, 0x08, 0x15, 0x17, 0x1C, 0x18, 0x0C, 0x12, 0x13]);
        assert_eq!(named("a-row"), vec![0x04, 0x16, 0x07, 0x09, 0x0A, 0x0B, 0x0D, 0x0E, 0x0F]);
        assert_eq!(named("z-row"), vec![0x1D, 0x1B, 0x06, 0x19, 0x05, 0x11, 0x10]);
        assert_eq!(named("nav-cluster"), vec![0x49, 0x4A, 0x4B, 0x4C, 0x4D, 0x4E]);
        assert_eq!(named("arrows"), vec![0x52, 0x50, 0x51, 0x4F], "↑ first: it's the higher row");
        assert_eq!(named("all").len(), ANSI_TKL.len());
    }

    #[test]
    fn main_keyboard_is_the_alphanumeric_block() {
        let main = named("main-keyboard");
        assert!(main.contains(&0x1A) && main.contains(&0x2C) && main.contains(&0xE0), "W, Space, Ctrl");
        for not_main in [0x29, 0x3A, 0x49, 0x52] {
            assert!(!main.contains(&not_main), "Esc, F1, Ins and ↑ are outside it: {not_main:#04x}");
        }
        assert_eq!(main[0], 0x35, "starts at ` on the number row");
    }

    #[test]
    fn zones_this_keyboard_lacks_are_empty() {
        for zone in ["keypad", "keypad-nums", "macro-keys", "all-macro-keys", "logo", "keypad-5", "m0", "pound", "ss-key", "no-such-zone"] {
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
    }
}
```

In `lib.rs`, `pub mod zones;` is already there.

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p gg-gamesense`
Expected: FAIL. `groups_hold_their_keys_in_reading_order`, `main_keyboard_is_the_alphanumeric_block` and
`single_keys_map_to_their_codes` fail. `every_zone_is_on_the_board_without_repeats` and
`zones_this_keyboard_lacks_are_empty` pass against the stub, since empty zones trivially hold.

- [ ] **Step 4: Implement**

Replace the stubs (keep the tests):

```rust
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
    if let Some(digit) = name.strip_prefix("keyboard-").and_then(|d| d.parse::<u8>().ok()) {
        return match digit {
            0 => Some(0x27),
            1..=9 => Some(0x1E + digit - 1),
            _ => None,
        };
    }
    if let Some(f) = name.strip_prefix('f').and_then(|n| n.parse::<u8>().ok()) {
        return (1..=12).contains(&f).then(|| 0x3A + f - 1);
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
        "win-menu" => 0x65,
        "l-ctrl" => 0xE0,
        "l-shift" => 0xE1,
        "l-alt" => 0xE2,
        "l-win" => 0xE3,
        "r-ctrl" => 0xE4,
        "r-shift" => 0xE5,
        "r-alt" => 0xE6,
        "r-win" => 0xE7,
        _ => return None, // ss-key and m0–m5 too: not HID keys on this board

    })
}
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p gg-gamesense && cargo clippy -p gg-gamesense --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: 5 passed; clippy count `0`.

- [ ] **Step 6: Commit**

```bash
git add Cargo.toml Cargo.lock crates/gg-gamesense
git commit -m "gg-gamesense: new crate; GameSense zones on the Apex TKL

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 2: Requests and handlers (`proto`)

**Files:**
- Create: `crates/gg-gamesense/src/proto.rs`
- Modify: `crates/gg-gamesense/src/lib.rs` (`pub mod proto;`)

**Interfaces:**
- Consumes: `zones::{named, on_board}` (Task 1).
- Produces (in `gg_gamesense::proto`):
  - `valid_name(&str) -> bool`, `MAX_NAME: usize = 128`;
  - request structs (all `Deserialize`, `Debug`, `Clone`):
    - `GameMetadata { game, game_display_name: Option<String>, developer: Option<String>, deinitialize_timer_length_ms: Option<u32> }`;
    - `RegisterEvent { game, event, min_value: Option<i64>, max_value: Option<i64>, icon_id: Option<u32>, value_optional: bool }`;
    - `BindEvent { game, event, min_value, max_value, icon_id, value_optional, handlers: Vec<Value> }`;
    - `EventData { value: Option<f64>, frame: Option<Map<String, Value>> }`;
    - `GameEvent { game, event, data: EventData }`;
    - `EventItem { event, data: EventData }`;
    - `MultipleEvents { game, events: Vec<EventItem> }`;
    - `GameOnly { game }`;
    - `RemoveEvent { game, event }`;
  - handler types (all `Debug`, `Clone`, `PartialEq`):
    - `Target { Keys(Vec<u8>), Unsupported }`;
    - `Mode { Color, Percent, Count, ContextColor }` (also `Copy`, `Eq`);
    - `ColorSpec { Static(Rgb), Gradient { zero: Rgb, hundred: Rgb }, Ranges(Vec<ColorRange>) }`;
    - `ColorRange { low: i64, high: i64, color: ColorSpec }`;
    - `Ranged<T> { Fixed(T), Ranges(Vec<(i64, i64, T)>) }`;
    - `Rate { frequency: Ranged<f32>, repeat_limit: Option<Ranged<u32>> }`;
    - `Handler { target, mode, color: Option<ColorSpec>, rate: Option<Rate>, context_frame_key: Option<String> }`;
  - `Handler::parse(&Value) -> Result<Handler, String>` and `parse_rgb(&Value) -> Result<Rgb, String>`.
  `Rgb` is `gg_device_keyboard::Rgb`.

- [ ] **Step 1: Write the failing tests**

`crates/gg-gamesense/src/proto.rs`. Write the whole module below **except** the bodies of
`valid_name`, `Handler::parse`, `parse_rgb`, `color_spec`, `rate` and `ranged`: give those
`todo!()` bodies for this step. Tests go at the bottom:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };
    const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };

    #[test]
    fn names_are_upper_case_digits_dash_and_underscore() {
        assert!(valid_name("MY_GAME-2"));
        for bad in ["", "my_game", "MY GAME", "GAME!", &"A".repeat(129)] {
            assert!(!valid_name(bad), "{bad:?}");
        }
        assert!(valid_name(&"A".repeat(128)));
    }

    #[test]
    fn the_sdk_health_bar_example_parses() {
        let bind: BindEvent = serde_json::from_value(json!({
            "game": "MY_GAME", "event": "HEALTH", "min_value": 0, "max_value": 100, "icon_id": 1,
            "handlers": [{
                "device-type": "keyboard", "zone": "function-keys", "mode": "percent",
                "color": { "gradient": { "zero": { "red": 255, "green": 0, "blue": 0 },
                                         "hundred": { "red": 0, "green": 255, "blue": 0 } } },
                "rate": { "frequency": [{ "low": 0, "high": 20, "frequency": 4 }],
                          "repeat_limit": [{ "low": 0, "high": 20, "repeat_limit": 5 }] }
            }]
        })).unwrap();
        let h = Handler::parse(&bind.handlers[0]).unwrap();
        assert_eq!(h.target, Target::Keys((0x3A..=0x45).collect()));
        assert_eq!(h.mode, Mode::Percent);
        assert_eq!(h.color, Some(ColorSpec::Gradient { zero: RED, hundred: GREEN }));
        assert_eq!(h.rate, Some(Rate {
            frequency: Ranged::Ranges(vec![(0, 20, 4.0)]),
            repeat_limit: Some(Ranged::Ranges(vec![(0, 20, 5)])),
        }));
    }

    #[test]
    fn handlers_for_other_devices_are_kept_as_unsupported() {
        for v in [
            json!({ "device-type": "screened", "zone": "one", "mode": "screen", "datas": [] }),
            json!({ "device-type": "tactile", "zone": "one", "mode": "vibrate", "pattern": [] }),
            json!({ "device-type": "mouse", "zone": "wheel", "color": { "red": 1, "green": 2, "blue": 3 } }),
            json!({ "device-type": "rgb-2-zone", "zone": "two", "color": { "red": 1, "green": 2, "blue": 3 } }),
        ] {
            assert_eq!(Handler::parse(&v).unwrap().target, Target::Unsupported, "{v}");
        }
    }

    #[test]
    fn custom_zone_keys_keep_their_order_and_drop_keys_the_tkl_lacks() {
        let h = Handler::parse(&json!({
            "device-type": "rgb-per-key-zones", "custom-zone-keys": [26, 4, 89, 22, 7],
            "color": { "red": 0, "green": 0, "blue": 255 }
        })).unwrap();
        assert_eq!(h.target, Target::Keys(vec![26, 4, 22, 7]), "WASD, without keypad 1 (89)");
        assert_eq!(h.mode, Mode::Color, "mode defaults to color");
    }

    #[test]
    fn colours_come_as_static_gradient_or_ranges() {
        let ranges = Handler::parse(&json!({
            "device-type": "keyboard", "zone": "all",
            "color": [ { "low": 0, "high": 10, "color": { "red": 255, "green": 0, "blue": 0 } },
                       { "low": 11, "high": 100, "color": { "gradient": { "zero": { "red": 255, "green": 0, "blue": 0 },
                                                                           "hundred": { "red": 0, "green": 255, "blue": 0 } } } } ]
        })).unwrap();
        assert_eq!(ranges.color, Some(ColorSpec::Ranges(vec![
            ColorRange { low: 0, high: 10, color: ColorSpec::Static(RED) },
            ColorRange { low: 11, high: 100, color: ColorSpec::Gradient { zero: RED, hundred: GREEN } },
        ])));
        let fixed = Handler::parse(&json!({ "device-type": "keyboard", "zone": "all",
            "color": { "red": 1, "green": 2, "blue": 3 }, "rate": { "frequency": 2.5, "repeat_limit": 3 } })).unwrap();
        assert_eq!(fixed.rate, Some(Rate { frequency: Ranged::Fixed(2.5), repeat_limit: Some(Ranged::Fixed(3)) }));
    }

    #[test]
    fn malformed_handlers_are_errors() {
        for bad in [
            json!({ "zone": "all", "color": { "red": 1, "green": 2, "blue": 3 } }),                 // no device-type
            json!({ "device-type": "keyboard", "color": { "red": 1, "green": 2, "blue": 3 } }),     // no zone
            json!({ "device-type": "keyboard", "zone": "all", "color": { "red": 300, "green": 0, "blue": 0 } }),
            json!({ "device-type": "keyboard", "zone": "all" }),                                     // no color
            json!({ "device-type": "keyboard", "zone": "all", "mode": "context-color" }),            // no context-frame-key
            json!({ "device-type": "keyboard", "zone": "all", "mode": "sparkle", "color": { "red": 1, "green": 2, "blue": 3 } }),
            json!("not an object"),
        ] {
            assert!(Handler::parse(&bad).is_err(), "{bad}");
        }
    }

    #[test]
    fn context_color_needs_only_its_frame_key() {
        let h = Handler::parse(&json!({ "device-type": "keyboard", "zone": "q-row", "mode": "context-color",
            "context-frame-key": "zone-color" })).unwrap();
        assert_eq!((h.mode, h.context_frame_key.as_deref(), h.color), (Mode::ContextColor, Some("zone-color"), None));
    }

    #[test]
    fn event_values_may_be_integers_or_floats_and_frames_pass_through() {
        let e: GameEvent = serde_json::from_value(json!({ "game": "G", "event": "E",
            "data": { "value": 42.6, "frame": { "zone-color": { "red": 1, "green": 2, "blue": 3 } } } })).unwrap();
        assert_eq!(e.data.value, Some(42.6));
        assert!(e.data.frame.unwrap().contains_key("zone-color"));
        let m: MultipleEvents = serde_json::from_value(json!({ "game": "G",
            "events": [ { "event": "A", "data": { "value": 1 } }, { "event": "B", "data": {} } ] })).unwrap();
        assert_eq!((m.events.len(), m.events[1].data.value), (2, None));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gg-gamesense proto`
Expected: FAIL. Every test panics at `not yet implemented`, except
`event_values_may_be_integers_or_floats_and_frames_pass_through`, which only uses serde and
passes.

- [ ] **Step 3: Implement**

The full `proto.rs` above the tests:

```rust
//! What games send: the GameSense requests, and colour handlers parsed into
//! something the painter can use. Parsing is lenient about devices this
//! machine can't show and strict about malformed colours.

use serde::Deserialize;
use serde_json::{Map, Value};

pub use gg_device_keyboard::Rgb;

use crate::zones;

/// Longest game or event name accepted.
pub const MAX_NAME: usize = 128;

/// `game` and `event` names: `A-Z 0-9 - _`, 1..=128 characters.
pub fn valid_name(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= MAX_NAME
        && name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'-' || b == b'_')
}

#[derive(Debug, Clone, Deserialize)]
pub struct GameMetadata {
    pub game: String,
    pub game_display_name: Option<String>,
    pub developer: Option<String>,
    pub deinitialize_timer_length_ms: Option<u32>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RegisterEvent {
    pub game: String,
    pub event: String,
    pub min_value: Option<i64>,
    pub max_value: Option<i64>,
    pub icon_id: Option<u32>,
    #[serde(default)]
    pub value_optional: bool,
}

#[derive(Debug, Clone, Deserialize)]
pub struct BindEvent {
    pub game: String,
    pub event: String,
    pub min_value: Option<i64>,
    pub max_value: Option<i64>,
    pub icon_id: Option<u32>,
    #[serde(default)]
    pub value_optional: bool,
    #[serde(default)]
    pub handlers: Vec<Value>,
}

/// An event's payload. `value` is an integer in the SDK; floats are accepted
/// and rounded, since some games send them.
#[derive(Debug, Clone, Deserialize)]
pub struct EventData {
    pub value: Option<f64>,
    pub frame: Option<Map<String, Value>>,
}

#[derive(Debug, Clone, Deserialize)]
pub struct GameEvent {
    pub game: String,
    pub event: String,
    pub data: EventData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct EventItem {
    pub event: String,
    pub data: EventData,
}

#[derive(Debug, Clone, Deserialize)]
pub struct MultipleEvents {
    pub game: String,
    pub events: Vec<EventItem>,
}

/// `game_heartbeat`, `stop_game`, `remove_game`.
#[derive(Debug, Clone, Deserialize)]
pub struct GameOnly {
    pub game: String,
}

#[derive(Debug, Clone, Deserialize)]
pub struct RemoveEvent {
    pub game: String,
    pub event: String,
}

/// Where a handler paints.
#[derive(Debug, Clone, PartialEq)]
pub enum Target {
    /// HID codes on this keyboard, in zone order.
    Keys(Vec<u8>),
    /// A device this machine can't show: a screen, tactile, mouse, headset, zoned RGB.
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Mode {
    Color,
    Percent,
    Count,
    ContextColor,
}

#[derive(Debug, Clone, PartialEq)]
pub enum ColorSpec {
    Static(Rgb),
    Gradient { zero: Rgb, hundred: Rgb },
    Ranges(Vec<ColorRange>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct ColorRange {
    pub low: i64,
    pub high: i64,
    pub color: ColorSpec,
}

/// A number, or a list of `(low, high, number)` ranges over the event value.
#[derive(Debug, Clone, PartialEq)]
pub enum Ranged<T> {
    Fixed(T),
    Ranges(Vec<(i64, i64, T)>),
}

#[derive(Debug, Clone, PartialEq)]
pub struct Rate {
    pub frequency: Ranged<f32>,
    pub repeat_limit: Option<Ranged<u32>>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Handler {
    pub target: Target,
    pub mode: Mode,
    pub color: Option<ColorSpec>,
    pub rate: Option<Rate>,
    pub context_frame_key: Option<String>,
}

/// Device types whose handlers this keyboard can show.
const KEY_DEVICES: [&str; 2] = ["keyboard", "rgb-per-key-zones"];

impl Handler {
    /// Parse one handler. Handlers for devices this machine can't show become
    /// [`Target::Unsupported`] without further checks; a malformed key handler
    /// is an error, so a game's bug is reported rather than silently dark.
    pub fn parse(v: &Value) -> Result<Handler, String> {
        let o = v.as_object().ok_or("a handler must be an object")?;
        let device = o
            .get("device-type")
            .and_then(Value::as_str)
            .ok_or("a handler needs a device-type")?;
        if !KEY_DEVICES.contains(&device) {
            return Ok(Handler { target: Target::Unsupported, mode: Mode::Color, color: None, rate: None, context_frame_key: None });
        }
        let target = match (o.get("custom-zone-keys"), o.get("zone").and_then(Value::as_str)) {
            (Some(keys), _) => Target::Keys(custom_keys(keys)?),
            (None, Some(zone)) => Target::Keys(zones::named(zone)),
            (None, None) => return Err("a handler needs a zone or custom-zone-keys".into()),
        };
        let mode = match o.get("mode").and_then(Value::as_str).unwrap_or("color") {
            "color" => Mode::Color,
            "percent" => Mode::Percent,
            "count" => Mode::Count,
            "context-color" => Mode::ContextColor,
            other => return Err(format!("unknown handler mode {other:?}")),
        };
        let context_frame_key = o.get("context-frame-key").and_then(Value::as_str).map(str::to_owned);
        let color = match (mode, o.get("color")) {
            (Mode::ContextColor, _) if context_frame_key.is_none() => {
                return Err("context-color needs a context-frame-key".into())
            }
            (Mode::ContextColor, _) => None,
            (_, Some(c)) => Some(color_spec(c)?),
            (_, None) => return Err("a handler needs a color".into()),
        };
        let rate = o.get("rate").map(rate).transpose()?;
        Ok(Handler { target, mode, color, rate, context_frame_key })
    }
}

/// `{red, green, blue}`, each 0..=255.
pub fn parse_rgb(v: &Value) -> Result<Rgb, String> {
    let channel = |k: &str| {
        v.get(k)
            .and_then(Value::as_u64)
            .filter(|&n| n <= 255)
            .map(|n| n as u8)
            .ok_or_else(|| format!("a colour needs {k} in 0..=255"))
    };
    Ok(Rgb { r: channel("red")?, g: channel("green")?, b: channel("blue")? })
}

fn custom_keys(v: &Value) -> Result<Vec<u8>, String> {
    let list = v.as_array().ok_or("custom-zone-keys must be a list of HID codes")?;
    Ok(list
        .iter()
        .filter_map(Value::as_u64)
        .filter_map(|n| u8::try_from(n).ok())
        .filter(|&c| zones::on_board(c))
        .collect())
}

fn color_spec(v: &Value) -> Result<ColorSpec, String> {
    if let Some(list) = v.as_array() {
        return list
            .iter()
            .map(|r| {
                let (low, high) = bounds(r, "colour")?;
                let color = color_spec(r.get("color").ok_or("a colour range needs a color")?)?;
                Ok(ColorRange { low, high, color })
            })
            .collect::<Result<_, String>>()
            .map(ColorSpec::Ranges);
    }
    if let Some(g) = v.get("gradient") {
        return Ok(ColorSpec::Gradient {
            zero: parse_rgb(g.get("zero").ok_or("a gradient needs zero")?)?,
            hundred: parse_rgb(g.get("hundred").ok_or("a gradient needs hundred")?)?,
        });
    }
    parse_rgb(v).map(ColorSpec::Static)
}

fn rate(v: &Value) -> Result<Rate, String> {
    let frequency = ranged(v.get("frequency").ok_or("a rate needs a frequency")?, "frequency", |x| {
        x.as_f64().map(|f| f as f32).filter(|f| f.is_finite() && *f >= 0.0)
    })?;
    let repeat_limit = v
        .get("repeat_limit")
        .map(|r| ranged(r, "repeat_limit", |x| x.as_u64().map(|n| n.min(u64::from(u32::MAX)) as u32)))
        .transpose()?;
    Ok(Rate { frequency, repeat_limit })
}

fn ranged<T>(v: &Value, key: &str, num: impl Fn(&Value) -> Option<T>) -> Result<Ranged<T>, String> {
    let Some(list) = v.as_array() else {
        return num(v).map(Ranged::Fixed).ok_or_else(|| format!("bad {key}"));
    };
    list.iter()
        .map(|r| {
            let (low, high) = bounds(r, key)?;
            let x = r.get(key).and_then(&num).ok_or_else(|| format!("a {key} range needs {key}"))?;
            Ok((low, high, x))
        })
        .collect::<Result<_, String>>()
        .map(Ranged::Ranges)
}

fn bounds(r: &Value, what: &str) -> Result<(i64, i64), String> {
    let low = r.get("low").and_then(Value::as_i64).ok_or_else(|| format!("a {what} range needs low"))?;
    let high = r.get("high").and_then(Value::as_i64).ok_or_else(|| format!("a {what} range needs high"))?;
    Ok((low, high))
}
```

Add `pub mod proto;` to `lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p gg-gamesense && cargo clippy -p gg-gamesense --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: all pass (5 zones + 8 proto); clippy count `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/gg-gamesense
git commit -m "gg-gamesense: GameSense requests and colour handlers

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 3: Painting one handler (`paint`)

**Files:**
- Create: `crates/gg-gamesense/src/paint.rs`
- Modify: `crates/gg-gamesense/src/lib.rs` (`pub mod paint;`)

**Interfaces:**
- Consumes: `proto::{ColorSpec, Handler, Mode, Ranged, Rate, Target, parse_rgb, Rgb}` (Task 2).
- Produces:
  - `pub struct Reading<'a> { pub value: i64, pub min: i64, pub max: i64, pub frame: Option<&'a Map<String, Value>>, pub age: f64 }`,
    with `percent(&self) -> f32`;
  - `color_of(&ColorSpec, &Reading) -> Option<Rgb>`;
  - `lit(Option<&Rate>, &Reading) -> bool`;
  - `paint(&Handler, &Reading) -> Vec<(u8, Rgb)>`.

- [ ] **Step 1: Write the failing tests**

`crates/gg-gamesense/src/paint.rs`: `use` lines, `Reading`, and stub bodies (`todo!()`) for
`percent`, `color_of`, `lit` and `paint`. Then:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::proto::ColorRange;
    use serde_json::json;

    const RED: Rgb = Rgb { r: 255, g: 0, b: 0 };
    const GREEN: Rgb = Rgb { r: 0, g: 255, b: 0 };
    const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

    fn at(value: i64) -> Reading<'static> {
        Reading { value, min: 0, max: 100, frame: None, age: 0.0 }
    }

    fn handler(mode: Mode, keys: &[u8], color: ColorSpec) -> Handler {
        Handler { target: Target::Keys(keys.to_vec()), mode, color: Some(color), rate: None, context_frame_key: None }
    }

    #[test]
    fn percent_scales_between_min_and_max_and_clamps() {
        assert_eq!(at(50).percent(), 50.0);
        assert_eq!(Reading { value: 15, min: 10, max: 20, ..at(0) }.percent(), 50.0);
        assert_eq!(at(-5).percent(), 0.0);
        assert_eq!(at(250).percent(), 100.0);
        assert_eq!(Reading { value: 7, min: 7, max: 7, ..at(0) }.percent(), 100.0, "an empty range is full at its value");
    }

    #[test]
    fn gradients_blend_by_percent_and_ranges_pick_by_value() {
        let g = ColorSpec::Gradient { zero: RED, hundred: GREEN };
        assert_eq!(color_of(&g, &at(0)), Some(RED));
        assert_eq!(color_of(&g, &at(100)), Some(GREEN));
        assert_eq!(color_of(&g, &at(50)), Some(Rgb { r: 128, g: 128, b: 0 }));
        let ranges = ColorSpec::Ranges(vec![
            ColorRange { low: 0, high: 10, color: ColorSpec::Static(RED) },
            ColorRange { low: 11, high: 100, color: ColorSpec::Static(GREEN) },
        ]);
        assert_eq!(color_of(&ranges, &at(10)), Some(RED), "bounds are inclusive");
        assert_eq!(color_of(&ranges, &at(11)), Some(GREEN));
        assert_eq!(color_of(&ranges, &at(101)), None, "outside every range paints nothing");
    }

    #[test]
    fn color_mode_paints_the_whole_zone() {
        let h = handler(Mode::Color, &[1, 2, 3], ColorSpec::Static(RED));
        assert_eq!(paint(&h, &at(5)), vec![(1, RED), (2, RED), (3, RED)]);
    }

    #[test]
    fn percent_mode_fills_in_order_dims_the_last_key_and_darkens_the_rest() {
        let h = handler(Mode::Percent, &[1, 2, 3, 4, 5], ColorSpec::Static(RED));
        assert_eq!(paint(&h, &at(50)), vec![
            (1, RED), (2, RED), (3, Rgb { r: 128, g: 0, b: 0 }), (4, BLACK), (5, BLACK),
        ]);
        assert!(paint(&h, &at(0)).iter().all(|&(_, c)| c == BLACK));
    }

    #[test]
    fn count_mode_lights_value_keys() {
        let h = handler(Mode::Count, &[1, 2, 3, 4], ColorSpec::Static(GREEN));
        assert_eq!(paint(&h, &at(2)), vec![(1, GREEN), (2, GREEN), (3, BLACK), (4, BLACK)]);
        assert_eq!(paint(&h, &at(9)).iter().filter(|&&(_, c)| c == GREEN).count(), 4, "clamped to the zone");
    }

    #[test]
    fn context_color_comes_from_the_frame() {
        let h = Handler { target: Target::Keys(vec![7]), mode: Mode::ContextColor, color: None, rate: None,
            context_frame_key: Some("tint".into()) };
        let frame = json!({ "tint": { "red": 1, "green": 2, "blue": 3 } });
        let frame = frame.as_object().unwrap();
        assert_eq!(paint(&h, &Reading { frame: Some(frame), ..at(0) }), vec![(7, Rgb { r: 1, g: 2, b: 3 })]);
        assert!(paint(&h, &at(0)).is_empty(), "no frame, nothing painted");
    }

    #[test]
    fn flashing_is_on_for_the_first_half_of_each_period() {
        let rate = Rate { frequency: Ranged::Fixed(2.0), repeat_limit: None };
        let r = |age| Reading { age, ..at(50) };
        assert!(lit(Some(&rate), &r(0.1)));
        assert!(!lit(Some(&rate), &r(0.3)));
        assert!(lit(Some(&rate), &r(0.55)));
        assert!(lit(None, &r(0.3)), "no rate: steady");
        let off = Rate { frequency: Ranged::Fixed(0.0), repeat_limit: None };
        assert!(lit(Some(&off), &r(0.3)), "frequency 0: steady");
    }

    #[test]
    fn repeat_limits_stop_the_flashing_on_and_ranges_pick_the_frequency() {
        let limited = Rate { frequency: Ranged::Fixed(2.0), repeat_limit: Some(Ranged::Fixed(2)) };
        assert!(!lit(Some(&limited), &Reading { age: 0.8, ..at(50) }), "second flash, off half");
        assert!(lit(Some(&limited), &Reading { age: 1.3, ..at(50) }), "after two flashes it stays on");
        let unlimited = Rate { frequency: Ranged::Fixed(2.0), repeat_limit: Some(Ranged::Fixed(0)) };
        assert!(!lit(Some(&unlimited), &Reading { age: 5.3, ..at(50) }), "0 means no limit");
        let low_only = Rate { frequency: Ranged::Ranges(vec![(0, 20, 4.0)]), repeat_limit: None };
        assert!(!lit(Some(&low_only), &Reading { age: 0.2, ..at(10) }), "10 is in 0..=20: flashing");
        assert!(lit(Some(&low_only), &Reading { age: 0.2, ..at(50) }), "50 matches no range: steady");
    }

    #[test]
    fn a_flashing_handler_paints_black_in_its_off_half_and_unsupported_paints_nothing() {
        let mut h = handler(Mode::Color, &[1], ColorSpec::Static(RED));
        h.rate = Some(Rate { frequency: Ranged::Fixed(2.0), repeat_limit: None });
        assert_eq!(paint(&h, &Reading { age: 0.3, ..at(1) }), vec![(1, BLACK)]);
        h.target = Target::Unsupported;
        assert!(paint(&h, &at(1)).is_empty());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gg-gamesense paint`
Expected: FAIL; every test panics at `not yet implemented`.

- [ ] **Step 3: Implement**

```rust
//! What one handler paints for an event's current value, at a moment in time.
//! Pure: the registry supplies the value, its range and its age.

use serde_json::{Map, Value};

use crate::proto::{parse_rgb, ColorSpec, Handler, Mode, Ranged, Rate, Rgb, Target};

const BLACK: Rgb = Rgb { r: 0, g: 0, b: 0 };

/// An event's value as it stands now.
pub struct Reading<'a> {
    pub value: i64,
    pub min: i64,
    pub max: i64,
    pub frame: Option<&'a Map<String, Value>>,
    /// Seconds since the value arrived, for flashing.
    pub age: f64,
}

impl Reading<'_> {
    /// The value as 0..=100 of its range.
    pub fn percent(&self) -> f32 {
        if self.max <= self.min {
            return if self.value >= self.max { 100.0 } else { 0.0 };
        }
        let p = (self.value - self.min) as f64 / (self.max - self.min) as f64 * 100.0;
        p.clamp(0.0, 100.0) as f32
    }
}

/// The colour a spec gives this reading; `None` when a range list has no match.
pub fn color_of(spec: &ColorSpec, r: &Reading) -> Option<Rgb> {
    match spec {
        ColorSpec::Static(c) => Some(*c),
        ColorSpec::Gradient { zero, hundred } => Some(lerp(*zero, *hundred, r.percent() / 100.0)),
        ColorSpec::Ranges(ranges) => ranges
            .iter()
            .find(|x| (x.low..=x.high).contains(&r.value))
            .and_then(|x| color_of(&x.color, r)),
    }
}

/// Whether a handler shows its colour now, or is in the dark half of a flash.
pub fn lit(rate: Option<&Rate>, r: &Reading) -> bool {
    let Some(rate) = rate else { return true };
    let f = pick(&rate.frequency, r.value).unwrap_or(0.0);
    if f <= 0.0 {
        return true;
    }
    let period = 1.0 / f64::from(f);
    let flashes = (r.age / period).floor();
    let limit = rate.repeat_limit.as_ref().and_then(|l| pick(l, r.value)).unwrap_or(0);
    if limit > 0 && flashes >= f64::from(limit) {
        return true; // done flashing; it stays on
    }
    r.age - flashes * period < period / 2.0
}

/// The keys one handler paints for this reading, with their colours.
pub fn paint(h: &Handler, r: &Reading) -> Vec<(u8, Rgb)> {
    let Target::Keys(keys) = &h.target else { return Vec::new() };
    let color = match h.mode {
        Mode::ContextColor => h
            .context_frame_key
            .as_ref()
            .and_then(|k| r.frame?.get(k))
            .and_then(|v| parse_rgb(v).ok()),
        _ => h.color.as_ref().and_then(|c| color_of(c, r)),
    };
    let Some(color) = color else { return Vec::new() };
    let color = if lit(h.rate.as_ref(), r) { color } else { BLACK };
    match h.mode {
        Mode::Color | Mode::ContextColor => keys.iter().map(|&k| (k, color)).collect(),
        Mode::Percent => {
            let filled = r.percent() / 100.0 * keys.len() as f32;
            keys.iter()
                .enumerate()
                .map(|(i, &k)| (k, scale(color, (filled - i as f32).clamp(0.0, 1.0))))
                .collect()
        }
        Mode::Count => {
            let n = r.value.clamp(0, keys.len() as i64) as usize;
            keys.iter().enumerate().map(|(i, &k)| (k, if i < n { color } else { BLACK })).collect()
        }
    }
}

fn pick<T: Copy>(r: &Ranged<T>, value: i64) -> Option<T> {
    match r {
        Ranged::Fixed(x) => Some(*x),
        Ranged::Ranges(list) => list.iter().find(|(lo, hi, _)| (*lo..=*hi).contains(&value)).map(|&(_, _, x)| x),
    }
}

fn lerp(a: Rgb, b: Rgb, t: f32) -> Rgb {
    let f = |x: u8, y: u8| (f32::from(x) + (f32::from(y) - f32::from(x)) * t).round() as u8;
    Rgb { r: f(a.r, b.r), g: f(a.g, b.g), b: f(a.b, b.b) }
}

fn scale(c: Rgb, k: f32) -> Rgb {
    let f = |v: u8| (f32::from(v) * k).round() as u8;
    Rgb { r: f(c.r), g: f(c.g), b: f(c.b) }
}
```

Add `pub mod paint;` to `lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p gg-gamesense && cargo clippy -p gg-gamesense --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: all pass (5 + 8 + 9); clippy count `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/gg-gamesense
git commit -m "gg-gamesense: paint a handler — colour forms, modes, flashing

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 4: The registry

**Files:**
- Create: `crates/gg-gamesense/src/registry.rs`
- Modify: `crates/gg-gamesense/src/lib.rs` (`pub mod registry;` and
  `pub use registry::{GameSummary, Layer, Registry, SavedEvent, SavedGame};`)

**Interfaces:**
- Consumes: `proto::*` (Task 2); `paint::{paint, Reading}` (Task 3).
- Produces (in `gg_gamesense`):
  - `type Layer = HashMap<u8, Rgb>`;
  - `MAX_GAMES = 64`, `MAX_EVENTS = 128`, `MAX_HANDLERS = 32`;
  - `SavedEvent { name, min_value: i64, max_value: i64, icon_id: u32, value_optional: bool, handlers: Vec<Value> }`
    (serde);
  - `SavedGame { name, display_name: Option<String>, developer: Option<String>, deinitialize_timer_length_ms: u32, enabled: bool, events: Vec<SavedEvent> }`
    (serde);
  - `GameSummary { name, display_name, developer, enabled, active, events: Vec<String> }`;
  - `Registry` (`Default`) with:
    - `game_metadata(GameMetadata) -> Result<(), String>`;
    - `register_event(RegisterEvent)`, `bind_event(BindEvent)`;
    - `game_event(GameEvent, now: f64)`, `multiple_events(MultipleEvents, now: f64)`;
    - `heartbeat(&str, now: f64)`, `stop_game(&str)`, `remove_game(&str)`,
      `remove_event(&str, &str)`, `set_enabled(&str, bool)` — every mutation returns
      `Result<(), String>`;
    - `active_game(now) -> Option<&str>`, `paint(now) -> Option<Layer>`,
      `summaries(now) -> Vec<GameSummary>`;
    - `saved() -> Vec<SavedGame>`, `Registry::restore(Vec<SavedGame>) -> Registry`.

- [ ] **Step 1: Write the failing tests**

`registry.rs`: types and method signatures with `todo!()` bodies, then:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn bind(game: &str, event: &str, handlers: Value) -> BindEvent {
        serde_json::from_value(json!({ "game": game, "event": event, "handlers": handlers })).unwrap()
    }

    fn fkeys_bar() -> Value {
        json!([{ "device-type": "keyboard", "zone": "function-keys", "mode": "percent",
                 "color": { "red": 255, "green": 0, "blue": 0 } }])
    }

    fn send(r: &mut Registry, game: &str, event: &str, value: f64, now: f64) -> Result<(), String> {
        r.game_event(serde_json::from_value(json!({ "game": game, "event": event, "data": { "value": value } })).unwrap(), now)
    }

    fn with_bar() -> Registry {
        let mut r = Registry::default();
        r.bind_event(bind("G", "HEALTH", fkeys_bar())).unwrap();
        r
    }

    #[test]
    fn bound_events_paint_once_a_value_arrives() {
        let mut r = with_bar();
        assert_eq!(r.paint(0.0), None, "no value yet");
        send(&mut r, "G", "HEALTH", 100.0, 0.0).unwrap();
        let layer = r.paint(0.5).unwrap();
        assert_eq!(layer.len(), 12);
        assert_eq!(layer[&0x3A], Rgb { r: 255, g: 0, b: 0 });
    }

    #[test]
    fn unknown_games_events_and_bad_names_are_errors() {
        let mut r = with_bar();
        assert!(send(&mut r, "NOPE", "HEALTH", 1.0, 0.0).is_err());
        assert!(send(&mut r, "G", "NOPE", 1.0, 0.0).is_err());
        assert!(r.bind_event(bind("lower", "E", fkeys_bar())).is_err());
        assert!(r.heartbeat("NOPE", 0.0).is_err());
        let missing = serde_json::from_value(json!({ "game": "G", "event": "HEALTH", "data": {} })).unwrap();
        assert!(r.game_event(missing, 0.0).is_err(), "value required unless value_optional");
        let min_over_max = serde_json::from_value(json!({ "game": "G", "event": "E", "min_value": 5, "max_value": 1 })).unwrap();
        assert!(r.register_event(min_over_max).is_err());
    }

    #[test]
    fn limits_are_enforced() {
        let mut r = Registry::default();
        for i in 0..MAX_GAMES {
            r.game_metadata(serde_json::from_value(json!({ "game": format!("G{i}") })).unwrap()).unwrap();
        }
        assert!(r.game_metadata(serde_json::from_value(json!({ "game": "ONE_MORE" })).unwrap()).is_err());
        let mut r = Registry::default();
        for i in 0..MAX_EVENTS {
            r.register_event(serde_json::from_value(json!({ "game": "G", "event": format!("E{i}") })).unwrap()).unwrap();
        }
        assert!(r.register_event(serde_json::from_value(json!({ "game": "G", "event": "ONE_MORE" })).unwrap()).is_err());
        let many = Value::Array(vec![fkeys_bar()[0].clone(); MAX_HANDLERS + 1]);
        assert!(Registry::default().bind_event(bind("G", "E", many)).is_err());
    }

    #[test]
    fn a_game_goes_quiet_after_its_timer_and_heartbeats_keep_it_up() {
        let mut r = with_bar();
        send(&mut r, "G", "HEALTH", 50.0, 0.0).unwrap();
        assert_eq!(r.active_game(14.9), Some("G"));
        assert_eq!(r.active_game(15.1), None, "the default timer is 15 s");
        r.heartbeat("G", 14.0).unwrap();
        assert_eq!(r.active_game(28.0), Some("G"));
        r.game_metadata(serde_json::from_value(json!({ "game": "G", "deinitialize_timer_length_ms": 100 })).unwrap()).unwrap();
        assert_eq!(r.active_game(15.2), None, "clamped up to 1 s: 14 + 1 < 15.2");
        assert_eq!(r.active_game(14.9), Some("G"));
    }

    #[test]
    fn stop_game_ends_it_at_once_and_a_new_event_restarts_it() {
        let mut r = with_bar();
        send(&mut r, "G", "HEALTH", 50.0, 0.0).unwrap();
        r.stop_game("G").unwrap();
        assert_eq!(r.paint(0.1), None);
        send(&mut r, "G", "HEALTH", 40.0, 0.2).unwrap();
        assert!(r.paint(0.3).is_some());
    }

    #[test]
    fn disabled_games_paint_nothing() {
        let mut r = with_bar();
        send(&mut r, "G", "HEALTH", 50.0, 0.0).unwrap();
        r.set_enabled("G", false).unwrap();
        assert_eq!(r.paint(0.1), None);
        assert!(!r.summaries(0.1)[0].active);
    }

    #[test]
    fn the_most_recently_active_game_is_shown() {
        let mut r = with_bar();
        r.bind_event(bind("H", "AMMO", json!([{ "device-type": "keyboard", "zone": "function-keys",
            "color": { "red": 0, "green": 0, "blue": 255 } }]))).unwrap();
        send(&mut r, "G", "HEALTH", 100.0, 0.0).unwrap();
        send(&mut r, "H", "AMMO", 1.0, 1.0).unwrap();
        assert_eq!(r.active_game(2.0), Some("H"));
        assert_eq!(r.paint(2.0).unwrap()[&0x3A], Rgb { r: 0, g: 0, b: 255 });
    }

    #[test]
    fn the_most_recently_changed_event_wins_and_repeats_do_not_count_as_changes() {
        let mut r = with_bar();
        r.bind_event(bind("G", "AMMO", json!([{ "device-type": "keyboard", "zone": "f1",
            "color": { "red": 0, "green": 0, "blue": 255 } }]))).unwrap();
        send(&mut r, "G", "HEALTH", 100.0, 0.0).unwrap();
        send(&mut r, "G", "AMMO", 5.0, 1.0).unwrap();
        assert_eq!(r.paint(1.5).unwrap()[&0x3A], Rgb { r: 0, g: 0, b: 255 }, "AMMO changed last");
        send(&mut r, "G", "HEALTH", 100.0, 2.0).unwrap();
        assert_eq!(r.paint(2.5).unwrap()[&0x3A], Rgb { r: 0, g: 0, b: 255 }, "same HEALTH value: not a change");
        send(&mut r, "G", "HEALTH", 99.0, 3.0).unwrap();
        assert_eq!(r.paint(3.5).unwrap()[&0x3A], Rgb { r: 255, g: 0, b: 0 });
    }

    #[test]
    fn a_game_with_only_unsupported_handlers_is_not_an_overlay() {
        let mut r = Registry::default();
        r.bind_event(bind("G", "SCREEN", json!([{ "device-type": "screened", "zone": "one", "mode": "screen" }]))).unwrap();
        send(&mut r, "G", "SCREEN", 1.0, 0.0).unwrap();
        assert_eq!(r.active_game(0.1), Some("G"));
        assert_eq!(r.paint(0.1), None, "an empty layer would take the keyboard for nothing");
    }

    #[test]
    fn saved_games_restore_with_their_bindings_but_not_their_values() {
        let mut r = with_bar();
        r.game_metadata(serde_json::from_value(json!({ "game": "G", "game_display_name": "Game",
            "developer": "Dev", "deinitialize_timer_length_ms": 5000 })).unwrap()).unwrap();
        r.set_enabled("G", false).unwrap();
        send(&mut r, "G", "HEALTH", 70.0, 0.0).unwrap();
        let saved = r.saved();
        let json = serde_json::to_string(&saved).unwrap();
        let mut back = Registry::restore(serde_json::from_str(&json).unwrap());
        assert_eq!(back.saved(), saved);
        let s = &back.summaries(0.0)[0];
        assert_eq!((s.display_name.as_deref(), s.developer.as_deref(), s.enabled, s.active), (Some("Game"), Some("Dev"), false, false));
        back.set_enabled("G", true).unwrap();
        assert_eq!(back.paint(0.0), None, "values are not saved");
        send(&mut back, "G", "HEALTH", 70.0, 1.0).unwrap();
        assert!(back.paint(1.0).is_some(), "bindings are");
    }

    #[test]
    fn remove_game_and_remove_event() {
        let mut r = with_bar();
        r.remove_event("G", "HEALTH").unwrap();
        assert!(send(&mut r, "G", "HEALTH", 1.0, 0.0).is_err());
        r.remove_game("G").unwrap();
        assert!(r.summaries(0.0).is_empty());
        assert!(r.remove_game("G").is_err());
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gg-gamesense registry`
Expected: FAIL; every test panics at `not yet implemented`.

- [ ] **Step 3: Implement**

```rust
//! Every game that has talked to the server, its events and their handlers,
//! and what the active one paints. Time is passed in (`now`, monotonic seconds),
//! so all of it is testable without a clock.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

use crate::paint::{paint, Reading};
use crate::proto::{valid_name, BindEvent, GameEvent, GameMetadata, Handler, MultipleEvents, RegisterEvent, Rgb};

/// Colours by HID code: what a game paints over the lighting.
pub type Layer = HashMap<u8, Rgb>;

pub const MAX_GAMES: usize = 64;
pub const MAX_EVENTS: usize = 128;
pub const MAX_HANDLERS: usize = 32;
const DEFAULT_TIMER_MS: u32 = 15_000;
const TIMER_MS: std::ops::RangeInclusive<u32> = 1_000..=60_000;

/// An event as saved: its bounds and the handlers as the game sent them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedEvent {
    pub name: String,
    pub min_value: i64,
    pub max_value: i64,
    pub icon_id: u32,
    pub value_optional: bool,
    pub handlers: Vec<Value>,
}

/// A game as saved; its live values are not.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SavedGame {
    pub name: String,
    pub display_name: Option<String>,
    pub developer: Option<String>,
    pub deinitialize_timer_length_ms: u32,
    pub enabled: bool,
    pub events: Vec<SavedEvent>,
}

/// What the UI shows for a game.
#[derive(Debug, Clone, PartialEq)]
pub struct GameSummary {
    pub name: String,
    pub display_name: Option<String>,
    pub developer: Option<String>,
    pub enabled: bool,
    pub active: bool,
    pub events: Vec<String>,
}

struct Event {
    saved: SavedEvent,
    handlers: Vec<Handler>,
    value: Option<i64>,
    frame: Option<Map<String, Value>>,
    received: bool,
    /// When the value last changed (or, with `value_optional`, last arrived).
    changed_at: f64,
}

struct Game {
    display_name: Option<String>,
    developer: Option<String>,
    timer_ms: u32,
    enabled: bool,
    events: BTreeMap<String, Event>,
    last_activity: Option<f64>,
    stopped: bool,
}

#[derive(Default)]
pub struct Registry {
    games: BTreeMap<String, Game>,
}

impl Registry {
    pub fn game_metadata(&mut self, m: GameMetadata) -> Result<(), String> {
        let game = self.game_or_new(&m.game)?;
        if m.game_display_name.is_some() {
            game.display_name = m.game_display_name;
        }
        if m.developer.is_some() {
            game.developer = m.developer;
        }
        if let Some(ms) = m.deinitialize_timer_length_ms {
            game.timer_ms = ms.clamp(*TIMER_MS.start(), *TIMER_MS.end());
        }
        Ok(())
    }

    pub fn register_event(&mut self, r: RegisterEvent) -> Result<(), String> {
        self.define(&r.game, &r.event, r.min_value, r.max_value, r.icon_id, r.value_optional, None)
    }

    pub fn bind_event(&mut self, b: BindEvent) -> Result<(), String> {
        if b.handlers.len() > MAX_HANDLERS {
            return Err(format!("at most {MAX_HANDLERS} handlers per event"));
        }
        let parsed = b.handlers.iter().map(Handler::parse).collect::<Result<Vec<_>, _>>()?;
        self.define(&b.game, &b.event, b.min_value, b.max_value, b.icon_id, b.value_optional, Some((b.handlers, parsed)))
    }

    pub fn game_event(&mut self, e: GameEvent, now: f64) -> Result<(), String> {
        let game = self.games.get_mut(&e.game).ok_or_else(|| format!("unknown game {}", e.game))?;
        apply(game, &e.event, e.data, now)
    }

    pub fn multiple_events(&mut self, m: MultipleEvents, now: f64) -> Result<(), String> {
        let game = self.games.get_mut(&m.game).ok_or_else(|| format!("unknown game {}", m.game))?;
        m.events.into_iter().try_for_each(|item| apply(game, &item.event, item.data, now))
    }

    pub fn heartbeat(&mut self, game: &str, now: f64) -> Result<(), String> {
        let g = self.game_mut(game)?;
        g.last_activity = Some(now);
        g.stopped = false;
        Ok(())
    }

    pub fn stop_game(&mut self, game: &str) -> Result<(), String> {
        self.game_mut(game)?.stopped = true;
        Ok(())
    }

    pub fn remove_game(&mut self, game: &str) -> Result<(), String> {
        self.games.remove(game).map(|_| ()).ok_or_else(|| format!("unknown game {game}"))
    }

    pub fn remove_event(&mut self, game: &str, event: &str) -> Result<(), String> {
        self.game_mut(game)?.events.remove(event).map(|_| ()).ok_or_else(|| format!("unknown event {event}"))
    }

    pub fn set_enabled(&mut self, game: &str, enabled: bool) -> Result<(), String> {
        self.game_mut(game)?.enabled = enabled;
        Ok(())
    }

    /// The enabled, not-stopped game heard from most recently within its timer.
    pub fn active_game(&self, now: f64) -> Option<&str> {
        self.games
            .iter()
            .filter(|(_, g)| g.enabled && !g.stopped)
            .filter_map(|(name, g)| g.last_activity.filter(|&t| now - t <= f64::from(g.timer_ms) / 1000.0).map(|t| (name, t)))
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(name, _)| name.as_str())
    }

    /// What the active game paints now; `None` when no game is active or it paints nothing.
    pub fn paint(&self, now: f64) -> Option<Layer> {
        let game = &self.games[self.active_game(now)?];
        let mut events: Vec<&Event> =
            game.events.values().filter(|e| e.received && (e.value.is_some() || e.saved.value_optional)).collect();
        events.sort_by(|a, b| a.changed_at.total_cmp(&b.changed_at)); // later changes paint over earlier ones
        let mut layer = Layer::new();
        for e in events {
            let reading = Reading {
                value: e.value.unwrap_or(0),
                min: e.saved.min_value,
                max: e.saved.max_value,
                frame: e.frame.as_ref(),
                age: now - e.changed_at,
            };
            for h in &e.handlers {
                layer.extend(paint(h, &reading));
            }
        }
        (!layer.is_empty()).then_some(layer)
    }

    pub fn summaries(&self, now: f64) -> Vec<GameSummary> {
        let active = self.active_game(now);
        self.games
            .iter()
            .map(|(name, g)| GameSummary {
                name: name.clone(),
                display_name: g.display_name.clone(),
                developer: g.developer.clone(),
                enabled: g.enabled,
                active: active == Some(name.as_str()),
                events: g.events.keys().cloned().collect(),
            })
            .collect()
    }

    pub fn saved(&self) -> Vec<SavedGame> {
        self.games
            .iter()
            .map(|(name, g)| SavedGame {
                name: name.clone(),
                display_name: g.display_name.clone(),
                developer: g.developer.clone(),
                deinitialize_timer_length_ms: g.timer_ms,
                enabled: g.enabled,
                events: g.events.values().map(|e| e.saved.clone()).collect(),
            })
            .collect()
    }

    /// Rebuild from saved games. Handlers that no longer parse are dropped.
    pub fn restore(saved: Vec<SavedGame>) -> Registry {
        let games = saved
            .into_iter()
            .filter(|g| valid_name(&g.name))
            .take(MAX_GAMES)
            .map(|g| {
                let events = g
                    .events
                    .into_iter()
                    .filter(|e| valid_name(&e.name))
                    .take(MAX_EVENTS)
                    .map(|e| {
                        let handlers = e.handlers.iter().filter_map(|h| Handler::parse(h).ok()).collect();
                        (e.name.clone(), Event { saved: e, handlers, value: None, frame: None, received: false, changed_at: 0.0 })
                    })
                    .collect();
                let game = Game {
                    display_name: g.display_name,
                    developer: g.developer,
                    timer_ms: g.deinitialize_timer_length_ms.clamp(*TIMER_MS.start(), *TIMER_MS.end()),
                    enabled: g.enabled,
                    events,
                    last_activity: None,
                    stopped: false,
                };
                (g.name, game)
            })
            .collect();
        Registry { games }
    }

    fn game_mut(&mut self, game: &str) -> Result<&mut Game, String> {
        self.games.get_mut(game).ok_or_else(|| format!("unknown game {game}"))
    }

    fn game_or_new(&mut self, name: &str) -> Result<&mut Game, String> {
        if !valid_name(name) {
            return Err(format!("bad game name {name:?}: use A-Z, 0-9, - and _"));
        }
        if !self.games.contains_key(name) && self.games.len() >= MAX_GAMES {
            return Err(format!("at most {MAX_GAMES} games"));
        }
        Ok(self.games.entry(name.to_owned()).or_insert_with(|| Game {
            display_name: None,
            developer: None,
            timer_ms: DEFAULT_TIMER_MS,
            enabled: true,
            events: BTreeMap::new(),
            last_activity: None,
            stopped: false,
        }))
    }

    /// Register or re-register an event, keeping its handlers unless new ones are given.
    #[allow(clippy::too_many_arguments)]
    fn define(
        &mut self,
        game: &str,
        event: &str,
        min: Option<i64>,
        max: Option<i64>,
        icon: Option<u32>,
        value_optional: bool,
        handlers: Option<(Vec<Value>, Vec<Handler>)>,
    ) -> Result<(), String> {
        if !valid_name(event) {
            return Err(format!("bad event name {event:?}: use A-Z, 0-9, - and _"));
        }
        let (min, max) = (min.unwrap_or(0), max.unwrap_or(100));
        if min > max {
            return Err("min_value must not be above max_value".into());
        }
        let g = self.game_or_new(game)?;
        if !g.events.contains_key(event) && g.events.len() >= MAX_EVENTS {
            return Err(format!("at most {MAX_EVENTS} events per game"));
        }
        let e = g.events.entry(event.to_owned()).or_insert_with(|| Event {
            saved: SavedEvent { name: event.to_owned(), min_value: 0, max_value: 100, icon_id: 0, value_optional: false, handlers: Vec::new() },
            handlers: Vec::new(),
            value: None,
            frame: None,
            received: false,
            changed_at: 0.0,
        });
        e.saved.min_value = min;
        e.saved.max_value = max;
        e.saved.icon_id = icon.unwrap_or(0);
        e.saved.value_optional = value_optional;
        if let Some((raw, parsed)) = handlers {
            e.saved.handlers = raw;
            e.handlers = parsed;
        }
        Ok(())
    }
}

/// Record one event's data on its game.
fn apply(game: &mut Game, event: &str, data: crate::proto::EventData, now: f64) -> Result<(), String> {
    let e = game.events.get_mut(event).ok_or_else(|| format!("unknown event {event}; register or bind it first"))?;
    let value = data.value.filter(|v| v.is_finite()).map(|v| v.round() as i64);
    if value.is_none() && !e.saved.value_optional {
        return Err(format!("event {event} needs data.value"));
    }
    if e.saved.value_optional || value != e.value || !e.received {
        e.changed_at = now;
    }
    if value.is_some() {
        e.value = value;
    }
    e.frame = data.frame;
    e.received = true;
    game.last_activity = Some(now);
    game.stopped = false;
    Ok(())
}
```

Add to `lib.rs`: `pub mod registry;` and `pub use registry::{GameSummary, Layer, Registry, SavedEvent, SavedGame};`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p gg-gamesense && cargo clippy -p gg-gamesense --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: all pass (22 earlier + 11 registry); clippy count `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/gg-gamesense
git commit -m "gg-gamesense: the game registry and what the active game paints

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 5: The Proton bridge files (`prefixes`)

**Files:**
- Create: `crates/gg-gamesense/src/prefixes.rs`
- Modify: `crates/gg-gamesense/src/lib.rs` (`pub mod prefixes;`)

**Interfaces:**
- Produces (in `gg_gamesense::prefixes`):
  - `steam_roots(home: &Path) -> Vec<PathBuf>`;
  - `libraries(roots: &[PathBuf]) -> Vec<PathBuf>`;
  - `program_data_dirs(libraries: &[PathBuf]) -> Vec<PathBuf>`;
  - `contents(port: u16) -> String`;
  - `is_ours(text: &str) -> bool`;
  - `enum Written { Wrote, Unchanged, Foreign }`;
  - `write(program_data: &Path, port: u16) -> io::Result<Written>`;
  - `remove(program_data: &Path) -> io::Result<bool>`;
  - `holds_ours(program_data: &Path) -> bool`.

- [ ] **Step 1: Write the failing tests**

`prefixes.rs` with `todo!()` bodies for the functions, then:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// A scratch directory, removed when dropped.
    struct Tmp(PathBuf);

    impl Tmp {
        fn new() -> Tmp {
            static N: AtomicU32 = AtomicU32::new(0);
            let dir = std::env::temp_dir()
                .join(format!("gglinux-prefixes-{}-{}", std::process::id(), N.fetch_add(1, Ordering::Relaxed)));
            std::fs::create_dir_all(&dir).unwrap();
            Tmp(dir)
        }
    }

    impl Drop for Tmp {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    /// A Steam root with a vdf listing itself and `extra`, and prefixes 10 (with ProgramData) and 20 (without).
    fn steam(tmp: &Tmp, extra: &Path) -> PathBuf {
        let root = tmp.0.join("Steam");
        std::fs::create_dir_all(root.join("steamapps/compatdata/10/pfx/drive_c/ProgramData")).unwrap();
        std::fs::create_dir_all(root.join("steamapps/compatdata/20")).unwrap();
        std::fs::write(root.join("steamapps/libraryfolders.vdf"), format!(
            "\"libraryfolders\"\n{{\n\t\"0\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t\t\"label\"\t\t\"\"\n\t}}\n\t\"1\"\n\t{{\n\t\t\"path\"\t\t\"{}\"\n\t}}\n}}\n",
            root.display(), extra.display())).unwrap();
        root
    }

    #[test]
    fn libraries_come_from_the_vdf_deduplicated() {
        let tmp = Tmp::new();
        let other = tmp.0.join("Games");
        std::fs::create_dir_all(other.join("steamapps/compatdata/30/pfx/drive_c/ProgramData")).unwrap();
        let root = steam(&tmp, &other);
        let libs = libraries(&[root.clone(), root.clone()]);
        assert_eq!(libs, vec![root.canonicalize().unwrap(), other.canonicalize().unwrap()]);
        let dirs = program_data_dirs(&libs);
        assert_eq!(dirs.len(), 2, "prefix 20 has no ProgramData yet");
        assert!(dirs.iter().all(|d| d.ends_with("pfx/drive_c/ProgramData")));
    }

    #[test]
    fn no_steam_means_no_libraries_and_no_error() {
        let tmp = Tmp::new();
        assert!(libraries(&[tmp.0.join("nope")]).is_empty());
        assert!(program_data_dirs(&[tmp.0.join("nope")]).is_empty());
        assert!(steam_roots(&tmp.0).is_empty());
    }

    #[test]
    fn writing_is_idempotent_and_follows_the_port() {
        let tmp = Tmp::new();
        let pd = tmp.0.join("ProgramData");
        std::fs::create_dir_all(&pd).unwrap();
        assert!(matches!(write(&pd, 5000).unwrap(), Written::Wrote));
        assert!(matches!(write(&pd, 5000).unwrap(), Written::Unchanged));
        let file = pd.join("SteelSeries/SteelSeries Engine 3/coreProps.json");
        assert_eq!(std::fs::read_to_string(&file).unwrap(), r#"{"address":"127.0.0.1:5000"}"#);
        assert!(matches!(write(&pd, 6000).unwrap(), Written::Wrote), "our own file follows the port");
        assert!(holds_ours(&pd));
    }

    #[test]
    fn a_file_that_is_not_ours_is_never_touched() {
        let tmp = Tmp::new();
        let pd = tmp.0.join("ProgramData");
        let dir = pd.join("SteelSeries/SteelSeries Engine 3");
        std::fs::create_dir_all(&dir).unwrap();
        let gg = r#"{"address":"127.0.0.1:51234","encryptedAddress":"x"}"#;
        std::fs::write(dir.join("coreProps.json"), gg).unwrap();
        assert!(matches!(write(&pd, 5000).unwrap(), Written::Foreign));
        assert!(!remove(&pd).unwrap());
        assert_eq!(std::fs::read_to_string(dir.join("coreProps.json")).unwrap(), gg);
        assert!(!holds_ours(&pd));
    }

    #[test]
    fn removing_takes_our_file_and_empty_directories_only() {
        let tmp = Tmp::new();
        let pd = tmp.0.join("ProgramData");
        std::fs::create_dir_all(&pd).unwrap();
        write(&pd, 5000).unwrap();
        assert!(remove(&pd).unwrap());
        assert!(!pd.join("SteelSeries").exists(), "both directories were ours and empty");
        assert!(!remove(&pd).unwrap(), "nothing left to remove");
        write(&pd, 5000).unwrap();
        std::fs::write(pd.join("SteelSeries/other.txt"), "keep").unwrap();
        assert!(remove(&pd).unwrap());
        assert!(pd.join("SteelSeries/other.txt").exists(), "a directory with other files stays");
    }

    #[test]
    fn ours_means_only_a_loopback_address() {
        assert!(is_ours(&contents(1234)));
        assert!(!is_ours(r#"{"address":"10.0.0.2:1234"}"#));
        assert!(!is_ours(r#"{"address":"127.0.0.1:1","more":1}"#));
        assert!(!is_ours("not json"));
    }
}
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gg-gamesense prefixes`
Expected: FAIL; every test panics at `not yet implemented`.

- [ ] **Step 3: Implement**

```rust
//! The Proton bridge: `coreProps.json` in every Proton prefix, so Windows games
//! under Proton find the server — Wine maps `C:\ProgramData` to the prefix's
//! `drive_c/ProgramData`, and pressure-vessel shares the host network, so the
//! loopback address works from inside. Only files gglinux wrote are ever
//! changed or removed.

use std::io;
use std::path::{Path, PathBuf};

use serde_json::Value;

/// Where GameSense clients look, under `C:\ProgramData`.
const CORE_PROPS: &str = "SteelSeries/SteelSeries Engine 3/coreProps.json";

/// Native Steam's install locations under `home` that exist, deduplicated
/// (`~/.steam/steam` is usually a link to `~/.local/share/Steam`).
pub fn steam_roots(home: &Path) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    for p in [home.join(".local/share/Steam"), home.join(".steam/steam")] {
        if let Ok(c) = p.canonicalize() {
            if !roots.contains(&c) {
                roots.push(c);
            }
        }
    }
    roots
}

/// The library folders Steam lists in `steamapps/libraryfolders.vdf` under each root.
pub fn libraries(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut libs = Vec::new();
    for root in roots {
        let Ok(text) = std::fs::read_to_string(root.join("steamapps/libraryfolders.vdf")) else { continue };
        for path in text.lines().filter_map(vdf_path) {
            let p = PathBuf::from(path);
            let p = p.canonicalize().unwrap_or(p);
            if !libs.contains(&p) {
                libs.push(p);
            }
        }
    }
    libs
}

/// `"path"		"/some/where"` → `/some/where`.
fn vdf_path(line: &str) -> Option<String> {
    let rest = line.trim().strip_prefix("\"path\"")?.trim();
    let inner = rest.strip_prefix('"')?.strip_suffix('"')?;
    Some(inner.replace("\\\\", "\\"))
}

/// `drive_c/ProgramData` of every Proton prefix in these libraries, sorted.
pub fn program_data_dirs(libraries: &[PathBuf]) -> Vec<PathBuf> {
    let mut dirs = Vec::new();
    for lib in libraries {
        let Ok(entries) = std::fs::read_dir(lib.join("steamapps/compatdata")) else { continue };
        for entry in entries.flatten() {
            let pd = entry.path().join("pfx/drive_c/ProgramData");
            if pd.is_dir() {
                dirs.push(pd);
            }
        }
    }
    dirs.sort();
    dirs
}

/// The file gglinux writes.
pub fn contents(port: u16) -> String {
    format!("{{\"address\":\"127.0.0.1:{port}\"}}")
}

/// Whether a coreProps.json is one gglinux wrote: only an `address`, on loopback.
pub fn is_ours(text: &str) -> bool {
    let Ok(Value::Object(o)) = serde_json::from_str::<Value>(text) else { return false };
    o.len() == 1 && o.get("address").and_then(Value::as_str).is_some_and(|a| a.starts_with("127.0.0.1:"))
}

pub enum Written {
    Wrote,
    Unchanged,
    /// Someone else's file (a real GG in the prefix); left alone.
    Foreign,
}

/// Put gglinux's coreProps.json in one prefix's `ProgramData`.
pub fn write(program_data: &Path, port: u16) -> io::Result<Written> {
    let file = program_data.join(CORE_PROPS);
    let want = contents(port);
    match std::fs::read_to_string(&file) {
        Ok(have) if have == want => return Ok(Written::Unchanged),
        Ok(have) if !is_ours(&have) => return Ok(Written::Foreign),
        Ok(_) => {}
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let dir = file.parent().expect("CORE_PROPS has a parent");
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("coreProps.json.tmp");
    std::fs::write(&tmp, &want)?;
    std::fs::rename(&tmp, &file)?;
    Ok(Written::Wrote)
}

/// Remove gglinux's coreProps.json from one prefix, and its two directories
/// if that leaves them empty. `true` if a file was removed.
pub fn remove(program_data: &Path) -> io::Result<bool> {
    let file = program_data.join(CORE_PROPS);
    match std::fs::read_to_string(&file) {
        Ok(text) if is_ours(&text) => {
            std::fs::remove_file(&file)?;
            let engine = file.parent().expect("CORE_PROPS has a parent");
            // `remove_dir` refuses a directory that isn't empty: exactly what we want.
            let _ = std::fs::remove_dir(engine);
            let _ = std::fs::remove_dir(engine.parent().expect("two levels deep"));
            Ok(true)
        }
        Ok(_) => Ok(false),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}

/// Whether this prefix holds gglinux's file right now.
pub fn holds_ours(program_data: &Path) -> bool {
    std::fs::read_to_string(program_data.join(CORE_PROPS)).is_ok_and(|t| is_ours(&t))
}
```

Add `pub mod prefixes;` to `lib.rs`.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test -p gg-gamesense && cargo clippy -p gg-gamesense --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: all pass (33 earlier + 6 prefixes); clippy count `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/gg-gamesense
git commit -m "gg-gamesense: coreProps.json in Proton prefixes, touching only our own files

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 6: GameSense in IPC and the fake daemon

**Files:**
- Modify: `crates/gg-ipc/src/lib.rs` (new types after `KeyboardState`; `Request`, `ResponseData`
  and `Event` variants; tests)
- Modify: `crates/gglinuxd/src/main.rs` (temporary dispatch arms; Task 9 replaces them)
- Modify: `crates/gglinuxd/examples/fakemouse.rs`

**Interfaces:**
- Produces (in `gg_ipc`):
  - `GameInfo { name: String, display_name: Option<String>, developer: Option<String>, enabled: bool, active: bool, events: Vec<String> }`;
  - `GameSenseState { enabled: bool, address: Option<String>, prefixes: u32, games: Vec<GameInfo> }`
    (`Default`);
  - `Request::{GetGamesenseState, SetGamesenseEnabled { enabled }, SetGameEnabled { game, enabled }, RemoveGame { game }, GamesenseTest}`;
  - `ResponseData::Gamesense { state: GameSenseState }`;
  - `Event::GamesenseChanged { state: GameSenseState }`.
  - JSON names: `get_gamesense_state`, `set_gamesense_enabled`, `set_game_enabled`,
    `remove_game`, `gamesense_test`, response type `gamesense`, event `gamesense_changed`.

- [ ] **Step 1: Write the failing test**

Append to `gg-ipc`'s `tests`:

```rust
    #[test]
    fn gamesense_requests_and_events_use_their_documented_json() {
        for (line, want) in [
            (r#"{"id":1,"req":{"method":"get_gamesense_state"}}"#, "GetGamesenseState"),
            (r#"{"id":2,"req":{"method":"set_gamesense_enabled","params":{"enabled":true}}}"#, "SetGamesenseEnabled"),
            (r#"{"id":3,"req":{"method":"set_game_enabled","params":{"game":"G","enabled":false}}}"#, "SetGameEnabled"),
            (r#"{"id":4,"req":{"method":"remove_game","params":{"game":"G"}}}"#, "RemoveGame"),
            (r#"{"id":5,"req":{"method":"gamesense_test"}}"#, "GamesenseTest"),
        ] {
            let req = serde_json::from_str::<RequestFrame>(line).unwrap().req;
            assert!(format!("{req:?}").starts_with(want), "{line} parsed as {req:?}");
        }
        let state = GameSenseState {
            enabled: true,
            address: Some("127.0.0.1:5000".into()),
            prefixes: 17,
            games: vec![GameInfo { name: "G".into(), display_name: None, developer: None, enabled: true, active: false, events: vec!["E".into()] }],
        };
        let v = serde_json::to_value(Event::GamesenseChanged { state: state.clone() }).unwrap();
        assert_eq!(v["event"], "gamesense_changed");
        assert_eq!(v["data"]["state"]["prefixes"], 17);
        let r = serde_json::to_value(ResponseData::Gamesense { state }).unwrap();
        assert_eq!(r["type"], "gamesense");
        assert_eq!(GameSenseState::default(), GameSenseState { enabled: false, address: None, prefixes: 0, games: vec![] });
    }
```

- [ ] **Step 2: Run it to verify it fails**

Run: `cargo test -p gg-ipc gamesense`
Expected: FAIL to compile: `cannot find type GameSenseState` / `GameInfo`, and no variant
`GamesenseChanged` / `Gamesense`.

- [ ] **Step 3: Implement**

In `crates/gg-ipc/src/lib.rs`, after `pub struct KeyboardState { … }`:

```rust
/// A game that has talked to the GameSense server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GameInfo {
    pub name: String,
    pub display_name: Option<String>,
    pub developer: Option<String>,
    /// `false` keeps the game's events off the keyboard.
    pub enabled: bool,
    /// Heard from within its timeout, so drawing now.
    pub active: bool,
    pub events: Vec<String>,
}

/// The GameSense server as the daemon runs it.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct GameSenseState {
    /// Off by default: turning it on writes into Steam's Proton prefixes.
    pub enabled: bool,
    /// `127.0.0.1:<port>` while the server runs.
    pub address: Option<String>,
    /// How many Proton prefixes hold gglinux's coreProps.json.
    pub prefixes: u32,
    pub games: Vec<GameInfo>,
}
```

In `enum Request`, after `SetLighting { lighting: Lighting },`:

```rust
    GetGamesenseState,
    /// Start or stop the GameSense server (and its files in Proton prefixes).
    SetGamesenseEnabled { enabled: bool },
    /// Let one game drive the keyboard, or not.
    SetGameEnabled { game: String, enabled: bool },
    RemoveGame { game: String },
    /// Play the built-in test game: an F-key health bar.
    GamesenseTest,
```

In `enum ResponseData`, after `Keyboard { state: KeyboardState },`: `Gamesense { state: GameSenseState },`.

In `enum Event`, after `KeyboardChanged { … }`:

```rust
    /// GameSense switched on/off, or a game appeared, changed, or went quiet.
    GamesenseChanged { state: GameSenseState },
```

In `crates/gglinuxd/src/main.rs` `dispatch`, before the closing `};` of `let result = match frame.req {`:

```rust
        // Wired to the GameSense controller in the next task.
        Request::GetGamesenseState
        | Request::SetGamesenseEnabled { .. }
        | Request::SetGameEnabled { .. }
        | Request::RemoveGame { .. }
        | Request::GamesenseTest => ResponseData::Error { message: "GameSense is not wired up yet".into() },
```

In `crates/gglinuxd/examples/fakemouse.rs`:
- add `GameInfo, GameSenseState` to the `gg_ipc` import;
- add a `gamesense: GameSenseState` field to `struct Fake`, initialised to `GameSenseState::default()`;
- add these arms to its request `match`:

```rust
        Request::GetGamesenseState => ResponseData::Gamesense { state: s.gamesense.clone() },
        Request::SetGamesenseEnabled { enabled } => {
            s.gamesense.enabled = enabled;
            s.gamesense.address = enabled.then(|| "127.0.0.1:51234".to_string());
            s.gamesense.prefixes = if enabled { 17 } else { 0 };
            ResponseData::Success
        }
        Request::SetGameEnabled { game, enabled } => match s.gamesense.games.iter_mut().find(|g| g.name == game) {
            Some(g) => {
                g.enabled = enabled;
                ResponseData::Success
            }
            None => ResponseData::Error { message: format!("unknown game {game}") },
        },
        Request::RemoveGame { game } => {
            s.gamesense.games.retain(|g| g.name != game);
            ResponseData::Success
        }
        Request::GamesenseTest => {
            if !s.gamesense.games.iter().any(|g| g.name == "GGLINUX_TEST") {
                s.gamesense.games.push(GameInfo {
                    name: "GGLINUX_TEST".into(),
                    display_name: Some("gglinux test".into()),
                    developer: Some("gglinux".into()),
                    enabled: true,
                    active: true,
                    events: vec!["HEALTH".into()],
                });
            }
            ResponseData::Success
        }
```

The example also has a function that names each request for logging, with arms like
`Request::SetLighting { .. } => "set_lighting"`. Add:

```rust
        Request::GetGamesenseState => "get_gamesense_state",
        Request::SetGamesenseEnabled { .. } => "set_gamesense_enabled",
        Request::SetGameEnabled { .. } => "set_game_enabled",
        Request::RemoveGame { .. } => "remove_game",
        Request::GamesenseTest => "gamesense_test",
```

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --all-targets 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6} END{print "passed="p" failed="f}' && cargo clippy --workspace --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: `failed=0`; clippy count `0`.

- [ ] **Step 5: Commit**

```bash
git add crates/gg-ipc/src/lib.rs crates/gglinuxd/src/main.rs crates/gglinuxd/examples/fakemouse.rs
git commit -m "gg-ipc: GameSense state, requests and event

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 7: Overlays and takeover in the keyboard worker

**Files:**
- Modify: `crates/gglinuxd/src/keyboard.rs`
- Modify: `crates/gglinuxd/src/keyboard/effects.rs` (add `dim`)
- Modify: `crates/gglinuxd/src/main.rs` (`keyboard::spawn(event_tx.clone(), Vec::new())`)

**Interfaces:**
- Produces (in `crate::keyboard`):
  - `pub type Layer = std::collections::HashMap<u8, gg_device_keyboard::Rgb>` — the same type as
    `gg_gamesense::Layer`;
  - `pub trait Overlay: Send + Sync { fn paint(&self, now: Instant) -> Option<Layer>; }`;
  - `pub fn spawn(event_tx, overlays: Vec<Arc<dyn Overlay>>) -> Handle`;
  - `Handle::wake(&self)`;
  - `Command::Wake`;
  - pure helpers:
    - `drawing(lighting_enabled: bool, overlay_active: bool) -> bool`;
    - `should_hand_back(taken: bool, lighting_enabled: bool, overlay_active: bool) -> bool`;
    - `overlay_color(layers: &[Layer], code: u8) -> Option<Rgb>`;
    - `wake_forces_frame(last_frame: Option<Instant>) -> bool`;
    - `interval(lighting, live_hits, overlay_active)`.
- In `effects`: `pub fn dim(c: Rgb, brightness: u8) -> Rgb`.

- [ ] **Step 1: Write the failing tests**

In `keyboard.rs`'s `tests` module, change the existing `interval(&x, y)` calls to
`interval(&x, y, false)`, and add:

```rust
    #[test]
    fn gglinux_draws_for_its_own_lighting_or_an_active_overlay_and_hands_back_otherwise() {
        assert!(drawing(true, false));
        assert!(drawing(false, true), "a game takes the keyboard over");
        assert!(!drawing(false, false));
        assert!(should_hand_back(true, false, false), "held, but nothing wants it");
        assert!(!should_hand_back(true, false, true), "a game still draws");
        assert!(!should_hand_back(true, true, false), "gglinux lighting still draws");
        assert!(!should_hand_back(false, false, false), "not held");
    }

    #[test]
    fn the_last_active_overlay_that_paints_a_key_wins() {
        let red = gg_device_keyboard::Rgb { r: 255, g: 0, b: 0 };
        let blue = gg_device_keyboard::Rgb { r: 0, g: 0, b: 255 };
        let layers = vec![Layer::from([(0x04, red), (0x05, red)]), Layer::from([(0x05, blue)])];
        assert_eq!(overlay_color(&layers, 0x04), Some(red));
        assert_eq!(overlay_color(&layers, 0x05), Some(blue));
        assert_eq!(overlay_color(&layers, 0x06), None);
    }

    #[test]
    fn an_active_overlay_animates_even_a_static_effect() {
        let still = lighting(true, false, Effect::Static);
        assert_eq!(interval(&still, false, true), FRAME);
        assert_eq!(interval(&still, false, false), STATIC_REFRESH);
    }

    #[test]
    fn wake_forces_a_frame_at_most_once_per_frame_interval() {
        assert!(wake_forces_frame(None));
        assert!(!wake_forces_frame(Some(Instant::now())), "a frame just went out: wait for the cadence");
        assert!(wake_forces_frame(Some(Instant::now() - FRAME)));
    }
```

And in `effects.rs` tests:

```rust
    #[test]
    fn dim_applies_the_brightness_to_overlay_colours() {
        assert_eq!(dim(Rgb { r: 200, g: 100, b: 0 }, 50), Rgb { r: 100, g: 50, b: 0 });
        assert_eq!(dim(Rgb { r: 200, g: 100, b: 0 }, 100), Rgb { r: 200, g: 100, b: 0 });
    }
```

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gglinuxd keyboard 2>&1 | grep -E "^error" | sort | uniq -c`
Expected: compile errors: cannot find `drawing`, `should_hand_back`, `overlay_color`, `Layer`,
`wake_forces_frame`, `dim`; `interval` takes 2 arguments, not 3.

- [ ] **Step 3: Implement**

`effects.rs`, after `animated`:

```rust
/// An overlay's colour at the lighting's brightness.
pub fn dim(c: Rgb, brightness: u8) -> Rgb {
    scale(c, f32::from(brightness) / 100.0)
}
```

`keyboard.rs`:

1. Imports: add `use std::sync::Arc;` and `use gg_device_keyboard::Rgb;`.
2. After the constants:

```rust
/// Colours by HID code, drawn over the lighting.
pub type Layer = HashMap<u8, Rgb>;

/// Something that paints over the lighting while it's active: GameSense now,
/// built-in effects later. Asked once per loop, so keep `paint` cheap.
pub trait Overlay: Send + Sync {
    /// The keys it paints at `now`, or `None` while inactive.
    fn paint(&self, now: Instant) -> Option<Layer>;
}
```

3. `enum Command` gains, before `Shutdown`:

```rust
    /// Something an overlay shows changed: draw soon. No reply.
    Wake,
```

   `impl Debug for Command` gains the arm `Command::Wake => "Wake",`.
4. `Handle` gains:

```rust
    /// Ask for a prompt frame; fire and forget.
    pub fn wake(&self) {
        let _ = self.tx.send(Command::Wake);
    }
```

5. `spawn(event_tx: broadcast::Sender<Event>, overlays: Vec<Arc<dyn Overlay>>) -> Handle` passes
   `overlays` into `worker(rx, readers_tx, event_tx, overlays)`.
6. `State` gains:

```rust
    /// Painters over the lighting, and what they painted this loop.
    overlays: Vec<Arc<dyn Overlay>>,
    layers: Vec<Layer>,
```

   Initialise them with `overlays, layers: Vec::new()`.
7. The loop body after the `match` becomes:

```rust
        if state.device.is_none() {
            try_open(&mut state);
        }
        let now = Instant::now();
        state.layers = state.overlays.iter().filter_map(|o| o.paint(now)).collect();
        sync_reader(&mut state);
        if should_hand_back(state.taken, state.lighting.enabled, !state.layers.is_empty()) {
            hand_back(&mut state);
        }
        render(&mut state);
        publish(&mut state, &event_tx);
```

8. Helpers, after `push_hit`:

```rust
/// Whether gglinux draws on the keyboard: its own lighting, or an overlay taking it over.
fn drawing(lighting_enabled: bool, overlay_active: bool) -> bool {
    lighting_enabled || overlay_active
}

/// Give the keyboard back: gglinux holds it, but nothing wants to draw any more.
fn should_hand_back(taken: bool, lighting_enabled: bool, overlay_active: bool) -> bool {
    taken && !drawing(lighting_enabled, overlay_active)
}

/// The colour overlays give this key: the last active overlay that paints it.
fn overlay_color(layers: &[Layer], code: u8) -> Option<Rgb> {
    layers.iter().rev().find_map(|l| l.get(&code).copied())
}

/// A wake draws at once only if the last frame is at least a frame old, so a
/// game sending hundreds of events a second still gets 30 frames, not hundreds.
fn wake_forces_frame(last_frame: Option<Instant>) -> bool {
    is_older_than(last_frame, FRAME)
}
```

9. `interval(lighting: &Lighting, live_hits: bool, overlay_active: bool)` returns `FRAME` when
   `effects::animated(&lighting.effect) || live_hits || overlay_active`. Update its two callers:
   in `next_wait` and `render`, the third argument is `!state.layers.is_empty()`.
10. In `next_wait`, the early return becomes
    `if state.device.is_none() || !drawing(state.lighting.enabled, !state.layers.is_empty()) { return IDLE_WAIT; }`.
11. In `handle`:
   - `SetLighting`: remove the `turned_off` variable and its `if turned_off { hand_back(state); }`
     (the loop's `should_hand_back` now decides, so an active game isn't interrupted);
   - add the arm:

```rust
        Command::Wake => {
            if wake_forces_frame(state.last_frame) {
                state.last_frame = None;
            }
        }
```

12. In `render`:
   - The guard becomes
     `if !drawing(state.lighting.enabled, !state.layers.is_empty()) || !is_older_than(state.last_frame, interval(&state.lighting, !state.hits.is_empty(), !state.layers.is_empty())) { return; }`.
   - After `let hits = &state.hits;`, add `let layers = &state.layers;`.
   - The frame call becomes:

```rust
        .and_then(|()| device.show(|code| match overlay_color(layers, code) {
            Some(c) => effects::dim(c, state.lighting.brightness),
            None => effects::color(&state.lighting, &state.positions, code, t, hits),
        }));
```

13. `main.rs`: `keyboard: keyboard::spawn(event_tx.clone(), Vec::new()),`. Task 9 passes the
    GameSense overlay.

- [ ] **Step 4: Run the tests to verify they pass**

Run: `cargo test --workspace --all-targets 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6} END{print "passed="p" failed="f}' && cargo clippy --workspace --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | head -20`
Expected: `failed=0`; no clippy output. `Overlay` and `Handle::wake` have no caller until Task 9.
If rustc reports them as `dead_code`, add `#[allow(dead_code)] // used from Task 9` on each, and
ledger it.

- [ ] **Step 5: Commit**

```bash
git add crates/gglinuxd/src/keyboard.rs crates/gglinuxd/src/keyboard/effects.rs crates/gglinuxd/src/main.rs
git commit -m "gglinuxd: overlays over the keyboard lighting, and taking it over for them

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 8: The GameSense HTTP server

**Files:**
- Create: `crates/gglinuxd/src/gamesense/server.rs`
- Create: `crates/gglinuxd/src/gamesense.rs` (for now only `pub mod server;`, `Clock` and
  `Change`; Task 9 fills in the rest)
- Modify: `crates/gglinuxd/src/main.rs` (`mod gamesense;`)
- Modify: `crates/gglinuxd/Cargo.toml`

**Interfaces:**
- Consumes: `gg_gamesense::{proto, Registry}` (Tasks 2 and 4).
- Produces:
  - `gamesense::Clock` (`Copy`) with `start()`, `now(&self) -> f64` and
    `at(&self, t: Instant) -> f64`;
  - `gamesense::Change { Live, Structure }`;
  - `gamesense::server::{Shared, router(Shared) -> axum::Router, BODY_LIMIT}`, where
    `Shared { registry: Arc<Mutex<Registry>>, clock: Clock, changed: Arc<dyn Fn(Change) + Send + Sync> }`
    (`Clone`).

- [ ] **Step 1: Dependencies and the shared bits**

`crates/gglinuxd/Cargo.toml` `[dependencies]`: add `gg-gamesense = { path = "../gg-gamesense" }`,
`axum.workspace = true`, `serde.workspace = true`. Add:

```toml
[dev-dependencies]
tower.workspace = true
http-body-util.workspace = true
```

`crates/gglinuxd/src/gamesense.rs`:

```rust
//! GameSense: the SDK server, its files in Proton prefixes, and the games it
//! lets draw on the keyboard.

pub mod server;

use std::time::Instant;

/// Monotonic seconds since the daemon started: the registry's `now`.
#[derive(Clone, Copy)]
pub struct Clock(Instant);

impl Clock {
    /// Seconds count from now.
    pub fn start() -> Clock {
        Clock(Instant::now())
    }

    pub fn now(&self) -> f64 {
        self.0.elapsed().as_secs_f64()
    }

    /// The same scale for a given instant.
    pub fn at(&self, t: Instant) -> f64 {
        t.saturating_duration_since(self.0).as_secs_f64()
    }
}

/// What a request changed: live values only, or something worth saving.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Change {
    Live,
    Structure,
}
```

`main.rs`: add `mod gamesense;` under `mod clock;`.

- [ ] **Step 2: Write the failing tests**

`crates/gglinuxd/src/gamesense/server.rs`, with stub `router` (`todo!()`) and the `Shared` struct,
then:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::http::Request as HttpRequest;
    use http_body_util::BodyExt;
    use tower::ServiceExt;

    fn shared() -> (Shared, Arc<Mutex<Vec<Change>>>) {
        let seen = Arc::new(Mutex::new(Vec::new()));
        let log = Arc::clone(&seen);
        let s = Shared {
            registry: Arc::new(Mutex::new(Registry::default())),
            clock: Clock::start(),
            changed: Arc::new(move |c| log.lock().unwrap().push(c)),
        };
        (s, seen)
    }

    async fn call(app: &Router, method: &str, path: &str, headers: &[(&str, &str)], body: String) -> (StatusCode, String) {
        let mut req = HttpRequest::builder().method(method).uri(path);
        for (k, v) in headers {
            req = req.header(*k, *v);
        }
        let res = app.clone().oneshot(req.body(Body::from(body)).unwrap()).await.unwrap();
        let status = res.status();
        let bytes = res.into_body().collect().await.unwrap().to_bytes();
        (status, String::from_utf8_lossy(&bytes).into_owned())
    }

    async fn post(app: &Router, path: &str, body: &str) -> (StatusCode, String) {
        call(app, "POST", path, &[("content-type", "application/json")], body.to_string()).await
    }

    const BIND: &str = r#"{"game":"G","event":"HEALTH","handlers":[{"device-type":"keyboard",
        "zone":"function-keys","mode":"percent","color":{"red":255,"green":0,"blue":0}}]}"#;

    #[tokio::test]
    async fn web_pages_are_refused() {
        let (s, _) = shared();
        let app = router(s);
        let (status, body) = call(&app, "POST", "/game_event",
            &[("content-type", "application/json"), ("origin", "https://example.com")], "{}".into()).await;
        assert_eq!(status, StatusCode::FORBIDDEN);
        assert!(body.contains("error"));
        let (status, _) = call(&app, "POST", "/game_event", &[("content-type", "text/plain")], "{}".into()).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE, "a no-cors fetch can only send text/plain");
        let (status, _) = call(&app, "POST", "/game_event", &[], "{}".into()).await;
        assert_eq!(status, StatusCode::UNSUPPORTED_MEDIA_TYPE);
    }

    #[tokio::test]
    async fn oversized_bodies_are_refused() {
        let (s, _) = shared();
        let (status, _) = post(&router(s), "/game_event", &" ".repeat(BODY_LIMIT + 1)).await;
        assert_eq!(status, StatusCode::PAYLOAD_TOO_LARGE);
    }

    #[tokio::test]
    async fn register_bind_and_send_paint_the_keyboard() {
        let (s, seen) = shared();
        let app = router(s.clone());
        assert_eq!(post(&app, "/game_metadata", r#"{"game":"G","game_display_name":"Game"}"#).await, (StatusCode::OK, "{}".into()));
        assert_eq!(post(&app, "/bind_game_event", BIND).await.0, StatusCode::OK);
        assert_eq!(post(&app, "/game_event", r#"{"game":"G","event":"HEALTH","data":{"value":100}}"#).await.0, StatusCode::OK);
        let layer = s.registry.lock().unwrap().paint(s.clock.now()).expect("G paints");
        assert_eq!(layer.len(), 12);
        assert_eq!(*seen.lock().unwrap(), vec![Change::Structure, Change::Structure, Change::Live]);
    }

    #[tokio::test]
    async fn mistakes_are_400_with_an_error_message() {
        let (s, seen) = shared();
        let app = router(s);
        let (status, body) = post(&app, "/game_event", "{not json").await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("\"error\""), "{body}");
        let (status, body) = post(&app, "/game_event", r#"{"game":"G","event":"E","data":{"value":1}}"#).await;
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.contains("unknown game"), "{body}");
        assert!(seen.lock().unwrap().is_empty(), "failed requests change nothing");
    }

    #[tokio::test]
    async fn heartbeats_keep_a_game_up_and_stop_game_ends_it() {
        let (s, _) = shared();
        let app = router(s.clone());
        post(&app, "/bind_game_event", BIND).await;
        post(&app, "/game_event", r#"{"game":"G","event":"HEALTH","data":{"value":50}}"#).await;
        assert_eq!(post(&app, "/game_heartbeat", r#"{"game":"G"}"#).await.0, StatusCode::OK);
        assert_eq!(s.registry.lock().unwrap().active_game(s.clock.now()), Some("G"));
        assert_eq!(post(&app, "/stop_game", r#"{"game":"G"}"#).await.0, StatusCode::OK);
        assert_eq!(s.registry.lock().unwrap().paint(s.clock.now()), None);
    }

    #[tokio::test]
    async fn multiple_events_and_the_capability_probe() {
        let (s, _) = shared();
        let app = router(s.clone());
        post(&app, "/bind_game_event", BIND).await;
        let (status, _) = post(&app, "/multiple_game_events",
            r#"{"game":"G","events":[{"event":"HEALTH","data":{"value":10}},{"event":"HEALTH","data":{"value":90}}]}"#).await;
        assert_eq!(status, StatusCode::OK);
        let (status, body) = call(&app, "GET", "/supports_multiple_game_events", &[], String::new()).await;
        assert_eq!((status, body.as_str()), (StatusCode::OK, "{}"));
    }
}
```

- [ ] **Step 3: Run the tests to verify they fail**

Run: `cargo test -p gglinuxd gamesense::server`
Expected: FAIL; the tests panic at `not yet implemented` (inside `router`).

- [ ] **Step 4: Implement**

```rust
//! The GameSense HTTP endpoints, on loopback only, and closed to web pages:
//! a request carrying an `Origin` header is refused, and every POST must be
//! `application/json`, which a browser can't send cross-origin without a
//! CORS preflight this server never approves.

use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{DefaultBodyLimit, Request, State};
use axum::http::{header, Method, StatusCode};
use axum::middleware::{self, Next};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use gg_gamesense::{proto, Registry};
use serde::de::DeserializeOwned;
use serde_json::json;

use super::{Change, Clock};

/// Largest request body accepted.
pub const BODY_LIMIT: usize = 64 * 1024;

/// What the endpoints share.
#[derive(Clone)]
pub struct Shared {
    pub registry: Arc<Mutex<Registry>>,
    pub clock: Clock,
    /// Called after every accepted request: wake the keyboard, publish, save.
    pub changed: Arc<dyn Fn(Change) + Send + Sync>,
}

pub fn router(shared: Shared) -> Router {
    Router::new()
        .route("/game_metadata", post(game_metadata))
        .route("/register_game_event", post(register_game_event))
        .route("/bind_game_event", post(bind_game_event))
        .route("/game_event", post(game_event))
        .route("/multiple_game_events", post(multiple_game_events))
        .route("/game_heartbeat", post(game_heartbeat))
        .route("/stop_game", post(stop_game))
        .route("/remove_game", post(remove_game))
        .route("/remove_game_event", post(remove_game_event))
        .route("/supports_multiple_game_events", get(|| async { Json(json!({})) }))
        .layer(middleware::from_fn(guard))
        .layer(DefaultBodyLimit::max(BODY_LIMIT))
        .with_state(shared)
}

async fn guard(req: Request, next: Next) -> Response {
    if req.headers().contains_key(header::ORIGIN) {
        return error(StatusCode::FORBIDDEN, "requests from web pages are not accepted");
    }
    if req.method() == Method::POST {
        let json = req
            .headers()
            .get(header::CONTENT_TYPE)
            .and_then(|v| v.to_str().ok())
            .and_then(|v| v.split(';').next())
            .is_some_and(|mime| mime.trim().eq_ignore_ascii_case("application/json"));
        if !json {
            return error(StatusCode::UNSUPPORTED_MEDIA_TYPE, "Content-Type must be application/json");
        }
    }
    next.run(req).await
}

fn error(status: StatusCode, message: &str) -> Response {
    (status, Json(json!({ "error": message }))).into_response()
}

/// Parse the body, apply it to the registry, and answer.
fn apply<T: DeserializeOwned>(
    s: &Shared,
    body: &Bytes,
    change: Change,
    f: impl FnOnce(&mut Registry, T, f64) -> Result<(), String>,
) -> Response {
    let req: T = match serde_json::from_slice(body) {
        Ok(req) => req,
        Err(e) => return error(StatusCode::BAD_REQUEST, &format!("invalid request: {e}")),
    };
    let result = {
        let mut registry = s.registry.lock().unwrap_or_else(|e| e.into_inner());
        f(&mut registry, req, s.clock.now())
    };
    match result {
        Ok(()) => {
            (s.changed)(change);
            Json(json!({})).into_response()
        }
        Err(message) => error(StatusCode::BAD_REQUEST, &message),
    }
}

async fn game_metadata(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, m: proto::GameMetadata, _| r.game_metadata(m))
}

async fn register_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, e: proto::RegisterEvent, _| r.register_event(e))
}

async fn bind_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, e: proto::BindEvent, _| r.bind_event(e))
}

async fn game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, e: proto::GameEvent, now| r.game_event(e, now))
}

async fn multiple_game_events(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, m: proto::MultipleEvents, now| r.multiple_events(m, now))
}

async fn game_heartbeat(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, g: proto::GameOnly, now| r.heartbeat(&g.game, now))
}

async fn stop_game(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Live, |r, g: proto::GameOnly, _| r.stop_game(&g.game))
}

async fn remove_game(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, g: proto::GameOnly, _| r.remove_game(&g.game))
}

async fn remove_game_event(State(s): State<Shared>, body: Bytes) -> Response {
    apply(&s, &body, Change::Structure, |r, e: proto::RemoveEvent, _| r.remove_event(&e.game, &e.event))
}
```

At the top of the test module add `use gg_gamesense::Registry;` and
`use crate::gamesense::{Change, Clock};` if the test code doesn't see them through `super::*`.

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test -p gglinuxd gamesense::server && cargo clippy -p gglinuxd --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | head -20`
Expected: 6 passed. Clippy may report `dead_code` for `router`/`Shared` (used from Task 9) and
`Clock::at`. If so, add `#[allow(dead_code)] // used from Task 9` to `mod gamesense;` in
`main.rs`, and ledger it.

- [ ] **Step 6: Commit**

```bash
git add crates/gglinuxd/src/gamesense.rs crates/gglinuxd/src/gamesense crates/gglinuxd/src/main.rs crates/gglinuxd/Cargo.toml Cargo.lock
git commit -m "gglinuxd: the GameSense HTTP endpoints, closed to web pages

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 9: The GameSense controller, test game and wiring

**Files:**
- Modify: `crates/gglinuxd/src/gamesense.rs`
- Modify: `crates/gglinuxd/src/main.rs` (`Workers.gamesense`; engine before the keyboard; dispatch)

**Interfaces:**
- Consumes:
  - `server::{Shared, router}`, `Clock`, `Change` (Task 8);
  - `keyboard::{Overlay, Layer, Handle::wake}` (Task 7);
  - `gg_gamesense::{Registry, SavedGame, prefixes}` (Tasks 4 and 5);
  - `gg_ipc::{GameSenseState, GameInfo, Event::GamesenseChanged}` (Task 6).
- Produces (in `crate::gamesense`):
  - `Engine::load() -> Engine` and `Engine::overlay(&self) -> Arc<dyn Overlay>`;
  - `spawn(engine, event_tx, keyboard: keyboard::Handle) -> Handle`;
  - `Handle::send` (same shape as the other workers);
  - `Command::{GetState, SetEnabled { enabled, reply }, SetGameEnabled { game, enabled, reply }, RemoveGame { game, reply }, RunTest(reply)}`;
  - pure helpers:
    - `Settings { enabled, port, games }` (serde) and `load_settings(&Path) -> Settings`;
    - `test_values() -> Vec<i64>`;
    - `newly_failed(seen: &mut HashSet<PathBuf>, failed: &[PathBuf]) -> Vec<PathBuf>`;
    - `async fn bind_loopback(port: Option<u16>) -> io::Result<TcpListener>`.

- [ ] **Step 1: Write the failing tests**

Append to `gamesense.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn scratch(name: &str) -> PathBuf {
        let dir = std::env::temp_dir().join(format!("gglinux-gs-{}-{name}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn unreadable_settings_fall_back_to_off() {
        let dir = scratch("settings");
        let path = dir.join("gamesense.json");
        assert_eq!(load_settings(&path), Settings::default(), "missing file");
        std::fs::write(&path, "{ not json").unwrap();
        assert_eq!(load_settings(&path), Settings::default(), "corrupted file");
        assert!(!Settings::default().enabled, "GameSense is off until asked");
        let s = Settings { enabled: true, port: Some(5000), games: Vec::new() };
        save_settings(&path, &s);
        assert_eq!(load_settings(&path), s);
        std::fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn the_test_game_sweeps_down_and_back_up() {
        let v = test_values();
        assert_eq!(v.len(), 201);
        assert_eq!((v[0], v[100], v[200]), (100, 0, 100));
    }

    #[test]
    fn the_test_game_draws_a_red_to_green_health_bar_on_the_f_keys() {
        let mut r = Registry::default();
        define_test_game(&mut r).unwrap();
        send_test_value(&mut r, 100, 0.0).unwrap();
        let full = r.paint(0.0).unwrap();
        assert_eq!(full.len(), 12);
        assert_eq!(full[&0x45], gg_device_keyboard::Rgb { r: 0, g: 255, b: 0 }, "F12 green at 100");
        send_test_value(&mut r, 10, 1.0).unwrap();
        let low = r.paint(1.0).unwrap();
        assert_eq!(low[&0x45], gg_device_keyboard::Rgb::default(), "F12 dark at 10%");
        assert_ne!(low[&0x3A], gg_device_keyboard::Rgb::default(), "F1 lit (first half of a flash)");
        assert_eq!(r.paint(1.2).unwrap()[&0x3A], gg_device_keyboard::Rgb::default(), "…and dark in the second half at 4 Hz");
    }

    #[test]
    fn a_failing_prefix_is_reported_once() {
        let mut seen = HashSet::new();
        let a = PathBuf::from("/a");
        let b = PathBuf::from("/b");
        assert_eq!(newly_failed(&mut seen, &[a.clone()]), vec![a.clone()]);
        assert!(newly_failed(&mut seen, &[a.clone()]).is_empty(), "already warned");
        assert_eq!(newly_failed(&mut seen, &[a.clone(), b.clone()]), vec![b]);
        assert!(newly_failed(&mut seen, &[]).is_empty());
        assert_eq!(newly_failed(&mut seen, &[a.clone()]), vec![a], "fixed, then broken again: warn again");
    }

    #[tokio::test]
    async fn a_taken_port_falls_back_to_a_free_one() {
        let held = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let taken = held.local_addr().unwrap().port();
        let l = bind_loopback(Some(taken)).await.unwrap();
        let addr = l.local_addr().unwrap();
        assert!(addr.ip().is_loopback());
        assert_ne!(addr.port(), taken);
        let fresh = bind_loopback(None).await.unwrap();
        assert!(fresh.local_addr().unwrap().ip().is_loopback());
    }

    #[test]
    fn summaries_become_ipc_game_infos() {
        let mut r = Registry::default();
        define_test_game(&mut r).unwrap();
        let infos = game_infos(&r, 0.0);
        assert_eq!(infos[0].name, "GGLINUX_TEST");
        assert_eq!(infos[0].display_name.as_deref(), Some("gglinux test"));
        assert_eq!(infos[0].events, vec!["HEALTH".to_string()]);
    }
}
```

For this step, add stubs (`todo!()` bodies) for `load_settings`, `save_settings`, `test_values`,
`define_test_game`, `send_test_value`, `newly_failed`, `bind_loopback` and `game_infos`, plus the
`Settings` struct (derives `Debug, Clone, Default, PartialEq, Serialize, Deserialize`) and the
needed `use` lines.

- [ ] **Step 2: Run the tests to verify they fail**

Run: `cargo test -p gglinuxd gamesense::tests`
Expected: FAIL; the tests panic at `not yet implemented`.

- [ ] **Step 3: Implement the controller**

Complete `gamesense.rs`. It keeps `Clock` and `Change` from Task 8:

```rust
//! GameSense: the SDK server, its files in Proton prefixes, and the games it
//! lets draw on the keyboard. A tokio task owns the on/off switch, the server,
//! the prefix scan and the settings file; the registry is shared with the
//! server's handlers and with the keyboard worker (through [`Overlay`]).

pub mod server;

use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use gg_gamesense::prefixes::{self, Written};
use gg_gamesense::{proto, Registry, SavedGame};
use gg_ipc::{Event, GameInfo, GameSenseState};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::sync::{broadcast, mpsc, oneshot};
use tokio::task::JoinHandle;
use tracing::{debug, info, warn};

use crate::keyboard::{self, Layer, Overlay};

const SCAN_EVERY: Duration = Duration::from_secs(1);
/// Scans between re-reads of `libraryfolders.vdf` (30 s at one scan a second).
const LIBRARY_REFRESH_SCANS: u32 = 30;
const TEST_GAME: &str = "GGLINUX_TEST";
const TEST_STEP: Duration = Duration::from_millis(100);

type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<GameSenseState>),
    SetEnabled { enabled: bool, reply: Reply<()> },
    SetGameEnabled { game: String, enabled: bool, reply: Reply<()> },
    RemoveGame { game: String, reply: Reply<()> },
    RunTest(Reply<()>),
}

#[derive(Clone)]
pub struct Handle {
    tx: mpsc::UnboundedSender<Command>,
}

impl Handle {
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx.send(make(reply_tx)).map_err(|_| "GameSense is not running".to_string())?;
        reply_rx.await.map_err(|_| "GameSense dropped the request".to_string())?
    }
}

/// `~/.config/gglinux/gamesense.json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct Settings {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default)]
    pub port: Option<u16>,
    #[serde(default)]
    pub games: Vec<SavedGame>,
}

/// Saved settings; a missing or unreadable file means GameSense off.
pub fn load_settings(path: &Path) -> Settings {
    match std::fs::read_to_string(path) {
        Ok(text) => serde_json::from_str(&text).unwrap_or_else(|e| {
            warn!(path = %path.display(), error = %e, "ignoring unreadable GameSense settings");
            Settings::default()
        }),
        Err(_) => Settings::default(),
    }
}

/// Save atomically; a failure is logged, never fatal.
pub fn save_settings(path: &Path, settings: &Settings) {
    let write = || -> anyhow::Result<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir)?;
        }
        let tmp = path.with_extension("json.tmp");
        std::fs::write(&tmp, serde_json::to_string_pretty(settings)?)?;
        std::fs::rename(&tmp, path)?;
        Ok(())
    };
    if let Err(e) = write() {
        warn!(path = %path.display(), error = %format!("{e:#}"), "could not save GameSense settings");
    }
}

/// The registry and clock, made before the keyboard worker so it can be given the overlay.
pub struct Engine {
    registry: Arc<Mutex<Registry>>,
    clock: Clock,
    settings_path: PathBuf,
    settings: Settings,
}

impl Engine {
    pub fn load() -> Engine {
        let settings_path = gg_ipc::config_dir().join("gamesense.json");
        let settings = load_settings(&settings_path);
        let registry = Registry::restore(settings.games.clone());
        Engine { registry: Arc::new(Mutex::new(registry)), clock: Clock::start(), settings_path, settings }
    }

    pub fn overlay(&self) -> Arc<dyn Overlay> {
        Arc::new(GameOverlay { registry: Arc::clone(&self.registry), clock: self.clock })
    }
}

struct GameOverlay {
    registry: Arc<Mutex<Registry>>,
    clock: Clock,
}

impl Overlay for GameOverlay {
    fn paint(&self, now: Instant) -> Option<Layer> {
        self.registry.lock().unwrap_or_else(|e| e.into_inner()).paint(self.clock.at(now))
    }
}

/// Bind on loopback: the saved port if it's free, else any free one.
pub async fn bind_loopback(port: Option<u16>) -> std::io::Result<TcpListener> {
    if let Some(p) = port {
        match TcpListener::bind(("127.0.0.1", p)).await {
            Ok(l) => return Ok(l),
            Err(e) => info!(port = p, error = %e, "saved GameSense port is taken; picking another"),
        }
    }
    TcpListener::bind(("127.0.0.1", 0)).await
}

/// Prefixes that failed this scan and didn't last time, so each is warned about once.
pub fn newly_failed(seen: &mut HashSet<PathBuf>, failed: &[PathBuf]) -> Vec<PathBuf> {
    let now: HashSet<PathBuf> = failed.iter().cloned().collect();
    let new = failed.iter().filter(|p| !seen.contains(*p)).cloned().collect();
    *seen = now;
    new
}

/// 100 → 0 → 100, one value per step.
pub fn test_values() -> Vec<i64> {
    (0..=100).rev().chain(1..=100).collect()
}

fn define_test_game(r: &mut Registry) -> Result<(), String> {
    r.game_metadata(proto::GameMetadata {
        game: TEST_GAME.into(),
        game_display_name: Some("gglinux test".into()),
        developer: Some("gglinux".into()),
        deinitialize_timer_length_ms: None,
    })?;
    r.bind_event(serde_json::from_value(serde_json::json!({
        "game": TEST_GAME, "event": "HEALTH", "min_value": 0, "max_value": 100,
        "handlers": [{
            "device-type": "keyboard", "zone": "function-keys", "mode": "percent",
            "color": { "gradient": { "zero": { "red": 255, "green": 0, "blue": 0 },
                                     "hundred": { "red": 0, "green": 255, "blue": 0 } } },
            "rate": { "frequency": [{ "low": 0, "high": 20, "frequency": 4 }] }
        }]
    })).map_err(|e| e.to_string())?)
}

fn send_test_value(r: &mut Registry, value: i64, now: f64) -> Result<(), String> {
    r.game_event(proto::GameEvent {
        game: TEST_GAME.into(),
        event: "HEALTH".into(),
        data: proto::EventData { value: Some(value as f64), frame: None },
    }, now)
}

fn game_infos(r: &Registry, now: f64) -> Vec<GameInfo> {
    r.summaries(now)
        .into_iter()
        .map(|s| GameInfo {
            name: s.name,
            display_name: s.display_name,
            developer: s.developer,
            enabled: s.enabled,
            active: s.active,
            events: s.events,
        })
        .collect()
}

struct Running {
    port: u16,
    shutdown: oneshot::Sender<()>,
    task: JoinHandle<()>,
}

struct Controller {
    engine: Engine,
    event_tx: broadcast::Sender<Event>,
    keyboard: keyboard::Handle,
    changes: mpsc::UnboundedSender<Change>,
    running: Option<Running>,
    libraries: Vec<PathBuf>,
    scans: u32,
    holding: u32,
    failed: HashSet<PathBuf>,
    test: Option<JoinHandle<()>>,
    published: Option<GameSenseState>,
}

pub fn spawn(engine: Engine, event_tx: broadcast::Sender<Event>, keyboard: keyboard::Handle) -> Handle {
    let (tx, rx) = mpsc::unbounded_channel();
    let (changes, changes_rx) = mpsc::unbounded_channel();
    let c = Controller {
        engine,
        event_tx,
        keyboard,
        changes,
        running: None,
        libraries: Vec::new(),
        scans: 0,
        holding: 0,
        failed: HashSet::new(),
        test: None,
        published: None,
    };
    tokio::spawn(run(c, rx, changes_rx));
    Handle { tx }
}

async fn run(mut c: Controller, mut rx: mpsc::UnboundedReceiver<Command>, mut changes: mpsc::UnboundedReceiver<Change>) {
    if c.engine.settings.enabled {
        if let Err(e) = c.start().await {
            warn!(error = %e, "could not start the GameSense server");
        }
    }
    let mut tick = tokio::time::interval(SCAN_EVERY);
    loop {
        tokio::select! {
            command = rx.recv() => match command {
                Some(command) => c.handle(command).await,
                None => break,
            },
            Some(change) = changes.recv() => {
                if change == Change::Structure {
                    c.save();
                }
                c.publish();
            }
            _ = tick.tick() => {
                if c.running.is_some() {
                    c.scan().await;
                }
                c.publish(); // games go quiet with time
            }
        }
    }
}

impl Controller {
    async fn handle(&mut self, command: Command) {
        match command {
            Command::GetState(reply) => {
                let _ = reply.send(Ok(self.state()));
            }
            Command::SetEnabled { enabled, reply } => {
                let result = if enabled { self.start().await } else { self.stop().await };
                let _ = reply.send(result);
                self.publish();
            }
            Command::SetGameEnabled { game, enabled, reply } => {
                let result = self.registry().set_enabled(&game, enabled);
                self.after_edit(result, reply);
            }
            Command::RemoveGame { game, reply } => {
                let result = self.registry().remove_game(&game);
                self.after_edit(result, reply);
            }
            Command::RunTest(reply) => {
                let result = define_test_game(&mut self.registry());
                if result.is_ok() {
                    self.run_test();
                }
                self.after_edit(result, reply);
            }
        }
    }

    fn after_edit(&mut self, result: Result<(), String>, reply: Reply<()>) {
        if result.is_ok() {
            self.save();
            self.keyboard.wake();
        }
        let _ = reply.send(result);
        self.publish();
    }

    fn registry(&self) -> std::sync::MutexGuard<'_, Registry> {
        self.engine.registry.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Start the server, remember its port, and put the files in the prefixes.
    async fn start(&mut self) -> Result<(), String> {
        if self.running.is_some() {
            return Ok(());
        }
        let listener = bind_loopback(self.engine.settings.port).await.map_err(|e| format!("GameSense server: {e}"))?;
        let port = listener.local_addr().map_err(|e| e.to_string())?.port();
        let keyboard = self.keyboard.clone();
        let changes = self.changes.clone();
        let shared = server::Shared {
            registry: Arc::clone(&self.engine.registry),
            clock: self.engine.clock,
            changed: Arc::new(move |change| {
                keyboard.wake();
                let _ = changes.send(change);
            }),
        };
        let (shutdown, stop) = oneshot::channel::<()>();
        let task = tokio::spawn(async move {
            let serve = axum::serve(listener, server::router(shared)).with_graceful_shutdown(async {
                let _ = stop.await;
            });
            if let Err(e) = serve.await {
                warn!(error = %e, "GameSense server stopped");
            }
        });
        info!(port, "GameSense server listening on 127.0.0.1");
        self.running = Some(Running { port, shutdown, task });
        self.engine.settings.enabled = true;
        self.engine.settings.port = Some(port);
        self.save();
        self.scans = 0; // re-read the libraries now
        self.scan().await;
        Ok(())
    }

    /// Stop the server and take gglinux's files back out of the prefixes.
    async fn stop(&mut self) -> Result<(), String> {
        if let Some(r) = self.running.take() {
            let _ = r.shutdown.send(());
            let _ = tokio::time::timeout(Duration::from_secs(1), r.task).await;
            info!("GameSense server stopped");
        }
        self.engine.settings.enabled = false;
        self.save();
        let libraries = self.libraries_or_read();
        let removed = tokio::task::spawn_blocking(move || {
            prefixes::program_data_dirs(&libraries)
                .iter()
                .filter(|pd| prefixes::remove(pd).unwrap_or(false))
                .count()
        })
        .await
        .unwrap_or(0);
        debug!(removed, "removed coreProps.json from Proton prefixes");
        self.holding = 0;
        Ok(())
    }

    fn libraries_or_read(&mut self) -> Vec<PathBuf> {
        if self.libraries.is_empty() || self.scans % LIBRARY_REFRESH_SCANS == 0 {
            let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
            self.libraries = prefixes::libraries(&prefixes::steam_roots(&home));
        }
        self.libraries.clone()
    }

    /// Write the file into any prefix that lacks it; count the ones that hold it.
    async fn scan(&mut self) {
        let Some(port) = self.running.as_ref().map(|r| r.port) else { return };
        let libraries = self.libraries_or_read();
        self.scans = self.scans.wrapping_add(1);
        let (holding, failed) = tokio::task::spawn_blocking(move || {
            let mut holding = 0u32;
            let mut failed = Vec::new();
            for pd in prefixes::program_data_dirs(&libraries) {
                match prefixes::write(&pd, port) {
                    Ok(Written::Wrote) => {
                        debug!(prefix = %pd.display(), "wrote coreProps.json");
                        holding += 1;
                    }
                    Ok(Written::Unchanged) => holding += 1,
                    Ok(Written::Foreign) => {}
                    Err(_) => failed.push(pd),
                }
            }
            (holding, failed)
        })
        .await
        .unwrap_or_default();
        for pd in newly_failed(&mut self.failed, &failed) {
            warn!(prefix = %pd.display(), "could not write coreProps.json into this Proton prefix");
        }
        self.holding = holding;
    }

    /// Play the test game: an F-key health bar sweeping 100 → 0 → 100.
    fn run_test(&mut self) {
        if let Some(old) = self.test.take() {
            old.abort();
        }
        let registry = Arc::clone(&self.engine.registry);
        let clock = self.engine.clock;
        let keyboard = self.keyboard.clone();
        let changes = self.changes.clone();
        self.test = Some(tokio::spawn(async move {
            for value in test_values() {
                {
                    let mut r = registry.lock().unwrap_or_else(|e| e.into_inner());
                    if send_test_value(&mut r, value, clock.now()).is_err() {
                        return; // removed from the UI mid-run
                    }
                }
                keyboard.wake();
                let _ = changes.send(Change::Live);
                tokio::time::sleep(TEST_STEP).await;
            }
            let _ = registry.lock().unwrap_or_else(|e| e.into_inner()).stop_game(TEST_GAME);
            keyboard.wake();
            let _ = changes.send(Change::Live);
        }));
    }

    fn state(&self) -> GameSenseState {
        GameSenseState {
            enabled: self.running.is_some(),
            address: self.running.as_ref().map(|r| format!("127.0.0.1:{}", r.port)),
            prefixes: self.holding,
            games: game_infos(&self.registry(), self.engine.clock.now()),
        }
    }

    fn save(&mut self) {
        let games = self.registry().saved(); // the guard ends here, before `self` is mutated
        self.engine.settings.games = games;
        save_settings(&self.engine.settings_path, &self.engine.settings);
    }

    fn publish(&mut self) {
        let now = self.state();
        if self.published.as_ref() != Some(&now) {
            self.published = Some(now.clone());
            let _ = self.event_tx.send(Event::GamesenseChanged { state: now });
        }
    }
}
```

`anyhow` is already a `gglinuxd` dependency.

- [ ] **Step 4: Wire it into `main.rs`**

- Add `use gamesense::{Command as GamesenseCommand, Handle as GamesenseHandle};` next to the other
  worker imports, and `gamesense: GamesenseHandle,` to `struct Workers`.
- Remove the `#[allow(dead_code)]` put on `mod gamesense;` in Task 8, if one was added, and any
  from Task 7 on `Overlay` / `Handle::wake`.
- Replace the `let workers = Workers { … };` block with:

```rust
    // GameSense's registry exists before the keyboard worker, which draws it.
    let engine = gamesense::Engine::load();
    let keyboard = keyboard::spawn(event_tx.clone(), vec![engine.overlay()]);
    let workers = Workers {
        mouse: mouse::spawn(event_tx.clone()),
        headset: headset::spawn(event_tx.clone()),
        gamesense: gamesense::spawn(engine, event_tx.clone(), keyboard.clone()),
        keyboard,
        sonar: sonar::spawn(event_tx.clone()),
    };
```

- Replace the Task 6 placeholder arms in `dispatch` with:

```rust
        Request::GetGamesenseState => match workers.gamesense.send(GamesenseCommand::GetState).await {
            Ok(state) => ResponseData::Gamesense { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetGamesenseEnabled { enabled } => outcome(
            workers.gamesense.send::<()>(|reply| GamesenseCommand::SetEnabled { enabled, reply }).await,
        ),
        Request::SetGameEnabled { game, enabled } => outcome(
            workers
                .gamesense
                .send::<()>(|reply| GamesenseCommand::SetGameEnabled { game, enabled, reply })
                .await,
        ),
        Request::RemoveGame { game } => outcome(
            workers.gamesense.send::<()>(|reply| GamesenseCommand::RemoveGame { game, reply }).await,
        ),
        Request::GamesenseTest => outcome(workers.gamesense.send(GamesenseCommand::RunTest).await),
```

- [ ] **Step 5: Run the tests to verify they pass**

Run: `cargo test --workspace --all-targets 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6} END{print "passed="p" failed="f}' && cargo clippy --workspace --all-targets 2>&1 | grep -E "^(warning|error)" -A4 | head -20`
Expected: `failed=0`, with the six new `gamesense::tests` passing; no clippy output.

Smoke-test the daemon without hardware changes. This uses a scratch `HOME` and config, so no real
prefixes and no real settings are touched:

```bash
S=$(mktemp -d)
mkdir -p $S/home/.local/share/Steam/steamapps/compatdata/99/pfx/drive_c/ProgramData $S/cfg
printf '"libraryfolders"\n{\n\t"0"\n\t{\n\t\t"path"\t\t"%s"\n\t}\n}\n' "$S/home/.local/share/Steam" > $S/home/.local/share/Steam/steamapps/libraryfolders.vdf
echo '{"enabled":true}' > $S/cfg/gamesense.json
```

Then run the daemon, `cargo run -q -p gglinuxd`, with `HOME=$S/home XDG_CONFIG_HOME=$S XDG_RUNTIME_DIR=$S/run`,
renaming `$S/cfg` to `$S/gglinux` first so `config_dir()` finds it. The real daemon must not be
running. Check that:
- the log says `GameSense server listening`;
- `cat "$S/home/.local/share/Steam/steamapps/compatdata/99/pfx/drive_c/ProgramData/SteelSeries/SteelSeries Engine 3/coreProps.json"`
  shows the address;
- `curl -s -H 'Content-Type: application/json' -d '{"game":"SMOKE"}' http://<address>/game_metadata`
  prints `{}`;
- `curl -s -H 'Origin: http://x' -H 'Content-Type: application/json' -d '{}' http://<address>/game_event`
  prints an error;
- SIGINT stops it.

**Do not enable reactive or lighting.** The keyboard worker never takes over, because no game
event with key handlers is sent. Ledger what was seen.

- [ ] **Step 6: Commit**

```bash
git add crates/gglinuxd/src/gamesense.rs crates/gglinuxd/src/main.rs
git commit -m "gglinuxd: GameSense controller — switch, server, Proton prefixes, test game

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

### Task 10: The Games page; docs

**Files:**
- Modify: `ui/src/lib/types.ts`, `ui/src/lib/daemon.svelte.ts`, `ui/src/lib/icons.ts`, `ui/src/App.svelte`
- Create: `ui/src/lib/GamesPanel.svelte`
- Modify: `README.md`, `PLAN.md`, `docs/PROTOCOLS.md`

**Interfaces:**
- Consumes: the JSON from Task 6.

- [ ] **Step 1: Types and the daemon store (the compile gate)**

`types.ts`, after `KeyboardState`:

```ts
export interface GameInfo {
  name: string;
  display_name: string | null;
  developer: string | null;
  enabled: boolean;
  active: boolean;
  events: string[];
}

export interface GameSenseState {
  enabled: boolean;
  address: string | null;
  prefixes: number;
  games: GameInfo[];
}
```

Add to the `Request` union:

```ts
  | { method: "get_gamesense_state" }
  | { method: "set_gamesense_enabled"; params: { enabled: boolean } }
  | { method: "set_game_enabled"; params: { game: string; enabled: boolean } }
  | { method: "remove_game"; params: { game: string } }
  | { method: "gamesense_test" }
```

Add `| { type: "gamesense"; state: GameSenseState }` to the response union, and
`| { event: "gamesense_changed"; data: { state: GameSenseState } }` to the event union.

`daemon.svelte.ts`:
- import `GameSenseState`;
- add the field `gamesense = $state<GameSenseState | null>(null);` with the doc comment
  `/** GameSense server and games; null until first fetched. */`;
- in the disconnect branch, `this.gamesense = null;`;
- in the event switch, `case "gamesense_changed": this.gamesense = ev.data.state; break;`;
- the methods:

```ts
  async getGamesenseState(): Promise<GameSenseState> {
    const res = await this.request({ method: "get_gamesense_state" });
    if (res.type !== "gamesense") throw new Error(`unexpected response: ${res.type}`);
    this.gamesense = res.state;
    return res.state;
  }

  setGamesenseEnabled(enabled: boolean): Promise<void> {
    return this.#gamesenseCommand({ method: "set_gamesense_enabled", params: { enabled } });
  }

  setGameEnabled(game: string, enabled: boolean): Promise<void> {
    return this.#gamesenseCommand({ method: "set_game_enabled", params: { game, enabled } });
  }

  removeGame(game: string): Promise<void> {
    return this.#gamesenseCommand({ method: "remove_game", params: { game } });
  }

  gamesenseTest(): Promise<void> {
    return this.#gamesenseCommand({ method: "gamesense_test" });
  }

  async #gamesenseCommand(req: Request): Promise<void> {
    const res = await this.request(req);
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getGamesenseState();
  }
```

Run: `cd ui && npm run check 2>&1 | tail -1`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 2: The Games page**

`icons.ts`, in `ICONS` after `moments`:

```ts
  games: ["M7 8.5h10a4.5 4.5 0 0 1 4.5 4.5v.5a3.5 3.5 0 0 1-6.4 2l-.9-1.3h-4.4l-.9 1.3a3.5 3.5 0 0 1-6.4-2V13A4.5 4.5 0 0 1 7 8.5z", "M8 11v3 M6.5 12.5h3", "M15.5 12h.01 M17.5 13.5h.01"],
```

`ui/src/lib/GamesPanel.svelte`:

```svelte
<script lang="ts">
  // GameSense: the switch, where games find it, a test, and the games that have talked to it.

  import { daemon } from "./daemon.svelte";

  let gs = $derived(daemon.gamesense);
  let busy = $state(false);
  let error = $state<string | null>(null);

  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getGamesenseState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  async function run(action: () => Promise<void>) {
    busy = true;
    error = null;
    try {
      await action();
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }
</script>

<article class="panel">
  {#if gs}
    <section class="first">
      <label class="toggle">
        <input
          type="checkbox"
          checked={gs.enabled}
          disabled={busy}
          onchange={(e) => run(() => daemon.setGamesenseEnabled(e.currentTarget.checked))}
        />
        Let games control the keyboard lighting (GameSense)
      </label>
      <p class="note">
        Turning this on writes a small <code>coreProps.json</code> into each of Steam's Proton prefixes, so Windows
        games find gglinux; turning it off removes them. Games that support GameSense can take the keyboard over
        while they run, even with gglinux lighting off, and hand it back 15 seconds after they go quiet.
      </p>
      {#if gs.enabled}
        <p class="meta">Listening on <code>{gs.address}</code> · in {gs.prefixes} Proton prefix{gs.prefixes === 1 ? "" : "es"}</p>
      {/if}
      <button class="ghost" disabled={busy} onclick={() => run(() => daemon.gamesenseTest())}>
        Run test (F-key health bar, 20 s)
      </button>
    </section>

    <section>
      <div class="head"><h4>Games</h4></div>
      {#if gs.games.length === 0}
        <p class="meta">No game has talked to GameSense yet.</p>
      {:else}
        <ul class="games">
          {#each gs.games as g (g.name)}
            <li>
              <span class="dot" class:on={g.active} title={g.active ? "active" : "quiet"}></span>
              <div class="who">
                <strong>{g.display_name ?? g.name}</strong>
                {#if g.developer}<span class="meta">{g.developer}</span>{/if}
                <span class="meta">{g.events.join(", ")}</span>
              </div>
              <label class="toggle small">
                <input
                  type="checkbox"
                  checked={g.enabled}
                  disabled={busy}
                  onchange={(e) => run(() => daemon.setGameEnabled(g.name, e.currentTarget.checked))}
                />
                on
              </label>
              <button class="ghost" disabled={busy} onclick={() => run(() => daemon.removeGame(g.name))}>Remove</button>
            </li>
          {/each}
        </ul>
      {/if}
    </section>
  {/if}

  {#if error}
    <p class="error">{error}</p>
  {/if}
</article>

<style>
  .panel { padding: 1rem 1.1rem; background: var(--surface); border: 1px solid var(--line); border-radius: 12px; }
  section { margin-top: 1.1rem; padding-top: 1rem; border-top: 1px solid var(--line); }
  section.first { margin-top: 0; padding-top: 0; border-top: none; }
  h4 { margin: 0; font-size: 0.8rem; font-weight: 600; text-transform: uppercase; letter-spacing: 0.05em; color: var(--text-dim); }
  .head { display: flex; align-items: center; justify-content: space-between; margin-bottom: 0.6rem; }
  .toggle { display: flex; align-items: center; gap: 0.5rem; font-size: 0.85rem; cursor: pointer; }
  .toggle.small { font-size: 0.78rem; color: var(--text-dim); }
  .note { margin: 0.6rem 0 0; font-size: 0.8rem; color: var(--text-dim); max-width: 44rem; }
  .meta { margin: 0.5rem 0 0; font-size: 0.8rem; color: var(--text-dim); }
  code { font-size: 0.78rem; color: var(--text); }
  .ghost { margin-top: 0.75rem; padding: 0.3rem 0.65rem; font-size: 0.8rem; color: var(--text-dim); background: none; border: 1px solid var(--line); border-radius: 7px; cursor: pointer; }
  .ghost:hover:not(:disabled) { border-color: var(--accent); color: var(--accent); }
  .ghost:disabled { opacity: 0.4; cursor: default; }
  .games { list-style: none; margin: 0; padding: 0; }
  .games li { display: flex; align-items: center; gap: 0.75rem; padding: 0.55rem 0; border-top: 1px solid var(--line); }
  .games li:first-child { border-top: none; }
  .games li .ghost { margin-top: 0; }
  .who { flex: 1; display: flex; flex-direction: column; }
  .who .meta { margin: 0.1rem 0 0; }
  .dot { flex: none; width: 9px; height: 9px; border-radius: 50%; background: var(--line); }
  .dot.on { background: var(--accent); }
  .error { margin: 0.8rem 0 0; padding: 0.5rem 0.7rem; font-size: 0.8rem; color: var(--danger); background: color-mix(in srgb, var(--danger) 12%, transparent); border-radius: 8px; }
</style>
```

`App.svelte`:
- `import GamesPanel from "./lib/GamesPanel.svelte";`;
- `type Page = "engine" | "sonar" | "games" | "moments" | "settings";`;
- in `NAV`, after the sonar entry:
  `{ id: "games", icon: "games", label: "Games", blurb: "GameSense: games drive your lighting" },`,
  and change the settings blurb to `"Profiles, updates"`;
- `COMING`'s type becomes `Record<Exclude<Page, "engine" | "sonar" | "games">, string>`, and its
  `settings` text becomes `"M7 — profiles and per-game switching"`;
- after the sonar page branch (`{:else if page === "sonar"} … `), add:

```svelte
    {:else if page === "games"}
      <header class="page-head">
        <h1>Games</h1>
        <p>{NAV[2].blurb}</p>
      </header>
      {#if daemon.connected}
        <GamesPanel />
      {:else}
        <section class="notice"><h2>gglinuxd isn&rsquo;t running</h2></section>
      {/if}
```

Run: `cd ui && npm run check 2>&1 | tail -1`
Expected: `0 ERRORS 0 WARNINGS`.

- [ ] **Step 3: Docs**

README.md — after the Sonar optional-packages paragraph, add:

```markdown
**GameSense** (games and apps driving the keyboard lighting) is off until you turn it on in the
Games page. Turning it on starts a server on `127.0.0.1` and writes a small `coreProps.json`
(`{"address":"127.0.0.1:<port>"}`) into every Proton prefix under `steamapps/compatdata/*/pfx/drive_c/ProgramData/SteelSeries/SteelSeries Engine 3/`,
so Windows games under Proton find it; turning it off removes those files (gglinux never touches a
coreProps.json it didn't write). Native Arch `steam` only — Flatpak Steam isn't supported. Few games
speak GameSense; **Run test** shows what it looks like.
```

PLAN.md — in the M6 section, after the **Accept:** line, add:

```markdown
Progress 2026-09-29 — **M6a GameSense server done in software** (spec:
`docs/superpowers/specs/2026-09-29-gamesense-server-design.md`): new `gg-gamesense` crate (requests,
handlers, zones, painting, registry, Proton-prefix files), an axum server on loopback closed to web
pages, `coreProps.json` in every Proton prefix while enabled, games drawn over the lighting as an
overlay (taking the keyboard over when gglinux lighting is off, handing back after the game goes
quiet), and a Games page with a built-in test game. None of the owner's installed games speak the
SDK. Not yet verified on hardware. Next: M6b game-independent effects on the same overlay interface.
```

`docs/PROTOCOLS.md` §4 — append under the Proton-bridge paragraph:

```markdown
**Implemented 2026-09-29** in `gg-gamesense` + `gglinuxd/src/gamesense*`: the prefix writer never
overwrites a coreProps.json that isn't gglinux's (only an `address` key on `127.0.0.1`), and removes
its own files when GameSense is turned off. Requests with an `Origin` header are refused and POSTs
must be `application/json`, so browsers can't reach it.
```

- [ ] **Step 4: Final checks**

Run: `cd ui && npm run check 2>&1 | tail -1; cd .. && cargo test --workspace --all-targets 2>&1 | grep -E "^test result" | awk '{p+=$4;f+=$6} END{print "passed="p" failed="f}'; cargo clippy --workspace --all-targets 2>&1 | grep -cE "^(warning|error)"`
Expected: `0 ERRORS 0 WARNINGS`, `failed=0`, `0`.

- [ ] **Step 5: Commit**

```bash
git add ui/src README.md PLAN.md docs/PROTOCOLS.md
git commit -m "UI: the Games page for GameSense; docs

Co-Authored-By: Claude Opus 5.5 <noreply@anthropic.com>"
```

---

## Hardware acceptance (the owner, after review)

1. Games page → turn GameSense on. Then
   `find ~/.local/share/Steam/steamapps/compatdata -name coreProps.json | wc -l` equals the page's
   prefix count.
2. **Run test** with gglinux lighting on: the F-keys show a red→green bar sweeping down and up,
   flashing below 20 %.
3. Turn gglinux lighting **off**, then Run test: the keyboard is taken over (the rest shows the saved
   effect) and handed back to the onboard profile when the test ends.
4. Turn GameSense off: the `find` count drops to 0.
5. Add the prefix files (and `gamesense.json`) to the list of files gglinux writes.
