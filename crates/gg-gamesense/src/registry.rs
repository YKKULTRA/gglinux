//! Every game that has talked to the server, its events and their handlers,
//! and what the active one paints. Time is passed in (`now`, monotonic seconds),
//! so all of it is testable without a clock.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::paint::{paint, Reading};
use crate::proto::{parse_rgb, valid_name, BindEvent, GameEvent, GameMetadata, Handler, MultipleEvents, RegisterEvent, Rgb};

/// Colours by HID code: what a game paints over the lighting.
pub type Layer = HashMap<u8, Rgb>;

pub const MAX_GAMES: usize = 64;
pub const MAX_EVENTS: usize = 128;
pub const MAX_HANDLERS: usize = 32;
/// Most handler JSON kept across every game, in bytes as `gamesense.json` holds
/// it (pretty-printed). Handlers for other devices are kept as sent, so without
/// this a game could fill memory and the disk.
pub const MAX_HANDLER_BYTES: usize = 2 * 1024 * 1024;
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
    /// The size of `saved.handlers`, counted against [`MAX_HANDLER_BYTES`].
    bytes: usize,
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
        let bytes = json_bytes(&b.handlers);
        let replaced = self.games.get(&b.game).and_then(|g| g.events.get(&b.event)).map_or(0, |e| e.bytes);
        if self.handler_bytes() - replaced + bytes > MAX_HANDLER_BYTES {
            return Err(format!("at most {} KiB of handlers in all; remove a game to make room", MAX_HANDLER_BYTES / 1024));
        }
        self.define(&b.game, &b.event, b.min_value, b.max_value, b.icon_id, b.value_optional, Some((b.handlers, parsed, bytes)))
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
                        let bytes = json_bytes(&e.handlers);
                        (e.name.clone(), Event { saved: e, handlers, bytes, value: None, frame: None, received: false, changed_at: 0.0 })
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

    /// Handler JSON kept across every game.
    fn handler_bytes(&self) -> usize {
        self.games.values().flat_map(|g| g.events.values()).map(|e| e.bytes).sum()
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
        handlers: Option<(Vec<Value>, Vec<Handler>, usize)>,
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
            bytes: 0,
            value: None,
            frame: None,
            received: false,
            changed_at: 0.0,
        });
        e.saved.min_value = min;
        e.saved.max_value = max;
        e.saved.icon_id = icon.unwrap_or(0);
        e.saved.value_optional = value_optional;
        if let Some((raw, parsed, bytes)) = handlers {
            e.saved.handlers = raw;
            e.handlers = parsed;
            e.bytes = bytes;
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
    e.frame = data.frame.map(|f| colours_read(&e.handlers, &f));
    e.received = true;
    game.last_activity = Some(now);
    game.stopped = false;
    Ok(())
}

/// The frame entries an event's handlers read, each cut down to its colour: a
/// frame can be as large as the request, and nothing else in it is painted.
fn colours_read(handlers: &[Handler], frame: &Map<String, Value>) -> Map<String, Value> {
    handlers
        .iter()
        .filter_map(|h| h.context_frame_key.as_ref())
        .filter_map(|k| {
            let c = parse_rgb(frame.get(k)?).ok()?;
            Some((k.clone(), json!({ "red": c.r, "green": c.g, "blue": c.b })))
        })
        .collect()
}

/// Handlers' size as `gamesense.json` holds them.
fn json_bytes(handlers: &[Value]) -> usize {
    serde_json::to_vec_pretty(handlers).map_or(0, |v| v.len())
}

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
    fn handlers_kept_in_all_have_a_size_limit() {
        // An unsupported handler is kept as sent, so its size is the game's choice.
        let big = json!([{ "device-type": "tactile", "pattern": vec![0; 8_000] }]);
        let mut r = Registry::default();
        let full = (0..MAX_EVENTS)
            .find(|i| r.bind_event(bind("G", &format!("E{i}"), big.clone())).is_err())
            .expect("refused before every event is bound");
        assert!(full > 1 && r.handler_bytes() <= MAX_HANDLER_BYTES, "{full} fit");
        r.bind_event(bind("G", "E1", big.clone())).expect("a rebind counts its new handlers, not both");
        r.remove_event("G", "E0").unwrap();
        r.bind_event(bind("G", &format!("E{full}"), big)).expect("a removed event makes room");
    }

    #[test]
    fn a_frame_keeps_only_the_colours_its_handlers_read() {
        let mut r = Registry::default();
        r.bind_event(bind("G", "TINT", json!([{ "device-type": "keyboard", "zone": "w", "mode": "context-color",
            "context-frame-key": "tint" }]))).unwrap();
        let event = json!({ "game": "G", "event": "TINT", "data": { "value": 1, "frame": {
            "tint": { "red": 1, "green": 2, "blue": 3, "padding": vec![0; 16] }, "unread": vec![0; 16] } } });
        r.game_event(serde_json::from_value(event).unwrap(), 0.0).unwrap();
        assert_eq!(r.paint(0.0).unwrap()[&0x1A], Rgb { r: 1, g: 2, b: 3 });
        let kept = r.games["G"].events["TINT"].frame.clone().map(Value::Object);
        assert_eq!(kept, Some(json!({ "tint": { "red": 1, "green": 2, "blue": 3 } })));
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
