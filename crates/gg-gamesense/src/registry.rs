// SPDX-License-Identifier: GPL-3.0-or-later
//! Every game that has talked to the server, its events and their handlers,
//! and what the active one paints on the keys and shows on the screen. Time is
//! passed in (`now`, monotonic seconds), so all of it is testable without a clock.

use std::collections::{BTreeMap, HashMap};

use gg_device_keyboard::Bitmap;
use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::bitmap;
use crate::paint::{paint, Reading};
use crate::proto::{
    parse_rgb, valid_name, BindEvent, GameEvent, GameMetadata, Handler, MultipleEvents,
    RegisterEvent, Rgb, Target,
};
use crate::screen::{self, Data, Show};

/// Colours by HID code: what a game paints over the lighting.
pub type Layer = HashMap<u8, Rgb>;

/// What one handler of an event draws on, for the user to see.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Draws {
    /// Keys.
    Keys,
    /// The keyboard's screen, with what it asks for that isn't drawn.
    Screen { notes: Vec<String> },
    /// Every key, from the event's colour grid.
    Bitmap,
    /// Every key but those of these events (unless the event names others).
    PartialBitmap { excluded: Vec<String> },
    /// A device this machine doesn't have, by its device-type.
    Elsewhere { device: String },
}

/// What the active game shows on the keyboard's screen now.
#[derive(Debug, Clone, PartialEq)]
pub struct ScreenNow {
    pub picture: Bitmap,
    /// Seconds until that changes by itself: the next frame, or the game going quiet.
    pub next_change: Option<f64>,
}

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
    /// `(event, what the handler draws on)`, a pair per handler, in event order.
    pub handlers: Vec<(String, Draws)>,
    /// Drawing on the keyboard's screen now.
    pub on_screen: bool,
}

struct Event {
    saved: SavedEvent,
    handlers: Vec<Handler>,
    /// What each handler draws on, for the summaries.
    draws: Vec<Draws>,
    /// The size of `saved.handlers`, counted against [`MAX_HANDLER_BYTES`].
    bytes: usize,
    value: Option<i64>,
    frame: Option<Map<String, Value>>,
    /// What the last frame carried for a picture or a bitmap handler.
    extras: Extras,
    received: bool,
    /// When the value last changed (or, with `value_optional`, last arrived).
    changed_at: f64,
}

impl Event {
    fn new(saved: SavedEvent) -> Event {
        Event {
            saved,
            handlers: Vec::new(),
            draws: Vec::new(),
            bytes: 0,
            value: None,
            frame: None,
            extras: Extras::default(),
            received: false,
            changed_at: 0.0,
        }
    }
}

/// From an event's frame: its own screen picture, its colour grid and the events a
/// background leaves alone, each kept only when one of its handlers reads it.
#[derive(Default)]
struct Extras {
    image: Option<Vec<u8>>,
    bitmap: Option<Vec<Option<Rgb>>>,
    excluded: Option<Vec<String>>,
}

struct Game {
    display_name: Option<String>,
    developer: Option<String>,
    timer_ms: u32,
    enabled: bool,
    events: BTreeMap<String, Event>,
    last_activity: Option<f64>,
    stopped: bool,
    /// The screen frames playing, and the event that started them: the last event that
    /// wrote the screen.
    screen: Option<(String, Show)>,
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
        self.define(
            &r.game,
            &r.event,
            r.min_value,
            r.max_value,
            r.icon_id,
            r.value_optional,
            None,
        )
    }

    pub fn bind_event(&mut self, b: BindEvent) -> Result<(), String> {
        if b.handlers.len() > MAX_HANDLERS {
            return Err(format!("at most {MAX_HANDLERS} handlers per event"));
        }
        let parsed = b
            .handlers
            .iter()
            .map(Handler::parse)
            .collect::<Result<Vec<_>, _>>()?;
        let bytes = json_bytes(&b.handlers);
        let replaced = self
            .games
            .get(&b.game)
            .and_then(|g| g.events.get(&b.event))
            .map_or(0, |e| e.bytes);
        if self.handler_bytes() - replaced + bytes > MAX_HANDLER_BYTES {
            return Err(format!(
                "at most {} KiB of handlers in all; remove a game to make room",
                MAX_HANDLER_BYTES / 1024
            ));
        }
        self.define(
            &b.game,
            &b.event,
            b.min_value,
            b.max_value,
            b.icon_id,
            b.value_optional,
            Some((b.handlers, parsed, bytes)),
        )
    }

    pub fn game_event(&mut self, e: GameEvent, now: f64) -> Result<(), String> {
        let game = self
            .games
            .get_mut(&e.game)
            .ok_or_else(|| format!("unknown game {}", e.game))?;
        apply(game, &e.event, e.data, now)
    }

    pub fn multiple_events(&mut self, m: MultipleEvents, now: f64) -> Result<(), String> {
        let game = self
            .games
            .get_mut(&m.game)
            .ok_or_else(|| format!("unknown game {}", m.game))?;
        m.events
            .into_iter()
            .try_for_each(|item| apply(game, &item.event, item.data, now))
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
        self.games
            .remove(game)
            .map(|_| ())
            .ok_or_else(|| format!("unknown game {game}"))
    }

    pub fn remove_event(&mut self, game: &str, event: &str) -> Result<(), String> {
        let g = self.game_mut(game)?;
        g.events
            .remove(event)
            .ok_or_else(|| format!("unknown event {event}"))?;
        if g.screen.as_ref().is_some_and(|(e, _)| e == event) {
            g.screen = None;
        }
        Ok(())
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
            .filter_map(|(name, g)| {
                g.last_activity
                    .filter(|&t| now - t <= f64::from(g.timer_ms) / 1000.0)
                    .map(|t| (name, t))
            })
            .max_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(name, _)| name.as_str())
    }

    /// What the active game paints now; `None` when no game is active or it paints nothing.
    pub fn paint(&self, now: f64) -> Option<Layer> {
        let game = &self.games[self.active_game(now)?];
        let mut events: Vec<&Event> = game
            .events
            .values()
            .filter(|e| e.received && (e.value.is_some() || e.saved.value_optional))
            .collect();
        events.sort_by(|a, b| a.changed_at.total_cmp(&b.changed_at)); // later changes paint over earlier ones
        let mut layer = Layer::new();
        for e in events {
            let reading = Reading {
                value: e.value.unwrap_or(0),
                min: e.saved.min_value,
                max: e.saved.max_value,
                frame: e.frame.as_ref(),
                // A value can arrive after the caller read `now`: count it as just arrived.
                age: (now - e.changed_at).max(0.0),
            };
            for h in &e.handlers {
                match &h.target {
                    Target::Bitmap(partial) => paint_bitmap(game, e, partial.as_ref(), &mut layer),
                    _ => layer.extend(paint(h, &reading)),
                }
            }
        }
        (!layer.is_empty()).then_some(layer)
    }

    /// What the active game shows on the keyboard's screen now; `None` when no game is
    /// active, or its frames have played out (the screen goes back to the user's own).
    pub fn screen(&self, now: f64) -> Option<ScreenNow> {
        let game = &self.games[self.active_game(now)?];
        let (_, show) = game.screen.as_ref()?;
        let (picture, next) = show.picture(now)?;
        // Just past the moment it goes quiet, so it's gone when asked again.
        let quiet = game
            .last_activity
            .map(|t| t + f64::from(game.timer_ms) / 1000.0 - now + 0.005);
        let next_change = match (next, quiet) {
            (Some(a), Some(b)) => Some(a.min(b)),
            (a, b) => a.or(b),
        };
        Some(ScreenNow {
            picture,
            next_change: next_change.map(|s| s.max(0.0)),
        })
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
                handlers: g
                    .events
                    .iter()
                    .flat_map(|(event, e)| e.draws.iter().map(|d| (event.clone(), d.clone())))
                    .collect(),
                on_screen: active == Some(name.as_str())
                    && g.screen.as_ref().is_some_and(|(_, s)| s.at(now).is_some()),
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
                        let (kinds, handlers): (Vec<Draws>, Vec<Handler>) = e
                            .handlers
                            .iter()
                            .filter_map(|h| {
                                let parsed = Handler::parse(h).ok()?;
                                Some((draws(h, &parsed), parsed))
                            })
                            .unzip();
                        let bytes = json_bytes(&e.handlers);
                        let mut event = Event::new(e);
                        event.handlers = handlers;
                        event.draws = kinds;
                        event.bytes = bytes;
                        (event.saved.name.clone(), event)
                    })
                    .collect();
                let game = Game {
                    display_name: g.display_name,
                    developer: g.developer,
                    timer_ms: g
                        .deinitialize_timer_length_ms
                        .clamp(*TIMER_MS.start(), *TIMER_MS.end()),
                    enabled: g.enabled,
                    events,
                    last_activity: None,
                    stopped: false,
                    screen: None,
                };
                (g.name, game)
            })
            .collect();
        Registry { games }
    }

    /// Handler JSON kept across every game.
    fn handler_bytes(&self) -> usize {
        self.games
            .values()
            .flat_map(|g| g.events.values())
            .map(|e| e.bytes)
            .sum()
    }

    fn game_mut(&mut self, game: &str) -> Result<&mut Game, String> {
        self.games
            .get_mut(game)
            .ok_or_else(|| format!("unknown game {game}"))
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
            screen: None,
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
        let e = g.events.entry(event.to_owned()).or_insert_with(|| {
            Event::new(SavedEvent {
                name: event.to_owned(),
                min_value: 0,
                max_value: 100,
                icon_id: 0,
                value_optional: false,
                handlers: Vec::new(),
            })
        });
        e.saved.min_value = min;
        e.saved.max_value = max;
        e.saved.icon_id = icon.unwrap_or(0);
        e.saved.value_optional = value_optional;
        if let Some((raw, parsed, bytes)) = handlers {
            e.draws = raw.iter().zip(&parsed).map(|(r, p)| draws(r, p)).collect();
            e.saved.handlers = raw;
            e.handlers = parsed;
            e.bytes = bytes;
        }
        Ok(())
    }
}

/// What a handler draws on, from its parsed form (and its JSON, for another device's name).
fn draws(raw: &Value, h: &Handler) -> Draws {
    match &h.target {
        Target::Keys(_) => Draws::Keys,
        Target::Screen(s) => Draws::Screen {
            notes: s.notes.clone(),
        },
        Target::Bitmap(None) => Draws::Bitmap,
        Target::Bitmap(Some(excluded)) => Draws::PartialBitmap {
            excluded: excluded.clone(),
        },
        Target::Unsupported => Draws::Elsewhere {
            device: raw
                .get("device-type")
                .and_then(Value::as_str)
                .unwrap_or_default()
                .chars()
                .take(64)
                .collect(),
        },
    }
}

/// Record one event's data on its game. A change (a new value, or any event with
/// `value_optional`) starts its screen frames, over what the screen showed.
fn apply(
    game: &mut Game,
    event: &str,
    data: crate::proto::EventData,
    now: f64,
) -> Result<(), String> {
    let e = game
        .events
        .get_mut(event)
        .ok_or_else(|| format!("unknown event {event}; register or bind it first"))?;
    let value = data
        .value
        .filter(|v| v.is_finite())
        .map(|v| v.round() as i64);
    if value.is_none() && !e.saved.value_optional {
        return Err(format!("event {event} needs data.value"));
    }
    // Without `value_optional` the same value again is no change: the SDK doc warns that
    // this caching keeps new frame text off the screen.
    let changed = e.saved.value_optional || value != e.value || !e.received;
    if changed {
        e.changed_at = now;
    }
    if value.is_some() {
        e.value = value;
    }
    e.extras = data
        .frame
        .as_ref()
        .map(|f| extras(&e.handlers, f))
        .unwrap_or_default();
    e.frame = data.frame.map(|f| frame_read(&e.handlers, &f));
    e.received = true;
    let show = changed.then(|| start_show(e, now)).flatten();
    game.last_activity = Some(now);
    game.stopped = false;
    if let Some(show) = show {
        game.screen = Some((event.to_owned(), show));
    }
    Ok(())
}

/// The screen frames an event starts: those of its `screened-128x40` handler, else its
/// `screened` one, for its value; `None` if it has neither or no frames for the value.
fn start_show(e: &Event, now: f64) -> Option<Show> {
    let specs = || {
        e.handlers.iter().filter_map(|h| match &h.target {
            Target::Screen(s) => Some(s.as_ref()),
            _ => None,
        })
    };
    let spec = specs().find(|s| s.exact).or_else(|| specs().next())?;
    Some(Show {
        frames: spec.frames(e.value.unwrap_or(0))?.to_vec(),
        data: Data {
            value: e.value,
            frame: e.frame.clone().unwrap_or_default(),
            image: e.extras.image.clone(),
        },
        started: now,
    })
}

/// Paint an event's colour grid on the keys, all of them for `bitmap`, and for
/// `partial-bitmap` all but the keys of the events it leaves alone: the frame's
/// `excluded-events` if it sent them, else the handler's.
fn paint_bitmap(game: &Game, e: &Event, partial: Option<&Vec<String>>, layer: &mut Layer) {
    let Some(colours) = &e.extras.bitmap else {
        return;
    };
    let left_alone: Vec<u8> = partial
        .map(|defaults| e.extras.excluded.as_ref().unwrap_or(defaults))
        .into_iter()
        .flatten()
        .filter_map(|name| game.events.get(name))
        .flat_map(|other| &other.handlers)
        .filter_map(|h| match &h.target {
            Target::Keys(keys) => Some(keys.iter().copied()),
            _ => None,
        })
        .flatten()
        .collect();
    for &(cell, code) in bitmap::cells() {
        if let Some(Some(c)) = colours.get(cell) {
            if !left_alone.contains(&code) {
                layer.insert(code, *c);
            }
        }
    }
}

/// What an event's handlers read from its frame besides colours and text: its own
/// screen picture, its colour grid and the events a background leaves alone.
fn extras(handlers: &[Handler], frame: &Map<String, Value>) -> Extras {
    let image = handlers
        .iter()
        .any(|h| matches!(&h.target, Target::Screen(s) if s.has_image()));
    let grid = handlers
        .iter()
        .any(|h| matches!(h.target, Target::Bitmap(_)));
    let partial = handlers
        .iter()
        .any(|h| matches!(h.target, Target::Bitmap(Some(_))));
    Extras {
        image: frame
            .get(screen::IMAGE_KEY)
            .filter(|_| image)
            .and_then(screen::picture)
            .filter(|b| b.len() == screen::IMAGE_BYTES),
        bitmap: frame
            .get("bitmap")
            .filter(|_| grid)
            .and_then(bitmap::colours),
        excluded: frame
            .get("excluded-events")
            .filter(|_| partial)
            .and_then(|v| bitmap::excluded(v).ok())
            .map(|names| {
                names
                    .into_iter()
                    .filter(|n| valid_name(n))
                    .take(MAX_EVENTS)
                    .collect()
            }),
    }
}

/// The frame entries an event's handlers read: colours cut down to their colour, and
/// the text, numbers and switches its screen lines show (text cut at
/// [`screen::MAX_TEXT`]). A frame can be as large as the request; nothing else in it is
/// shown.
fn frame_read(handlers: &[Handler], frame: &Map<String, Value>) -> Map<String, Value> {
    let mut kept = colours_read(handlers, frame);
    let screen_keys = handlers.iter().filter_map(|h| match &h.target {
        Target::Screen(s) => Some(s.frame_keys()),
        _ => None,
    });
    for key in screen_keys.flatten() {
        let value = match frame.get(&key) {
            Some(Value::String(s)) => Value::String(s.chars().take(screen::MAX_TEXT).collect()),
            Some(v @ (Value::Number(_) | Value::Bool(_))) => v.clone(),
            _ => continue,
        };
        kept.entry(key).or_insert(value);
    }
    kept
}

/// The colours an event's handlers read, each cut down to its colour.
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
        serde_json::from_value(json!({ "game": game, "event": event, "handlers": handlers }))
            .unwrap()
    }

    fn fkeys_bar() -> Value {
        json!([{ "device-type": "keyboard", "zone": "function-keys", "mode": "percent",
                 "color": { "red": 255, "green": 0, "blue": 0 } }])
    }

    fn send(r: &mut Registry, game: &str, event: &str, value: f64, now: f64) -> Result<(), String> {
        r.game_event(
            serde_json::from_value(
                json!({ "game": game, "event": event, "data": { "value": value } }),
            )
            .unwrap(),
            now,
        )
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
        let missing =
            serde_json::from_value(json!({ "game": "G", "event": "HEALTH", "data": {} })).unwrap();
        assert!(
            r.game_event(missing, 0.0).is_err(),
            "value required unless value_optional"
        );
        let min_over_max = serde_json::from_value(
            json!({ "game": "G", "event": "E", "min_value": 5, "max_value": 1 }),
        )
        .unwrap();
        assert!(r.register_event(min_over_max).is_err());
    }

    #[test]
    fn limits_are_enforced() {
        let mut r = Registry::default();
        for i in 0..MAX_GAMES {
            r.game_metadata(serde_json::from_value(json!({ "game": format!("G{i}") })).unwrap())
                .unwrap();
        }
        assert!(r
            .game_metadata(serde_json::from_value(json!({ "game": "ONE_MORE" })).unwrap())
            .is_err());
        let mut r = Registry::default();
        for i in 0..MAX_EVENTS {
            r.register_event(
                serde_json::from_value(json!({ "game": "G", "event": format!("E{i}") })).unwrap(),
            )
            .unwrap();
        }
        assert!(r
            .register_event(
                serde_json::from_value(json!({ "game": "G", "event": "ONE_MORE" })).unwrap()
            )
            .is_err());
        let many = Value::Array(vec![fkeys_bar()[0].clone(); MAX_HANDLERS + 1]);
        assert!(Registry::default()
            .bind_event(bind("G", "E", many))
            .is_err());
    }

    #[test]
    fn handlers_kept_in_all_have_a_size_limit() {
        // An unsupported handler is kept as sent, so its size is the game's choice.
        let big = json!([{ "device-type": "tactile", "pattern": vec![0; 8_000] }]);
        let mut r = Registry::default();
        let full = (0..MAX_EVENTS)
            .find(|i| {
                r.bind_event(bind("G", &format!("E{i}"), big.clone()))
                    .is_err()
            })
            .expect("refused before every event is bound");
        assert!(
            full > 1 && r.handler_bytes() <= MAX_HANDLER_BYTES,
            "{full} fit"
        );
        r.bind_event(bind("G", "E1", big.clone()))
            .expect("a rebind counts its new handlers, not both");
        r.remove_event("G", "E0").unwrap();
        r.bind_event(bind("G", &format!("E{full}"), big))
            .expect("a removed event makes room");
    }

    #[test]
    fn a_frame_keeps_only_the_colours_its_handlers_read() {
        let mut r = Registry::default();
        r.bind_event(bind(
            "G",
            "TINT",
            json!([{ "device-type": "keyboard", "zone": "w", "mode": "context-color",
            "context-frame-key": "tint" }]),
        ))
        .unwrap();
        let event = json!({ "game": "G", "event": "TINT", "data": { "value": 1, "frame": {
            "tint": { "red": 1, "green": 2, "blue": 3, "padding": vec![0; 16] }, "unread": vec![0; 16] } } });
        r.game_event(serde_json::from_value(event).unwrap(), 0.0)
            .unwrap();
        assert_eq!(r.paint(0.0).unwrap()[&0x1A], Rgb { r: 1, g: 2, b: 3 });
        let kept = r.games["G"].events["TINT"].frame.clone().map(Value::Object);
        assert_eq!(
            kept,
            Some(json!({ "tint": { "red": 1, "green": 2, "blue": 3 } }))
        );
    }

    #[test]
    fn a_game_goes_quiet_after_its_timer_and_heartbeats_keep_it_up() {
        let mut r = with_bar();
        send(&mut r, "G", "HEALTH", 50.0, 0.0).unwrap();
        assert_eq!(r.active_game(14.9), Some("G"));
        assert_eq!(r.active_game(15.1), None, "the default timer is 15 s");
        r.heartbeat("G", 14.0).unwrap();
        assert_eq!(r.active_game(28.0), Some("G"));
        r.game_metadata(
            serde_json::from_value(json!({ "game": "G", "deinitialize_timer_length_ms": 100 }))
                .unwrap(),
        )
        .unwrap();
        assert_eq!(
            r.active_game(15.2),
            None,
            "clamped up to 1 s: 14 + 1 < 15.2"
        );
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
        r.bind_event(bind(
            "H",
            "AMMO",
            json!([{ "device-type": "keyboard", "zone": "function-keys",
            "color": { "red": 0, "green": 0, "blue": 255 } }]),
        ))
        .unwrap();
        send(&mut r, "G", "HEALTH", 100.0, 0.0).unwrap();
        send(&mut r, "H", "AMMO", 1.0, 1.0).unwrap();
        assert_eq!(r.active_game(2.0), Some("H"));
        assert_eq!(r.paint(2.0).unwrap()[&0x3A], Rgb { r: 0, g: 0, b: 255 });
    }

    #[test]
    fn the_most_recently_changed_event_wins_and_repeats_do_not_count_as_changes() {
        let mut r = with_bar();
        r.bind_event(bind(
            "G",
            "AMMO",
            json!([{ "device-type": "keyboard", "zone": "f1",
            "color": { "red": 0, "green": 0, "blue": 255 } }]),
        ))
        .unwrap();
        send(&mut r, "G", "HEALTH", 100.0, 0.0).unwrap();
        send(&mut r, "G", "AMMO", 5.0, 1.0).unwrap();
        assert_eq!(
            r.paint(1.5).unwrap()[&0x3A],
            Rgb { r: 0, g: 0, b: 255 },
            "AMMO changed last"
        );
        send(&mut r, "G", "HEALTH", 100.0, 2.0).unwrap();
        assert_eq!(
            r.paint(2.5).unwrap()[&0x3A],
            Rgb { r: 0, g: 0, b: 255 },
            "same HEALTH value: not a change"
        );
        send(&mut r, "G", "HEALTH", 99.0, 3.0).unwrap();
        assert_eq!(r.paint(3.5).unwrap()[&0x3A], Rgb { r: 255, g: 0, b: 0 });
    }

    #[test]
    fn a_value_newer_than_the_frame_starts_its_flash_lit() {
        // The keyboard reads the clock before it takes the lock, so a value can land in between.
        let mut r = Registry::default();
        r.bind_event(bind(
            "G",
            "HEALTH",
            json!([{ "device-type": "keyboard", "zone": "f1",
            "color": { "red": 255, "green": 0, "blue": 0 }, "rate": { "frequency": 4 } }]),
        ))
        .unwrap();
        send(&mut r, "G", "HEALTH", 10.0, 1.0).unwrap();
        assert_eq!(r.paint(0.999).unwrap()[&0x3A], Rgb { r: 255, g: 0, b: 0 });
    }

    #[test]
    fn a_game_with_only_unsupported_handlers_is_not_an_overlay() {
        let mut r = Registry::default();
        r.bind_event(bind(
            "G",
            "RUMBLE",
            json!([{ "device-type": "tactile", "zone": "one", "mode": "vibrate" }]),
        ))
        .unwrap();
        send(&mut r, "G", "RUMBLE", 1.0, 0.0).unwrap();
        assert_eq!(r.active_game(0.1), Some("G"));
        assert_eq!(
            r.paint(0.1),
            None,
            "an empty layer would take the keyboard for nothing"
        );
        assert_eq!(r.screen(0.1), None);
    }

    #[test]
    fn a_bitmap_handler_does_not_fail_the_rest_of_its_bind() {
        let mut r = Registry::default();
        let handlers = json!([{ "device-type": "rgb-per-key-zones", "mode": "bitmap" }, fkeys_bar()[0].clone()]);
        r.bind_event(bind("G", "HEALTH", handlers)).unwrap();
        send(&mut r, "G", "HEALTH", 100.0, 0.0).unwrap();
        assert_eq!(
            r.paint(0.5).unwrap().len(),
            12,
            "the F-key bar still paints"
        );
    }

    #[test]
    fn saved_games_restore_with_their_bindings_but_not_their_values() {
        let mut r = with_bar();
        r.game_metadata(
            serde_json::from_value(json!({ "game": "G", "game_display_name": "Game",
            "developer": "Dev", "deinitialize_timer_length_ms": 5000 }))
            .unwrap(),
        )
        .unwrap();
        r.set_enabled("G", false).unwrap();
        send(&mut r, "G", "HEALTH", 70.0, 0.0).unwrap();
        let saved = r.saved();
        let json = serde_json::to_string(&saved).unwrap();
        let mut back = Registry::restore(serde_json::from_str(&json).unwrap());
        assert_eq!(back.saved(), saved);
        let s = &back.summaries(0.0)[0];
        assert_eq!(
            (
                s.display_name.as_deref(),
                s.developer.as_deref(),
                s.enabled,
                s.active
            ),
            (Some("Game"), Some("Dev"), false, false)
        );
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

#[cfg(test)]
mod screen_tests;
