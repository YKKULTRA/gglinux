// SPDX-License-Identifier: GPL-3.0-or-later
//! Features that write to a device but haven't been tried on the owner's hardware yet.
//!
//! Each one is off until it is turned on in Settings → Experimental (or with
//! `ggctl experimental <name> on`). While a feature is off the daemon refuses to send its
//! commands, so a new protocol path can be built, tested against fakes and shipped without
//! ever reaching a device by accident.

use serde::{Deserialize, Serialize};

/// One gated feature.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Feature {
    /// Keyboard key remapping, the Fn layer and the second (deep-press) actuation point.
    KeyboardBindings,
    /// Reading, loading and writing the keyboard's onboard profile slots.
    KeyboardOnboard,
    /// Protection Mode on the keyboard.
    KeyboardProtection,
    /// Macros stored on the keyboard.
    KeyboardMacros,
    /// The mouse's sleep timer, battery saving, High-Efficiency Mode, Wireless Stability and
    /// Bluetooth Smoothing.
    MousePower,
    /// What the mouse's scroll wheel does when turned up and down.
    MouseWheel,
    /// The mouse's angle snapping, acceleration and lift-off settings.
    MouseSensor,
    /// Macros stored on the mouse.
    MouseMacros,
    /// Pairing the mouse with its dongle.
    MousePairing,
    /// The headset settings added after M10 (Bluetooth, gain, line out, base-station screen).
    HeadsetExtras,
    /// Keyboard lighting zones that take the colours of the screen.
    LightingReflect,
}

impl Feature {
    pub const ALL: [Feature; 11] = [
        Feature::KeyboardBindings,
        Feature::KeyboardOnboard,
        Feature::KeyboardProtection,
        Feature::KeyboardMacros,
        Feature::MousePower,
        Feature::MouseWheel,
        Feature::MouseSensor,
        Feature::MouseMacros,
        Feature::MousePairing,
        Feature::HeadsetExtras,
        Feature::LightingReflect,
    ];

    /// The name `ggctl experimental` takes, and the settings file uses.
    pub fn key(self) -> &'static str {
        match self {
            Feature::KeyboardBindings => "keyboard_bindings",
            Feature::KeyboardOnboard => "keyboard_onboard",
            Feature::KeyboardProtection => "keyboard_protection",
            Feature::KeyboardMacros => "keyboard_macros",
            Feature::MousePower => "mouse_power",
            Feature::MouseWheel => "mouse_wheel",
            Feature::MouseSensor => "mouse_sensor",
            Feature::MouseMacros => "mouse_macros",
            Feature::MousePairing => "mouse_pairing",
            Feature::HeadsetExtras => "headset_extras",
            Feature::LightingReflect => "lighting_reflect",
        }
    }

    /// The name Settings shows.
    pub fn label(self) -> &'static str {
        match self {
            Feature::KeyboardBindings => "Keyboard: key bindings and deep press",
            Feature::KeyboardOnboard => "Keyboard: onboard profile slots",
            Feature::KeyboardProtection => "Keyboard: Protection Mode",
            Feature::KeyboardMacros => "Keyboard: macros",
            Feature::MousePower => "Mouse: sleep timer and power settings",
            Feature::MouseWheel => "Mouse: scroll wheel actions",
            Feature::MouseSensor => "Mouse: angle snapping and sensor settings",
            Feature::MouseMacros => "Mouse: macros",
            Feature::MousePairing => "Mouse: pairing",
            Feature::HeadsetExtras => "Headset: Bluetooth, gain and line out",
            Feature::LightingReflect => "Lighting: Reflect screen colours",
        }
    }

    pub fn from_key(key: &str) -> Option<Feature> {
        Feature::ALL.into_iter().find(|f| f.key() == key)
    }
}

/// Which gated features are on. All off by default; a missing field reads as off.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Experimental {
    pub keyboard_bindings: bool,
    pub keyboard_onboard: bool,
    pub keyboard_protection: bool,
    pub keyboard_macros: bool,
    pub mouse_power: bool,
    pub mouse_wheel: bool,
    pub mouse_sensor: bool,
    pub mouse_macros: bool,
    pub mouse_pairing: bool,
    pub headset_extras: bool,
    pub lighting_reflect: bool,
}

impl Experimental {
    pub fn on(&self, feature: Feature) -> bool {
        match feature {
            Feature::KeyboardBindings => self.keyboard_bindings,
            Feature::KeyboardOnboard => self.keyboard_onboard,
            Feature::KeyboardProtection => self.keyboard_protection,
            Feature::KeyboardMacros => self.keyboard_macros,
            Feature::MousePower => self.mouse_power,
            Feature::MouseWheel => self.mouse_wheel,
            Feature::MouseSensor => self.mouse_sensor,
            Feature::MouseMacros => self.mouse_macros,
            Feature::MousePairing => self.mouse_pairing,
            Feature::HeadsetExtras => self.headset_extras,
            Feature::LightingReflect => self.lighting_reflect,
        }
    }

    pub fn set(&mut self, feature: Feature, on: bool) {
        let field = match feature {
            Feature::KeyboardBindings => &mut self.keyboard_bindings,
            Feature::KeyboardOnboard => &mut self.keyboard_onboard,
            Feature::KeyboardProtection => &mut self.keyboard_protection,
            Feature::KeyboardMacros => &mut self.keyboard_macros,
            Feature::MousePower => &mut self.mouse_power,
            Feature::MouseWheel => &mut self.mouse_wheel,
            Feature::MouseSensor => &mut self.mouse_sensor,
            Feature::MouseMacros => &mut self.mouse_macros,
            Feature::MousePairing => &mut self.mouse_pairing,
            Feature::HeadsetExtras => &mut self.headset_extras,
            Feature::LightingReflect => &mut self.lighting_reflect,
        };
        *field = on;
    }

    /// Why a command of `feature` isn't sent, or `None` if it may be.
    pub fn refusal(&self, feature: Feature) -> Option<String> {
        (!self.on(feature)).then(|| {
            format!(
                "“{}” hasn't been tried on your hardware yet: turn it on in Settings → Experimental first",
                feature.label()
            )
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn everything_is_off_by_default_and_when_missing() {
        let e = Experimental::default();
        assert!(Feature::ALL.iter().all(|&f| !e.on(f)));
        let parsed: Experimental = serde_json::from_str("{}").unwrap();
        assert_eq!(parsed, e);
        let parsed: Experimental = serde_json::from_str(r#"{"mouse_power":true}"#).unwrap();
        assert!(parsed.on(Feature::MousePower));
        assert!(!parsed.on(Feature::MouseSensor));
    }

    #[test]
    fn set_and_on_agree_for_every_feature_and_keys_are_unique() {
        let mut keys = std::collections::HashSet::new();
        for f in Feature::ALL {
            let mut e = Experimental::default();
            e.set(f, true);
            assert!(e.on(f), "{f:?}");
            assert_eq!(
                Feature::ALL.iter().filter(|&&g| e.on(g)).count(),
                1,
                "{f:?}"
            );
            assert_eq!(Feature::from_key(f.key()), Some(f));
            assert!(keys.insert(f.key()));
            // The settings file's field name is the key.
            let json = serde_json::to_value(e).unwrap();
            assert_eq!(json[f.key()], true, "{f:?}");
        }
    }

    /// The wheel's gate: its own name and label, off unless the file says otherwise.
    #[test]
    fn the_wheel_has_its_own_gate_off_by_default() {
        assert_eq!(Feature::from_key("mouse_wheel"), Some(Feature::MouseWheel));
        assert_eq!(Feature::MouseWheel.label(), "Mouse: scroll wheel actions");
        let old: Experimental = serde_json::from_str(r#"{"mouse_power":true}"#).unwrap();
        assert!(!old.on(Feature::MouseWheel), "a file from before it");
        let why = old.refusal(Feature::MouseWheel).unwrap();
        assert!(why.contains("scroll wheel") && why.contains("Settings → Experimental"));
    }

    /// Reflect's gate: its own name and label, off unless the file says otherwise.
    #[test]
    fn reflect_has_its_own_gate_off_by_default() {
        let f = Feature::from_key("lighting_reflect");
        assert_eq!(f, Some(Feature::LightingReflect));
        assert_eq!(
            Feature::LightingReflect.label(),
            "Lighting: Reflect screen colours"
        );
        let old: Experimental = serde_json::from_str(r#"{"headset_extras":true}"#).unwrap();
        assert!(!old.on(Feature::LightingReflect), "a file from before it");
        let why = old.refusal(Feature::LightingReflect).unwrap();
        assert!(why.contains("hasn't been tried on your hardware yet"));
    }

    #[test]
    fn a_refusal_names_the_feature_and_where_to_turn_it_on() {
        let mut e = Experimental::default();
        let why = e.refusal(Feature::KeyboardOnboard).unwrap();
        assert!(why.contains("onboard profile slots") && why.contains("Settings → Experimental"));
        e.set(Feature::KeyboardOnboard, true);
        assert_eq!(e.refusal(Feature::KeyboardOnboard), None);
    }
}
