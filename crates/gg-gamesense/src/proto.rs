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
    /// What this machine can't show: a screen, tactile, mouse, headset, zoned
    /// RGB, or a keyboard bitmap.
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
    /// Parse one handler. Handlers for devices this machine can't show, and the
    /// SDK's `bitmap` and `partial-bitmap` modes, become [`Target::Unsupported`]
    /// without further checks; a malformed key handler is an error, so a game's
    /// bug is reported rather than silently dark.
    pub fn parse(v: &Value) -> Result<Handler, String> {
        let o = v.as_object().ok_or("a handler must be an object")?;
        let device = o
            .get("device-type")
            .and_then(Value::as_str)
            .ok_or("a handler needs a device-type")?;
        let mode = o.get("mode").and_then(Value::as_str).unwrap_or("color");
        if !KEY_DEVICES.contains(&device) || matches!(mode, "bitmap" | "partial-bitmap") {
            return Ok(Handler { target: Target::Unsupported, mode: Mode::Color, color: None, rate: None, context_frame_key: None });
        }
        let target = match (o.get("custom-zone-keys"), o.get("zone").and_then(Value::as_str)) {
            (Some(keys), _) => Target::Keys(custom_keys(keys)?),
            (None, Some(zone)) => Target::Keys(zones::named(zone)),
            (None, None) => return Err("a handler needs a zone or custom-zone-keys".into()),
        };
        let mode = match mode {
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
    fn the_sdk_bitmap_modes_are_kept_as_unsupported() {
        for v in [
            json!({ "device-type": "rgb-per-key-zones", "mode": "bitmap" }),
            json!({ "device-type": "rgb-per-key-zones", "mode": "partial-bitmap", "excluded-events": ["HEALTH"] }),
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
