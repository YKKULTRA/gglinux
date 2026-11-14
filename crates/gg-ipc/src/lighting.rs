// SPDX-License-Identifier: GPL-3.0-or-later
//! Lighting zones, the idle scene and Reflect: the parts of [`Lighting`] added in M12.
//!
//! A zone is a named set of keys with its own effect. The zones form a stack, bottom
//! first: each zone paints its keys over the zones before it. Lighting saved before zones
//! existed has one effect over every key; [`Lighting::stack`] reads it as one zone, "all",
//! with that effect, so it looks the same until the zones are edited.
//!
//! The idle scene is a second stack, drawn after nobody has been at the computer for a
//! while. Reflect is a zone effect: each key takes the colour of the screen above it. It
//! is behind [`crate::Feature::LightingReflect`].

use std::borrow::Cow;
use std::ops::RangeInclusive;

use serde::{Deserialize, Serialize};

use crate::{Effect, Lighting, ReactiveMode, Rgb};

/// Which keys a zone covers.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ZoneKeys {
    /// Every key of the keyboard, whatever its region.
    All,
    /// These keys, by HID code. A code the keyboard doesn't have lights nothing.
    Keys { codes: Vec<u8> },
}

impl ZoneKeys {
    pub fn contains(&self, code: u8) -> bool {
        match self {
            ZoneKeys::All => true,
            ZoneKeys::Keys { codes } => codes.contains(&code),
        }
    }
}

/// Key sets offered for a new zone. They are gglinux's own picks, not GG's.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ZonePreset {
    Wasd,
    Arrows,
    FRow,
    NumberRow,
    All,
}

impl ZonePreset {
    pub const ALL: [ZonePreset; 5] = [
        ZonePreset::Wasd,
        ZonePreset::Arrows,
        ZonePreset::FRow,
        ZonePreset::NumberRow,
        ZonePreset::All,
    ];

    /// The name `ggctl lighting zones` takes.
    pub fn key(self) -> &'static str {
        match self {
            ZonePreset::Wasd => "wasd",
            ZonePreset::Arrows => "arrows",
            ZonePreset::FRow => "f-row",
            ZonePreset::NumberRow => "numbers",
            ZonePreset::All => "all",
        }
    }

    /// The name the app shows, also the new zone's name.
    pub fn label(self) -> &'static str {
        match self {
            ZonePreset::Wasd => "WASD",
            ZonePreset::Arrows => "Arrows",
            ZonePreset::FRow => "F-row",
            ZonePreset::NumberRow => "Number row",
            ZonePreset::All => "Every key",
        }
    }

    pub fn from_key(key: &str) -> Option<ZonePreset> {
        ZonePreset::ALL.into_iter().find(|p| p.key() == key)
    }

    pub fn keys(self) -> ZoneKeys {
        let codes = match self {
            // W, A, S, D
            ZonePreset::Wasd => vec![0x1A, 0x04, 0x16, 0x07],
            // Up, Left, Down, Right
            ZonePreset::Arrows => vec![0x52, 0x50, 0x51, 0x4F],
            // F1 to F12
            ZonePreset::FRow => (0x3A..=0x45).collect(),
            // 1 to 0
            ZonePreset::NumberRow => (0x1E..=0x27).collect(),
            ZonePreset::All => return ZoneKeys::All,
        };
        ZoneKeys::Keys { codes }
    }
}

/// What a zone shows on its keys.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ZoneEffect {
    /// One colour on every key of the zone.
    Static { color: Rgb },
    /// Each key its own colour, from [`Lighting::keys`]; keys not listed there are dark.
    PerKey,
    /// The zone pulses one colour, one breath per `period_s`.
    Breathing { color: Rgb, period_s: f32 },
    /// A rainbow moving across the zone, one pass per `period_s`.
    Wave { period_s: f32 },
    /// `from` on the zone's left edge, blending into `to` on its right edge.
    Gradient { from: Rgb, to: Rgb },
    /// The zone cycles the rainbow together, once per `period_s`.
    Spectrum { period_s: f32 },
    /// The zone's part of a spectrum analyser of whatever is playing.
    Visualizer { sensitivity: f32 },
    /// Keys light as they are pressed and fade back; between presses the zones below show.
    Reactive {
        mode: ReactiveMode,
        color: Rgb,
        duration_s: f32,
    },
    /// Each key the colour of the part of the screen it sits under. Needs
    /// [`crate::Feature::LightingReflect`]. Until a frame of the screen has come, the zones
    /// below show.
    Reflect,
}

impl From<Effect> for ZoneEffect {
    /// The effect lighting had before zones, as a zone's: its static colours come from
    /// [`Lighting::keys`], as they always did.
    fn from(e: Effect) -> ZoneEffect {
        match e {
            Effect::Static => ZoneEffect::PerKey,
            Effect::Breathing { color, period_s } => ZoneEffect::Breathing { color, period_s },
            Effect::Wave { period_s } => ZoneEffect::Wave { period_s },
            Effect::Spectrum { period_s } => ZoneEffect::Spectrum { period_s },
            Effect::Visualizer { sensitivity } => ZoneEffect::Visualizer { sensitivity },
        }
    }
}

impl ZoneEffect {
    /// Whether it changes by itself, so frames must keep coming. A reactive zone changes
    /// only with presses, which ask for frames themselves.
    pub fn animated(&self) -> bool {
        matches!(
            self,
            ZoneEffect::Breathing { .. }
                | ZoneEffect::Wave { .. }
                | ZoneEffect::Spectrum { .. }
                | ZoneEffect::Visualizer { .. }
                | ZoneEffect::Reflect
        )
    }

    /// The name `ggctl` prints and takes first.
    pub fn name(&self) -> &'static str {
        match self {
            ZoneEffect::Static { .. } => "static",
            ZoneEffect::PerKey => "per-key",
            ZoneEffect::Breathing { .. } => "breathing",
            ZoneEffect::Wave { .. } => "wave",
            ZoneEffect::Gradient { .. } => "gradient",
            ZoneEffect::Spectrum { .. } => "spectrum",
            ZoneEffect::Visualizer { .. } => "audio",
            ZoneEffect::Reactive { .. } => "reactive",
            ZoneEffect::Reflect => "reflect",
        }
    }

    /// Check ranges, with a message meant for the user.
    pub fn validate(&self) -> Result<(), String> {
        match *self {
            ZoneEffect::Breathing { period_s, .. }
            | ZoneEffect::Wave { period_s }
            | ZoneEffect::Spectrum { period_s } => {
                if !(period_s.is_finite() && Lighting::PERIOD_S.contains(&period_s)) {
                    return Err(format!(
                        "effect period {period_s} s out of range (0.5..=60)"
                    ));
                }
            }
            ZoneEffect::Visualizer { sensitivity } => {
                if !(sensitivity.is_finite() && Lighting::SENSITIVITY.contains(&sensitivity)) {
                    return Err(format!(
                        "visualizer sensitivity {sensitivity} out of range (0.25..=4)"
                    ));
                }
            }
            ZoneEffect::Reactive { duration_s, .. } => {
                if !(duration_s.is_finite() && crate::Reactive::DURATION_S.contains(&duration_s)) {
                    return Err(format!(
                        "reactive duration {duration_s} s out of range (0.1..=3)"
                    ));
                }
            }
            ZoneEffect::Static { .. }
            | ZoneEffect::PerKey
            | ZoneEffect::Gradient { .. }
            | ZoneEffect::Reflect => {}
        }
        Ok(())
    }
}

/// A named set of keys with its own effect.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Zone {
    pub name: String,
    pub keys: ZoneKeys,
    pub effect: ZoneEffect,
}

impl Zone {
    /// The longest name, in characters.
    pub const NAME_LEN: usize = 32;
    /// The most keys a zone lists.
    pub const MAX_KEYS: usize = 128;
    /// The name of the zone lighting from before zones reads as.
    pub const ALL: &'static str = "all";

    pub fn new(name: impl Into<String>, keys: ZoneKeys, effect: ZoneEffect) -> Zone {
        Zone {
            name: name.into(),
            keys,
            effect,
        }
    }

    fn validate(&self) -> Result<(), String> {
        let name = self.name.trim();
        if name.is_empty() {
            return Err("a zone needs a name".into());
        }
        if name != self.name || self.name.chars().count() > Zone::NAME_LEN {
            return Err(format!(
                "zone name '{}' must be 1 to {} characters, without spaces at either end",
                self.name,
                Zone::NAME_LEN
            ));
        }
        if let ZoneKeys::Keys { codes } = &self.keys {
            if codes.is_empty() {
                return Err(format!("zone '{}' has no keys", self.name));
            }
            if codes.len() > Zone::MAX_KEYS {
                return Err(format!(
                    "zone '{}' lists {} keys (at most {})",
                    self.name,
                    codes.len(),
                    Zone::MAX_KEYS
                ));
            }
        }
        self.effect
            .validate()
            .map_err(|e| format!("zone '{}': {e}", self.name))
    }
}

/// Check a stack: how many zones, their names (each once) and their effects.
fn validate_stack(zones: &[Zone], what: &str) -> Result<(), String> {
    if zones.len() > Lighting::MAX_ZONES {
        return Err(format!(
            "{what} has {} zones (at most {})",
            zones.len(),
            Lighting::MAX_ZONES
        ));
    }
    for (i, z) in zones.iter().enumerate() {
        z.validate()?;
        if zones[..i].iter().any(|o| o.name == z.name) {
            return Err(format!("two zones are called '{}'", z.name));
        }
    }
    Ok(())
}

/// The lighting while nobody is at the computer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct IdleLighting {
    pub enabled: bool,
    /// Minutes after the desktop says nobody is there (locked, idle, switched away or the
    /// screensaver on) before the idle scene starts. The desktop's own idle time comes
    /// first.
    pub after_minutes: u16,
    /// The idle scene's own zone stack, bottom first; empty is every key dark.
    pub zones: Vec<Zone>,
}

impl Default for IdleLighting {
    /// Off; when turned on, every key dark five minutes after the desktop says nobody is
    /// there. Five minutes is gglinux's own choice.
    fn default() -> Self {
        IdleLighting {
            enabled: false,
            after_minutes: 5,
            zones: Vec::new(),
        }
    }
}

/// Whether Reflect is looking at the screen.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum ReflectStatus {
    /// No Reflect zone is drawn now.
    #[default]
    Off,
    /// Taking frames of the screen.
    Running,
    /// A Reflect zone is drawn, but the screen can't be seen: why, for the user.
    Stopped { why: String },
}

impl ReflectStatus {
    /// Why Reflect stops while the screen capture isn't built into gglinux.
    pub const NOT_BUILT: &'static str =
        "Reflect can't see the screen yet: screen capture isn't built into this version of gglinux";
}

impl Lighting {
    /// The most zones in one stack.
    pub const MAX_ZONES: usize = 16;
    pub const IDLE_MINUTES: RangeInclusive<u16> = 0..=120;

    /// The zone stack drawn while somebody is at the computer, bottom first. Lighting
    /// saved before zones existed reads as its effect over every key, in one zone.
    pub fn stack(&self) -> Cow<'_, [Zone]> {
        match &self.zones {
            Some(zones) => Cow::Borrowed(zones),
            None => Cow::Owned(vec![Zone::new(
                Zone::ALL,
                ZoneKeys::All,
                ZoneEffect::from(self.effect),
            )]),
        }
    }

    /// The zone stack, written out to be edited: lighting from before zones gets its one
    /// zone first.
    pub fn zones_mut(&mut self) -> &mut Vec<Zone> {
        if self.zones.is_none() {
            self.zones = Some(self.stack().into_owned());
        }
        self.zones.get_or_insert_with(Vec::new)
    }

    /// Whether a Reflect zone is in either stack, the usual one or the idle scene's.
    pub fn uses_reflect(&self) -> bool {
        let reflect = |z: &Zone| z.effect == ZoneEffect::Reflect;
        self.stack().iter().any(reflect) || self.idle.zones.iter().any(reflect)
    }

    /// Put an every-key Reflect zone into the stack, if there's no Reflect zone yet: over
    /// the last zone that covers every key, so smaller zones (WASD, say) still show over
    /// the screen's colours.
    pub fn reflect_on(&mut self) {
        let zones = self.zones_mut();
        if zones.iter().any(|z| z.effect == ZoneEffect::Reflect) {
            return;
        }
        let at = zones
            .iter()
            .rposition(|z| z.keys == ZoneKeys::All)
            .map_or(0, |i| i + 1);
        let mut name = String::from("Screen");
        let mut n = 2;
        while zones.iter().any(|z| z.name == name) {
            name = format!("Screen {n}");
            n += 1;
        }
        zones.insert(at, Zone::new(name, ZoneKeys::All, ZoneEffect::Reflect));
    }

    /// Take every Reflect zone out of the usual stack.
    pub fn reflect_off(&mut self) {
        if self.stack().iter().any(|z| z.effect == ZoneEffect::Reflect) {
            self.zones_mut().retain(|z| z.effect != ZoneEffect::Reflect);
        }
    }

    /// The zones' and the idle scene's part of [`Lighting::validate`].
    pub(crate) fn validate_zones(&self) -> Result<(), String> {
        if let Some(zones) = &self.zones {
            validate_stack(zones, "the lighting")?;
        }
        let idle = &self.idle;
        if !Lighting::IDLE_MINUTES.contains(&idle.after_minutes) {
            return Err(format!(
                "idle time {} min out of range (0..=120)",
                idle.after_minutes
            ));
        }
        validate_stack(&idle.zones, "the idle scene")
    }
}
