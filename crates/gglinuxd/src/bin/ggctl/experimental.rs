// SPDX-License-Identifier: GPL-3.0-or-later
//! `ggctl experimental`: the features that write to a device but haven't been tried on
//! your hardware yet. Each is off until turned on here or in Settings → Experimental.

use anyhow::{bail, Context, Result};
use gg_ipc::{Experimental, Feature, Request};

use crate::{on_off, Action};

pub fn action(rest: &[&str]) -> Result<Action> {
    Ok(match rest {
        [] => Action::Send(Request::GetExperimental),
        [name, v] => {
            let feature = Feature::from_key(name).with_context(|| {
                let names: Vec<&str> = Feature::ALL.iter().map(|f| f.key()).collect();
                format!(
                    "no experimental feature '{name}' (try: {})",
                    names.join(", ")
                )
            })?;
            Action::SetFeature(feature, on_off(v)?)
        }
        _ => bail!("usage: ggctl experimental [<name> on|off]"),
    })
}

/// What `ggctl experimental` prints: every feature, on or off, and what it is.
pub fn print(experimental: &Experimental) {
    for line in lines(experimental) {
        println!("{line}");
    }
}

fn lines(experimental: &Experimental) -> Vec<String> {
    Feature::ALL
        .iter()
        .map(|&f| {
            let on = if experimental.on(f) { "on" } else { "off" };
            format!("{:<20} {on:<3}  {}", f.key(), f.label())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{args, parse, sends};

    fn sets(line: &str) -> (Feature, bool) {
        match parse(&args(line)).unwrap() {
            Action::SetFeature(f, on) => (f, on),
            _ => panic!("{line} is not an experimental change"),
        }
    }

    #[test]
    fn experimental_lists_or_turns_one_feature_on_or_off_by_its_name() {
        assert!(matches!(sends("experimental"), Request::GetExperimental));
        assert_eq!(
            sets("experimental keyboard_onboard on"),
            (Feature::KeyboardOnboard, true)
        );
        assert_eq!(
            sets("experimental mouse_power off"),
            (Feature::MousePower, false)
        );
        let unknown = parse(&args("experimental keyboard_flash on"))
            .err()
            .unwrap();
        assert!(
            unknown
                .to_string()
                .contains("keyboard_bindings, keyboard_onboard"),
            "{unknown}"
        );
        for bad in [
            "experimental keyboard_onboard maybe",
            "experimental keyboard_onboard",
            "experimental keyboard_onboard on now",
        ] {
            assert!(parse(&args(bad)).is_err(), "{bad}");
        }
    }

    #[test]
    fn the_list_has_every_feature_with_its_state_and_label() {
        let mut e = Experimental::default();
        e.set(Feature::KeyboardBindings, true);
        let lines = lines(&e);
        assert_eq!(lines.len(), Feature::ALL.len());
        assert_eq!(
            lines[0],
            "keyboard_bindings    on   Keyboard: key bindings and deep press"
        );
        assert_eq!(
            lines[1],
            "keyboard_onboard     off  Keyboard: onboard profile slots"
        );
        for (line, f) in lines.iter().zip(Feature::ALL) {
            assert!(
                line.starts_with(f.key()) && line.ends_with(f.label()),
                "{line}"
            );
        }
    }
}
