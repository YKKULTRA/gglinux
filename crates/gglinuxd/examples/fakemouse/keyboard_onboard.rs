// SPDX-License-Identifier: GPL-3.0-or-later
//! The fake keyboard's onboard profile slots: five made-up images ("Config 1" to "Config 5",
//! each checking out) in memory, behind the real slot code. A read, load or write runs the
//! same checked transaction the daemon runs, against memory instead of flash, so a write
//! takes about as long as on the keyboard. Refused while `keyboard_onboard` is off, as the
//! daemon refuses. Backups are kept in memory under made-up paths, and a backup is written
//! back only if it's one of those. Its slots always check out, so a `recover` is refused
//! as the daemon refuses it for a slot that checks out.
//!
//! Like a device, it lags: the first `get_onboard` after an operation still answers with
//! the state from before it. The operation's own answer is the state after it.

use std::sync::Mutex;

use gg_device_keyboard::onboard::fake::FakeSlots;
use gg_device_keyboard::onboard::{Image, Timing, Written, SLOTS};
use gg_device_keyboard::{oled, Bitmap, Families, Keyboard};
use gg_ipc::{
    check_slot, Feature, OnboardSlot, OnboardSource, OnboardState, Request, ResponseData, Screen,
    Switches, TapMode,
};

use crate::log::method_name;
use crate::{error, Fake};

/// Where the made-up backups seem to be.
const BACKUPS: &str = "/home/you/.config/gglinux/onboard-backups";

struct Slots {
    keyboard: Keyboard,
    state: OnboardState,
    /// What the next `get_onboard` answers: the state from before the last operation.
    before: Option<OnboardState>,
    /// Path and bytes of every backup made.
    backups: Vec<(String, Vec<u8>)>,
}

/// The fake's slots, made on first use (the fake's own state is the lead's).
static SLOTS_KEPT: Mutex<Option<Slots>> = Mutex::new(None);

impl Slots {
    fn new() -> Slots {
        let keyboard = FakeSlots::new().keyboard();
        // The gate is checked before anything gets here; the allowlist is the keyboard's.
        keyboard.set_families(Families {
            bindings: false,
            onboard: true,
        });
        Slots {
            keyboard,
            state: OnboardState::default(),
            before: None,
            backups: Vec::new(),
        }
    }

    /// A slot's entry: none are listed until something is known about one, as the daemon
    /// does it.
    fn slot(&mut self, slot: u8) -> &mut OnboardSlot {
        if self.state.slots.len() < usize::from(SLOTS) {
            self.state.slots = (0..SLOTS)
                .map(|index| OnboardSlot {
                    index,
                    ..OnboardSlot::default()
                })
                .collect();
        }
        &mut self.state.slots[usize::from(slot)]
    }

    fn back_up(&mut self, slot: u8, image: &Image) -> String {
        let n = self.backups.len();
        let path = format!(
            "{BACKUPS}/slot{}-20261106T12{:02}{:02}Z.bin",
            slot + 1,
            n / 60 % 60,
            n % 60
        );
        self.backups.push((path.clone(), image.serialize()));
        self.slot(slot).backup = Some(path.clone());
        path
    }

    fn read(&mut self, slot: Option<u8>) -> Result<(), String> {
        let slots: Vec<u8> = slot.map_or_else(|| (0..SLOTS).collect(), |s| vec![s]);
        for s in slots {
            let image = self
                .keyboard
                .read_image(s, &Timing::KEYBOARD)
                .map_err(|e| format!("slot {} couldn't be read: {e:#}", s + 1))?;
            self.back_up(s, &image);
            let kept = self.slot(s);
            kept.name = Some(image.name());
            kept.valid = Some(true);
        }
        Ok(())
    }

    /// One of the made-up backups, checked.
    fn backup(&self, path: &str) -> Result<Image, String> {
        let bytes = self
            .backups
            .iter()
            .find(|(p, _)| p == path)
            .map(|(_, b)| b.clone())
            .ok_or_else(|| format!("reading {path}: the fake daemon has no such backup"))?;
        Image::parse(&bytes).map_err(|e| format!("{e:#}"))
    }

    fn write(&mut self, slot: u8, from: OnboardSource, s: &Fake) -> Result<(), String> {
        let replacement = match &from {
            OnboardSource::Current => None,
            OnboardSource::Backup { path } => Some(self.backup(path)?),
            OnboardSource::Recover { path } => return self.recover(slot, path),
        };
        let (switches, screen) = (&s.keyboard.switches, &s.keyboard.screen);
        let mut saved = None;
        let mut name = None;
        let result = self.keyboard.write_slot(
            slot,
            &Timing::KEYBOARD,
            |old| {
                saved = Some(old.clone());
                Ok(old.serialize())
            },
            |old| {
                let new = match replacement {
                    Some(image) => image,
                    None => with_settings(old, switches, screen)?,
                };
                name = Some(new.name());
                Ok(new)
            },
        );
        if let Some(old) = saved {
            self.back_up(slot, &old);
        }
        let written = result.map_err(|e| e.to_string())?;
        if let Written::Written(check) = written {
            self.state.note = check.warning();
        }
        let kept = self.slot(slot);
        kept.name = name;
        kept.valid = Some(true);
        Ok(())
    }

    /// As the daemon does it: refused for a slot that checks out, which all of these do.
    fn recover(&mut self, slot: u8, path: &str) -> Result<(), String> {
        let image = self.backup(path)?;
        let check = self
            .keyboard
            .recover_slot(slot, &Timing::KEYBOARD, &image, |raw| Ok(raw.to_vec()))
            .map_err(|e| e.to_string())?;
        self.state.note = check.warning();
        let kept = self.slot(slot);
        kept.name = Some(image.name());
        kept.valid = Some(true);
        Ok(())
    }
}

/// As the daemon does it: the Rapid Tap pairs if gglinux sets the switches, and a still
/// screen picture.
fn with_settings(old: &Image, switches: &Switches, screen: &Screen) -> anyhow::Result<Image> {
    let mut new = old.clone();
    if switches.enabled {
        let pairs: Vec<gg_device_keyboard::TapPair> = switches
            .rapid_tap
            .pairs
            .iter()
            .map(|p| gg_device_keyboard::TapPair {
                a: p.a,
                b: p.b,
                mode: match p.mode {
                    TapMode::LastWins => gg_device_keyboard::TapMode::LastWins,
                    TapMode::Neither => gg_device_keyboard::TapMode::Neither,
                    TapMode::FirstWins => gg_device_keyboard::TapMode::FirstWins,
                    TapMode::SecondWins => gg_device_keyboard::TapMode::SecondWins,
                },
                report_both: p.report_both,
            })
            .collect();
        new.set_rapid_tap(switches.rapid_tap.enabled, &pairs)?;
    }
    match screen {
        Screen::Text { text } => new.set_oled(&oled::text(text)),
        Screen::Image { pixels } => {
            if let Ok(picture) = Bitmap::from_rows(pixels) {
                new.set_oled(&picture);
            }
        }
        _ => {}
    }
    Ok(new)
}

pub fn handle(req: Request, s: &mut Fake) -> ResponseData {
    let mut kept = SLOTS_KEPT.lock().unwrap_or_else(|e| e.into_inner());
    let slots = kept.get_or_insert_with(Slots::new);
    if let Request::GetOnboard = req {
        let state = slots.before.take().unwrap_or_else(|| slots.state.clone());
        return ResponseData::Onboard { state };
    }
    if let Some(why) = s.experimental.refusal(Feature::KeyboardOnboard) {
        return error(why);
    }
    let checked = match &req {
        Request::ReadOnboard { slot } => slot.map(check_slot).transpose().map(|_| ()),
        Request::LoadOnboard { slot } | Request::WriteOnboard { slot, .. } => check_slot(*slot),
        other => Err(format!("{} is not an onboard request", method_name(other))),
    };
    if let Err(message) = checked {
        return error(message);
    }
    let before = slots.state.clone();
    slots.state.note = None;
    let result = match req {
        Request::ReadOnboard { slot } => slots.read(slot),
        Request::LoadOnboard { slot } => slots
            .keyboard
            .load_slot(slot)
            .map(|()| slots.state.active = Some(slot))
            .map_err(|e| format!("{e:#}")),
        Request::WriteOnboard { slot, from } => slots.write(slot, from, s),
        _ => unreachable!("checked above"),
    };
    slots.state.error = result.as_ref().err().cloned();
    slots.before = Some(before);
    match result {
        Ok(()) => ResponseData::Onboard {
            state: slots.state.clone(),
        },
        Err(message) => error(message),
    }
}
