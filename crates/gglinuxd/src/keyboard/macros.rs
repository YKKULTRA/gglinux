// SPDX-License-Identifier: GPL-3.0-or-later
//! Keyboard macros: the worker's part ([`Store`], [`on_command`]) and the clients'
//! requests ([`handle`]).
//!
//! The library (the macros and the keys that play them) is kept in `<config>/macros.json`
//! with the slot it was last written into. `SetMacros` checks it and saves it, and sends
//! the keyboard nothing. `WriteMacros` puts it into one onboard slot, through the onboard
//! slots' checked transaction ([`Keyboard::write_slot`]: read, back up, write, read back,
//! put back on any mismatch); the change rewrites only the slot's macro area, the bound
//! keys' places and the CRC ([`gg_device_keyboard::macros::write`]). A slot whose macros
//! gglinux can't read is left alone. Writing the slot the keyboard runs loads it again, so
//! the macros take effect; the worker then tells the bindings and switches what a load
//! tells them.
//!
//! `SetMacros` needs `keyboard_macros`; `WriteMacros` needs it and `keyboard_onboard`,
//! each refused with [`Experimental::refusal`]'s words for the one that's off. The open
//! keyboard's allowlist lets the slot reports out only with the onboard family on. A write
//! says `busy` from its start to its end, announced each time ([`Event::MacrosChanged`]),
//! and counts in the onboard slots' [`Writes`](super::onboard::Writes) so the daemon
//! waits for it before it stops.

use std::path::{Path, PathBuf};

use gg_device_keyboard::macros::{self as slot, Event as SlotEvent, Mode, SlotMacro};
use gg_device_keyboard::onboard::{Restore, Timing, Written};
use gg_device_keyboard::Keyboard;
use gg_ipc::{
    check_slot, Event, Experimental, Feature, MacroEvent, MacroLibrary, MacroMode, MacrosState,
    Request, ResponseData,
};
use serde::{Deserialize, Serialize};
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::onboard::{save_backup, Running};
use super::{Command, Handle, Reply};

/// The onboard slots' backups folder (the onboard module's), where a write's backup goes
/// so the slots' list finds it too.
const BACKUPS: &str = "onboard-backups";

/// What `macros.json` holds.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
struct Saved {
    library: MacroLibrary,
    written_slot: Option<u8>,
}

/// What the keyboard worker keeps for the macros.
pub struct Store {
    state: MacrosState,
    config_dir: PathBuf,
}

impl Store {
    /// The macros saved in `config_dir`, or none.
    pub fn load(config_dir: &Path) -> Store {
        let saved: Saved = super::load(&config_dir.join(FILE), "macros");
        Store {
            state: MacrosState {
                library: saved.library,
                written_slot: saved.written_slot,
                ..MacrosState::default()
            },
            config_dir: config_dir.to_owned(),
        }
    }

    fn save(&self) {
        let saved = Saved {
            library: self.state.library.clone(),
            written_slot: self.state.written_slot,
        };
        super::save(&self.config_dir.join(FILE), &saved, "macros");
    }
}

/// The file the macros are kept in, in the config folder.
const FILE: &str = "macros.json";

/// The keyboard worker's commands for the macros. Each answers with the state after it.
pub enum Cmd {
    Get(Reply<MacrosState>),
    Set {
        library: MacroLibrary,
        reply: Reply<MacrosState>,
    },
    Write {
        slot: u8,
        reply: Reply<MacrosState>,
        /// Counted in the handle's slot writes until the write ends.
        running: Running,
    },
}

/// One command, on the keyboard worker's thread. `device` is the open keyboard, if any;
/// `active` the slot it runs, if known. Returns whether the keyboard loaded a slot again.
pub fn on_command(
    cmd: Cmd,
    store: &mut Store,
    device: Option<&Keyboard>,
    exp: &Experimental,
    event_tx: &broadcast::Sender<Event>,
    active: Option<u8>,
) -> bool {
    match cmd {
        Cmd::Get(reply) => {
            let _ = reply.send(Ok(store.state.clone()));
            false
        }
        Cmd::Set { library, reply } => {
            let answer = set(store, exp, library).map(|()| {
                announce(store, event_tx);
                store.state.clone()
            });
            let _ = reply.send(answer);
            false
        }
        Cmd::Write {
            slot,
            reply,
            running,
        } => {
            let (answer, loaded) = match ready(exp, device) {
                Ok(kb) => write(store, kb, slot, active, event_tx),
                Err(why) => (Err(why), false),
            };
            drop(running);
            let _ = reply.send(answer);
            loaded
        }
    }
}

/// Check a new library and keep it; nothing is sent.
fn set(store: &mut Store, exp: &Experimental, library: MacroLibrary) -> Result<(), String> {
    if let Some(why) = exp.refusal(Feature::KeyboardMacros) {
        return Err(why);
    }
    check(&library)?;
    store.state.library = library;
    store.save();
    Ok(())
}

/// The library's own checks, and that each key has a place for a macro.
fn check(library: &MacroLibrary) -> Result<(), String> {
    library.validate()?;
    match library.bindings.iter().find(|b| slot::place(b.code).is_none()) {
        Some(b) => Err(format!(
            "key {} can't play a macro (the Fn key and the media buttons can't, nor can a key this keyboard doesn't have)",
            gg_ipc::MacroBinding::key_name(b.code)
        )),
        None => Ok(()),
    }
}

/// The open keyboard, if both gates are on and it's there.
fn ready<'a>(exp: &Experimental, device: Option<&'a Keyboard>) -> Result<&'a Keyboard, String> {
    for feature in [Feature::KeyboardMacros, Feature::KeyboardOnboard] {
        if let Some(why) = exp.refusal(feature) {
            return Err(why);
        }
    }
    device.ok_or_else(|| "the keyboard isn't connected".to_owned())
}

/// The library as the slot holds macros: each bound key with its macro.
fn slot_macros(library: &MacroLibrary) -> Vec<SlotMacro> {
    library
        .bindings
        .iter()
        .filter_map(|b| {
            let m = library.find(&b.name)?;
            Some(SlotMacro {
                key: b.code,
                mode: match b.mode {
                    MacroMode::Once => Mode::Once,
                    MacroMode::WhileHeld => Mode::WhileHeld,
                },
                events: m.events.iter().map(slot_event).collect(),
            })
        })
        .collect()
}

fn slot_event(e: &MacroEvent) -> SlotEvent {
    match *e {
        MacroEvent::Key { code, down } => SlotEvent::Key { usage: code, down },
        MacroEvent::Button { button, down } => SlotEvent::Button { button, down },
        MacroEvent::Delay { ms } => SlotEvent::Delay { ms },
    }
}

/// Write the library into `slot`: busy, and announced, from start to end. Returns the
/// answer and whether the keyboard loaded the slot again.
fn write(
    store: &mut Store,
    kb: &Keyboard,
    slot: u8,
    active: Option<u8>,
    event_tx: &broadcast::Sender<Event>,
) -> (Result<MacrosState, String>, bool) {
    store.state.busy = true;
    store.state.error = None;
    store.state.note = None;
    announce(store, event_tx);
    let result = flash(store, kb, slot, active);
    store.state.busy = false;
    store.state.error = result.as_ref().err().cloned();
    announce(store, event_tx);
    match result {
        Ok(loaded) => (Ok(store.state.clone()), loaded),
        Err(e) => (Err(e), false),
    }
}

/// The checked slot write, then the load if it's the slot the keyboard runs.
fn flash(store: &mut Store, kb: &Keyboard, slot: u8, active: Option<u8>) -> Result<bool, String> {
    check(&store.state.library)?;
    let macros = slot_macros(&store.state.library);
    let n = u16::from(slot) + 1;
    let dir = store.config_dir.join(BACKUPS);
    let mut saved = None;
    let result = kb.write_slot(
        slot,
        &Timing::KEYBOARD,
        |old| {
            let (path, bytes) = save_backup(&dir, slot, old)?;
            saved = Some(path.display().to_string());
            Ok(bytes)
        },
        |old| slot::write(old, &macros),
    );
    if saved.is_some() {
        store.state.backup = saved;
    }
    let answer = match result {
        Ok(Written::Unchanged) => {
            info!(slot = n, "the keyboard slot already holds these macros");
            store.state.written_slot = Some(slot);
            store.save();
            return Ok(false);
        }
        Ok(Written::Written(answer)) => answer,
        Err(e) => {
            warn!(error = %e, "writing macros into a keyboard slot failed");
            let mut message = e.to_string();
            if matches!(e.restore, Restore::Failed(_)) {
                if let Some(path) = &store.state.backup {
                    message.push_str(&format!(" (its backup: {path})"));
                }
            }
            return Err(message);
        }
    };
    info!(
        slot = n,
        keys = macros.len(),
        "macros written into a keyboard slot"
    );
    store.state.written_slot = Some(slot);
    store.save();
    let mut notes: Vec<String> = answer.warning().into_iter().collect();
    let loaded = if active == Some(slot) {
        // The keyboard runs this slot: load it again so the macros take effect.
        match kb.load_slot(slot) {
            Ok(()) => true,
            Err(e) => {
                notes.push(format!(
                    "the slot couldn't be loaded again ({e:#}): load it with `ggctl onboard load {n}` for the macros to work"
                ));
                false
            }
        }
    } else {
        notes.push(format!(
            "the keyboard plays these macros once it runs slot {n}: `ggctl onboard load {n}`, or the slot's button on the keyboard"
        ));
        false
    };
    store.state.note = (!notes.is_empty()).then(|| notes.join("; "));
    Ok(loaded)
}

fn announce(store: &Store, event_tx: &broadcast::Sender<Event>) {
    let _ = event_tx.send(Event::MacrosChanged {
        state: store.state.clone(),
    });
}

/// A macros request from a client; `main` has already refused it if the keyboard part is
/// off.
pub async fn handle(req: Request, kb: &Handle) -> ResponseData {
    let answer = match req {
        Request::GetMacros => kb.send(|reply| Command::Macros(Cmd::Get(reply))).await,
        Request::SetMacros { library } => {
            kb.send(|reply| Command::Macros(Cmd::Set { library, reply }))
                .await
        }
        Request::WriteMacros { slot } => match check_slot(slot) {
            Ok(()) => {
                // Counted from here, so a write still waiting for the worker holds up the
                // daemon's stop too.
                let running = kb.slot_writes().start();
                kb.send(|reply| {
                    Command::Macros(Cmd::Write {
                        slot,
                        reply,
                        running,
                    })
                })
                .await
            }
            Err(e) => Err(e),
        },
        _ => Err("not a macros request".to_owned()),
    };
    match answer {
        Ok(state) => ResponseData::Macros { state },
        Err(message) => ResponseData::Error { message },
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use gg_device_keyboard::onboard::fake::FakeSlots;
    use gg_device_keyboard::onboard::Image;
    use gg_ipc::{Macro, MacroBinding, OnboardState};
    use std::fs;
    use std::time::Duration;

    const P: u8 = 0x13;
    const O: u8 = 0x12;

    fn gates(macros: bool, onboard: bool) -> Experimental {
        let mut e = Experimental::default();
        e.set(Feature::KeyboardMacros, macros);
        e.set(Feature::KeyboardOnboard, onboard);
        e
    }

    struct Rig {
        slots: FakeSlots,
        kb: Handle,
        events: broadcast::Receiver<Event>,
        dir: Scratch,
    }

    /// A worker on a fake keyboard whose slot 5 is like a factory one.
    fn rig(tag: &str, exp: Experimental) -> Rig {
        let dir = Scratch::new(tag);
        let slots = FakeSlots::new();
        slots.set_slot(4, slot::factory_like("Config 5").bytes());
        let (event_tx, events) = broadcast::channel(64);
        let kb = super::super::spawn_fake(slots.keyboard(), dir.0.clone(), event_tx, exp);
        Rig {
            slots,
            kb,
            events,
            dir,
        }
    }

    async fn ask(rig: &Rig, req: Request) -> Result<MacrosState, String> {
        match handle(req, &rig.kb).await {
            ResponseData::Macros { state } => Ok(state),
            ResponseData::Error { message } => Err(message),
            other => panic!("{other:?}"),
        }
    }

    /// The slot commands that reached the keyboard.
    fn slot_ops(slots: &FakeSlots) -> Vec<u8> {
        slots
            .wire()
            .sent()
            .iter()
            .map(|s| s.report[1])
            .filter(|op| [0x02, 0x03, 0x83, 0xB1, 0xB2].contains(op))
            .collect()
    }

    fn steps(words: &str) -> Vec<MacroEvent> {
        Macro::parse_steps(words).unwrap()
    }

    fn library(bound: &[(u8, MacroMode)]) -> MacroLibrary {
        MacroLibrary {
            macros: vec![Macro {
                name: "first".into(),
                events: steps("down:a wait:15 up:a wait:313 b wait:3187 click:1"),
            }],
            bindings: bound
                .iter()
                .map(|&(code, mode)| MacroBinding {
                    code,
                    name: "first".into(),
                    mode,
                })
                .collect(),
        }
    }

    fn set(library: MacroLibrary) -> Request {
        Request::SetMacros { library }
    }

    fn busy_events(rig: &mut Rig) -> Vec<bool> {
        let mut seen = Vec::new();
        while let Ok(event) = rig.events.try_recv() {
            if let Event::MacrosChanged { state } = event {
                seen.push(state.busy);
            }
        }
        seen
    }

    #[tokio::test]
    async fn with_a_gate_off_it_is_refused_in_that_gates_words_and_nothing_is_sent() {
        let macros_off = Experimental::default()
            .refusal(Feature::KeyboardMacros)
            .unwrap();
        let onboard_off = Experimental::default()
            .refusal(Feature::KeyboardOnboard)
            .unwrap();
        let rig1 = rig("macros-off", gates(false, true));
        let lib = library(&[(P, MacroMode::Once)]);
        assert_eq!(ask(&rig1, set(lib.clone())).await.unwrap_err(), macros_off);
        assert_eq!(
            ask(&rig1, Request::WriteMacros { slot: 4 })
                .await
                .unwrap_err(),
            macros_off
        );
        // Reading what's kept needs no gate.
        assert_eq!(
            ask(&rig1, Request::GetMacros).await.unwrap(),
            MacrosState::default()
        );
        let rig2 = rig("macros-no-onboard", gates(true, false));
        ask(&rig2, set(lib)).await.unwrap();
        assert_eq!(
            ask(&rig2, Request::WriteMacros { slot: 4 })
                .await
                .unwrap_err(),
            onboard_off
        );
        for r in [&rig1, &rig2] {
            assert!(slot_ops(&r.slots).is_empty());
            assert!(!r.dir.0.join(BACKUPS).exists());
        }
        assert!(!rig1.dir.0.join("macros.json").exists(), "nothing saved");
    }

    #[tokio::test]
    async fn a_library_is_checked_and_saved_and_nothing_is_sent() {
        let mut rig = rig("macros-set", gates(true, false));
        let lib = library(&[(P, MacroMode::WhileHeld)]);
        let state = ask(&rig, set(lib.clone())).await.unwrap();
        assert_eq!(state.library, lib);
        assert_eq!(busy_events(&mut rig), [false], "announced");
        assert_eq!(Store::load(&rig.dir.0).state.library, lib, "saved");
        // Refused with the reason: a key with no place for a macro, a bad step.
        let mut no_place = library(&[(0x46, MacroMode::Once)]);
        let e = ask(&rig, set(no_place.clone())).await.unwrap_err();
        assert!(
            e.contains("printscreen") && e.contains("can't play a macro"),
            "{e}"
        );
        no_place.bindings.clear();
        no_place.macros[0].events.push(MacroEvent::Delay { ms: 0 });
        let e = ask(&rig, set(no_place)).await.unwrap_err();
        assert!(e.contains("1 to 65535"), "{e}");
        assert_eq!(Store::load(&rig.dir.0).state.library, lib, "kept as it was");
        assert!(slot_ops(&rig.slots).is_empty());
    }

    #[tokio::test]
    async fn a_write_puts_the_macros_into_the_slot_after_a_backup() {
        let mut rig = rig("macros-write", gates(true, true));
        ask(&rig, set(library(&[(P, MacroMode::Once)])))
            .await
            .unwrap();
        busy_events(&mut rig);
        let old = rig.slots.slot(4);
        let state = ask(&rig, Request::WriteMacros { slot: 4 }).await.unwrap();
        assert!(state.error.is_none() && !state.busy, "{state:?}");
        assert_eq!(state.written_slot, Some(4));
        let new = Image::parse(&rig.slots.slot(4)).unwrap();
        let read = slot::read(&new).unwrap();
        assert_eq!(read.len(), 1);
        assert_eq!((read[0].key, read[0].mode), (P, Mode::Once));
        assert_eq!(read[0].events.len(), 9);
        // The backup holds the slot as it was, in the onboard slots' folder.
        let backup = PathBuf::from(state.backup.clone().unwrap());
        assert_eq!(backup.parent(), Some(rig.dir.0.join(BACKUPS).as_path()));
        assert_eq!(fs::read(&backup).unwrap(), old);
        // One erase, 25 chunks, the keyboard's check; no load, as the slot it runs isn't
        // known: the note says how to load it.
        let ops = slot_ops(&rig.slots);
        assert_eq!(ops.iter().filter(|&&op| op == 0x02).count(), 1);
        assert_eq!(ops.iter().filter(|&&op| op == 0x03).count(), 25);
        assert!(!ops.contains(&0xB2));
        let note = state.note.unwrap_or_default();
        assert!(note.contains("ggctl onboard load 5"), "{note}");
        assert_eq!(busy_events(&mut rig), [true, false]);
        assert_eq!(Store::load(&rig.dir.0).state.written_slot, Some(4));
        // The same again changes nothing and sends no erase.
        rig.slots.wire().clear_sent();
        let state = ask(&rig, Request::WriteMacros { slot: 4 }).await.unwrap();
        assert!(state.error.is_none(), "{state:?}");
        assert!(!slot_ops(&rig.slots).contains(&0x02));
    }

    #[tokio::test]
    async fn writing_the_slot_the_keyboard_runs_loads_it_again() {
        let rig = rig("macros-running", gates(true, true));
        // The first command lets the worker finish opening the keyboard (its `on_connect`
        // forgets which slot runs); then slot 5 is loaded.
        let both = library(&[(O, MacroMode::Once), (P, MacroMode::WhileHeld)]);
        ask(&rig, set(both)).await.unwrap();
        let loaded = super::super::onboard::serve(Request::LoadOnboard { slot: 4 }, &rig.kb).await;
        assert!(
            matches!(
                loaded,
                ResponseData::Onboard {
                    state: OnboardState {
                        active: Some(4),
                        ..
                    }
                }
            ),
            "{loaded:?}"
        );
        rig.slots.wire().clear_sent();
        let state = ask(&rig, Request::WriteMacros { slot: 4 }).await.unwrap();
        assert!(state.error.is_none(), "{state:?}");
        let ops = slot_ops(&rig.slots);
        assert_eq!(ops.last(), Some(&0xB2), "{ops:x?}");
        assert_eq!(rig.slots.loaded(), Some(4));
        assert_eq!(state.note, None);
    }

    #[tokio::test]
    async fn a_slot_with_macros_gglinux_cant_read_is_left_as_it_is() {
        let rig = rig("macros-unknown", gates(true, true));
        ask(&rig, set(library(&[(P, MacroMode::Once)])))
            .await
            .unwrap();
        // Slot 1 is a made-up image whose macro area is a byte pattern.
        let before = rig.slots.slot(0);
        let e = ask(&rig, Request::WriteMacros { slot: 0 })
            .await
            .unwrap_err();
        assert!(e.contains(slot::CANT_READ), "{e}");
        assert!(!slot_ops(&rig.slots).contains(&0x02), "never erased");
        assert_eq!(rig.slots.slot(0), before);
        let state = ask(&rig, Request::GetMacros).await.unwrap();
        assert_eq!(state.error.as_deref(), Some(e.as_str()));
        assert_eq!(state.written_slot, None);
    }

    #[tokio::test]
    async fn a_slot_number_past_five_is_refused_before_the_worker() {
        let rig = rig("macros-slot", gates(true, true));
        let e = ask(&rig, Request::WriteMacros { slot: 5 })
            .await
            .unwrap_err();
        assert!(e.contains("slots 1 to 5"), "{e}");
        assert!(slot_ops(&rig.slots).is_empty());
    }

    #[tokio::test]
    async fn a_write_counts_as_running_from_its_request_to_its_end() {
        let rig = rig("macros-counted", gates(true, true));
        ask(&rig, set(library(&[(P, MacroMode::Once)])))
            .await
            .unwrap();
        let writes = rig.kb.slot_writes().clone();
        let kb = rig.kb.clone();
        let write =
            tokio::spawn(async move { handle(Request::WriteMacros { slot: 4 }, &kb).await });
        let start = std::time::Instant::now();
        while !writes.running() {
            assert!(start.elapsed() < Duration::from_secs(5), "never counted");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        assert!(matches!(write.await.unwrap(), ResponseData::Macros { .. }));
        assert!(!writes.running());
    }

    #[test]
    fn without_a_keyboard_a_write_is_refused() {
        let dir = Scratch::new("macros-away");
        let mut store = Store::load(&dir.0);
        let (event_tx, _events) = broadcast::channel(8);
        let (reply, mut answer) = tokio::sync::oneshot::channel();
        let writes = super::super::onboard::Writes::default();
        let cmd = Cmd::Write {
            slot: 4,
            reply,
            running: writes.start(),
        };
        let loaded = on_command(cmd, &mut store, None, &gates(true, true), &event_tx, None);
        assert!(!loaded);
        let e = answer.try_recv().unwrap().unwrap_err();
        assert!(e.contains("isn't connected"), "{e}");
        assert!(!writes.running());
    }

    #[test]
    fn a_missing_or_unreadable_file_is_no_macros() {
        let dir = Scratch::new("macros-file");
        assert_eq!(Store::load(&dir.0).state, MacrosState::default());
        fs::write(dir.0.join("macros.json"), "not json").unwrap();
        assert_eq!(Store::load(&dir.0).state, MacrosState::default());
    }
}
