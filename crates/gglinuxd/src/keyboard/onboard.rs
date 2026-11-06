// SPDX-License-Identifier: GPL-3.0-or-later
//! The keyboard's onboard profile slots: the worker's part ([`Store`], [`on_command`],
//! [`on_connect`]) and the clients' requests ([`serve`]).
//!
//! Every operation is refused while `keyboard_onboard` is off, with
//! [`gg_ipc::Experimental::refusal`]'s words; the worker also keeps the open keyboard's
//! command families in step with the gates, so the allowlist refuses the reports
//! themselves too. An operation runs on the worker's thread: while it does, nothing else
//! reaches the keyboard, so the lighting stream pauses around a write as the protocol asks.
//! The state says `busy` meanwhile, and each start and end is announced
//! ([`Event::OnboardChanged`]).
//!
//! - **Read** reads a slot (or all five), checks it, and saves it to
//!   `<config>/onboard-backups/slot<N>-<UTC>.bin` (N from 1, as the user counts; the folder
//!   is mode 700, the files 600).
//! - **Load** makes the keyboard run a slot (`B2`).
//! - **Write** is [`Keyboard::write_slot`]'s checked transaction, with a backup saved and
//!   read back first. `Current` puts in what gglinux sets that a slot can hold at known
//!   places: the Rapid Tap pairs (if gglinux sets the switches) and the screen picture (text
//!   or a picture). Everything else stays as the slot has it: the per-key actuation points,
//!   Rapid Trigger and its distance are stored by the firmware's own key order, which isn't
//!   published, and the key bindings aren't the worker's to read. `Backup` writes a backup
//!   file, checked before anything is sent.
//!
//! The reports are framed in [`gg_device_keyboard::onboard`].

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use gg_device_keyboard::onboard::{Image, Timing, SLOTS};
use gg_device_keyboard::{oled, Bitmap, Keyboard};
use gg_ipc::{
    check_slot, Event, Experimental, Feature, OnboardSlot, OnboardSource, OnboardState, Request,
    ResponseData, Screen, Switches, TapMode,
};
use tokio::sync::broadcast;
use tracing::{info, warn};

use super::screen::ScreenState;
use super::switches::SwitchesState;
use super::{Command, Handle, Reply};

/// What the keyboard worker keeps for the onboard slots.
pub struct Store {
    state: OnboardState,
    config_dir: PathBuf,
}

impl Store {
    /// The slots as nothing has read them yet: none listed, unless backups of them are
    /// found in `config_dir` (the newest of each is kept).
    pub fn load(config_dir: &Path) -> Store {
        let dir = backups(config_dir);
        let mut store = Store {
            state: OnboardState::default(),
            config_dir: config_dir.to_owned(),
        };
        for index in 0..SLOTS {
            if let Some(path) = newest_backup(&dir, index) {
                store.slot(index).backup = Some(path.display().to_string());
            }
        }
        store
    }

    /// A slot's entry. None are listed until something is known about one; then all five.
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
}

/// The keyboard worker's commands for the onboard slots. Each answers with the state after
/// it.
pub enum Cmd {
    Get(Reply<OnboardState>),
    /// Read and back up one slot, or all five.
    Read {
        slot: Option<u8>,
        reply: Reply<OnboardState>,
    },
    Load {
        slot: u8,
        reply: Reply<OnboardState>,
    },
    Write {
        slot: u8,
        from: OnboardSource,
        reply: Reply<OnboardState>,
    },
}

/// One command, on the keyboard worker's thread. `device` is the open keyboard, if any.
pub fn on_command(
    cmd: Cmd,
    store: &mut Store,
    device: Option<&Keyboard>,
    exp: &Experimental,
    event_tx: &broadcast::Sender<Event>,
) {
    let (reply, answer) = match cmd {
        Cmd::Get(reply) => (reply, Ok(store.state.clone())),
        Cmd::Read { slot, reply } => {
            let answer = run(store, device, exp, event_tx, |s, kb| read(s, kb, slot));
            (reply, answer)
        }
        Cmd::Load { slot, reply } => {
            let answer = run(store, device, exp, event_tx, |s, kb| load(s, kb, slot));
            (reply, answer)
        }
        Cmd::Write { slot, from, reply } => {
            let answer = run(store, device, exp, event_tx, |s, kb| {
                write(s, kb, slot, from)
            });
            (reply, answer)
        }
    };
    let _ = reply.send(answer);
}

/// The keyboard just opened: which slot it runs isn't known any more.
pub fn on_connect(
    store: &mut Store,
    _device: &Keyboard,
    _exp: &Experimental,
    _event_tx: &broadcast::Sender<Event>,
) {
    store.state.active = None;
}

/// An onboard request from a client; `main` has already refused it if the keyboard part
/// is off.
pub async fn serve(req: Request, kb: &Handle) -> ResponseData {
    let answer = match req {
        Request::GetOnboard => kb.send(|reply| Command::Onboard(Cmd::Get(reply))).await,
        Request::ReadOnboard { slot } => match slot.map(check_slot).transpose() {
            Ok(_) => {
                kb.send(|reply| Command::Onboard(Cmd::Read { slot, reply }))
                    .await
            }
            Err(e) => Err(e),
        },
        Request::LoadOnboard { slot } => match check_slot(slot) {
            Ok(()) => {
                kb.send(|reply| Command::Onboard(Cmd::Load { slot, reply }))
                    .await
            }
            Err(e) => Err(e),
        },
        Request::WriteOnboard { slot, from } => match check_slot(slot) {
            Ok(()) => {
                kb.send(|reply| Command::Onboard(Cmd::Write { slot, from, reply }))
                    .await
            }
            Err(e) => Err(e),
        },
        _ => Err("not an onboard slot request".to_owned()),
    };
    match answer {
        Ok(state) => ResponseData::Onboard { state },
        Err(message) => ResponseData::Error { message },
    }
}

/// Run one operation: refused while the gate is off or with no keyboard; `busy`, and
/// announced, from its start to its end; its error kept in the state.
fn run(
    store: &mut Store,
    device: Option<&Keyboard>,
    exp: &Experimental,
    event_tx: &broadcast::Sender<Event>,
    op: impl FnOnce(&mut Store, &Keyboard) -> Result<(), String>,
) -> Result<OnboardState, String> {
    if let Some(why) = exp.refusal(Feature::KeyboardOnboard) {
        return Err(why);
    }
    let Some(kb) = device else {
        return Err("the keyboard isn't connected".to_owned());
    };
    store.state.busy = true;
    store.state.error = None;
    announce(store, event_tx);
    let result = op(store, kb);
    store.state.busy = false;
    store.state.error = result.as_ref().err().cloned();
    announce(store, event_tx);
    result.map(|()| store.state.clone())
}

fn announce(store: &Store, event_tx: &broadcast::Sender<Event>) {
    let _ = event_tx.send(Event::OnboardChanged {
        state: store.state.clone(),
    });
}

fn read(store: &mut Store, kb: &Keyboard, slot: Option<u8>) -> Result<(), String> {
    let slots: Vec<u8> = slot.map_or_else(|| (0..SLOTS).collect(), |s| vec![s]);
    let dir = backups(&store.config_dir);
    let mut errors = Vec::new();
    for s in slots {
        let n = u16::from(s) + 1;
        let bytes = match kb.read_slot(s) {
            Ok(bytes) => bytes,
            Err(e) => {
                errors.push(format!("slot {n} couldn't be read: {e:#}"));
                continue;
            }
        };
        let image = match Image::parse(&bytes) {
            Ok(image) => image,
            Err(e) => {
                let slot = store.slot(s);
                slot.valid = Some(false);
                slot.name = None;
                errors.push(format!(
                    "slot {n} doesn't hold an image gglinux knows ({e:#}), so it wasn't backed up and won't be written"
                ));
                continue;
            }
        };
        let slot = store.slot(s);
        slot.valid = Some(true);
        slot.name = Some(image.name());
        match save_backup(&dir, s, &image) {
            Ok((path, _)) => {
                info!(slot = n, path = %path.display(), "keyboard slot backed up");
                slot.backup = Some(path.display().to_string());
            }
            Err(e) => errors.push(format!("slot {n}'s backup couldn't be saved: {e:#}")),
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(errors.join("; "))
    }
}

fn load(store: &mut Store, kb: &Keyboard, slot: u8) -> Result<(), String> {
    kb.load_slot(slot).map_err(|e| format!("{e:#}"))?;
    store.state.active = Some(slot);
    Ok(())
}

/// How a write makes the new image from the slot's.
type Change = Box<dyn FnOnce(&Image) -> anyhow::Result<Image>>;

fn write(store: &mut Store, kb: &Keyboard, slot: u8, from: OnboardSource) -> Result<(), String> {
    let change: Change = match from {
        OnboardSource::Current => {
            let switches = SwitchesState::load(&store.config_dir).settings().clone();
            let screen = ScreenState::load(&store.config_dir).setting().clone();
            Box::new(move |old| with_settings(old, &switches, &screen))
        }
        OnboardSource::Backup { path } => {
            let image = read_backup(Path::new(&path)).map_err(|e| format!("{e:#}"))?;
            Box::new(move |_| Ok(image))
        }
    };
    let dir = backups(&store.config_dir);
    let mut saved = None;
    let mut made = None;
    let result = kb.write_slot(
        slot,
        &Timing::KEYBOARD,
        |old| {
            let (path, bytes) = save_backup(&dir, slot, old)?;
            saved = Some(path);
            Ok(bytes)
        },
        |old| {
            let new = change(old)?;
            made = Some(new.name());
            Ok(new)
        },
    );
    let s = store.slot(slot);
    if let Some(path) = saved {
        s.backup = Some(path.display().to_string());
    }
    match result {
        Ok(written) => {
            info!(slot = slot + 1, ?written, "keyboard slot written");
            s.valid = Some(true);
            s.name = made;
            Ok(())
        }
        Err(e) => {
            warn!(error = %e, "keyboard slot write failed");
            Err(e.to_string())
        }
    }
}

/// `old` with gglinux's settings in, where a slot holds them at a known place.
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
                mode: device_tap_mode(p.mode),
                report_both: p.report_both,
            })
            .collect();
        new.set_rapid_tap(switches.rapid_tap.enabled, &pairs)
            .context("gglinux's Rapid Tap pairs")?;
    }
    if let Some(picture) = still_picture(screen) {
        new.set_oled(&picture);
    }
    Ok(new)
}

/// The picture a screen setting shows for good: text or a picture. A clock or anything
/// that moves would stand still in flash, so the slot keeps its own.
fn still_picture(screen: &Screen) -> Option<Bitmap> {
    match screen {
        Screen::Text { text } => Some(oled::text(text)),
        Screen::Image { pixels } => Bitmap::from_rows(pixels).ok(),
        _ => None,
    }
}

fn device_tap_mode(mode: TapMode) -> gg_device_keyboard::TapMode {
    use gg_device_keyboard::TapMode as D;
    match mode {
        TapMode::LastWins => D::LastWins,
        TapMode::Neither => D::Neither,
        TapMode::FirstWins => D::FirstWins,
        TapMode::SecondWins => D::SecondWins,
    }
}

/// Where the slots' backups go.
fn backups(config_dir: &Path) -> PathBuf {
    config_dir.join("onboard-backups")
}

/// A backup file, checked as a slot read is.
fn read_backup(path: &Path) -> anyhow::Result<Image> {
    let bytes = fs::read(path).with_context(|| format!("reading {}", path.display()))?;
    Image::parse(&bytes)
        .with_context(|| format!("{} isn't a slot backup gglinux can write", path.display()))
}

/// Save `image` as a new backup of `slot` in `dir`, and read the file back: its path and
/// what it holds.
fn save_backup(dir: &Path, slot: u8, image: &Image) -> anyhow::Result<(PathBuf, Vec<u8>)> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("making {}", dir.display()))?;
    // Made earlier by something else, perhaps with a looser mode.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let stamp = utc_stamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let n = u16::from(slot) + 1;
    let mut path = dir.join(format!("slot{n}-{stamp}.bin"));
    let mut tries = 1;
    let mut file = loop {
        match fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .mode(0o600)
            .open(&path)
        {
            Ok(file) => break file,
            // Two in one second.
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists && tries < 100 => {
                tries += 1;
                path = dir.join(format!("slot{n}-{stamp}-{tries}.bin"));
            }
            Err(e) => return Err(e).with_context(|| format!("making {}", path.display())),
        }
    };
    file.write_all(image.bytes())?;
    file.sync_all()?;
    drop(file);
    let back = fs::read(&path).with_context(|| format!("reading {} back", path.display()))?;
    Ok((path, back))
}

/// The newest backup of `slot` in `dir`, by when it was written.
fn newest_backup(dir: &Path, slot: u8) -> Option<PathBuf> {
    let prefix = format!("slot{}-", u16::from(slot) + 1);
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(".bin")
        })
        .filter_map(|e| Some((e.metadata().ok()?.modified().ok()?, e.path())))
        .max()
        .map(|(_, path)| path)
}

/// `YYYYMMDDTHHMMSSZ` for a time in seconds since the Unix epoch.
fn utc_stamp(secs: u64) -> String {
    // Days to a civil date (Howard Hinnant's algorithm).
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400 + i64::from(m <= 2);
    format!(
        "{y:04}{m:02}{d:02}T{:02}{:02}{:02}Z",
        rem / 3_600,
        rem % 3_600 / 60,
        rem % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use gg_device_keyboard::onboard::fake::FakeSlots;
    use gg_device_keyboard::onboard::{at, IMAGE_BYTES};
    use gg_ipc::{RapidTap, TapPair};

    fn gate(on: bool) -> Experimental {
        let mut e = Experimental::default();
        e.set(Feature::KeyboardOnboard, on);
        e
    }

    struct Rig {
        slots: FakeSlots,
        kb: Handle,
        events: broadcast::Receiver<Event>,
        dir: Scratch,
    }

    fn rig(tag: &str, on: bool, prepare: impl FnOnce(&Path)) -> Rig {
        let dir = Scratch::new(tag);
        prepare(&dir.0);
        let slots = FakeSlots::new();
        let (event_tx, events) = broadcast::channel(64);
        let kb = super::super::spawn_fake(slots.keyboard(), dir.0.clone(), event_tx, gate(on));
        Rig {
            slots,
            kb,
            events,
            dir,
        }
    }

    async fn ask(rig: &Rig, req: Request) -> Result<OnboardState, String> {
        match serve(req, &rig.kb).await {
            ResponseData::Onboard { state } => Ok(state),
            ResponseData::Error { message } => Err(message),
            other => panic!("{other:?}"),
        }
    }

    /// The slot commands that reached the keyboard (not the lighting or the screen).
    fn slot_ops(slots: &FakeSlots) -> Vec<u8> {
        slots
            .wire()
            .sent()
            .iter()
            .map(|s| s.report[1])
            .filter(|op| [0x02, 0x03, 0x83, 0xB1, 0xB2].contains(op))
            .collect()
    }

    fn mode(path: &Path) -> u32 {
        fs::metadata(path).unwrap().permissions().mode() & 0o777
    }

    #[tokio::test]
    async fn with_the_gate_off_every_operation_is_refused_and_nothing_is_sent() {
        let rig = rig("onboard-off", false, |_| {});
        let refusal = Experimental::default()
            .refusal(Feature::KeyboardOnboard)
            .unwrap();
        for req in [
            Request::ReadOnboard { slot: None },
            Request::LoadOnboard { slot: 0 },
            Request::WriteOnboard {
                slot: 4,
                from: OnboardSource::Current,
            },
        ] {
            assert_eq!(ask(&rig, req).await.unwrap_err(), refusal);
        }
        // Nothing known, nothing busy: as on a fresh install.
        let state = ask(&rig, Request::GetOnboard).await.unwrap();
        assert_eq!(state, OnboardState::default());
        assert!(slot_ops(&rig.slots).is_empty());
        assert!(!rig.dir.0.join("onboard-backups").exists());
    }

    #[tokio::test]
    async fn a_slot_number_past_five_is_refused_before_the_worker() {
        let rig = rig("onboard-slot5", true, |_| {});
        for req in [
            Request::ReadOnboard { slot: Some(5) },
            Request::LoadOnboard { slot: 5 },
            Request::WriteOnboard {
                slot: 9,
                from: OnboardSource::Current,
            },
        ] {
            assert!(ask(&rig, req).await.unwrap_err().contains("slots 1 to 5"));
        }
        assert!(slot_ops(&rig.slots).is_empty());
    }

    #[tokio::test]
    async fn reading_all_five_backs_each_up_privately() {
        let mut rig = rig("onboard-read", true, |_| {});
        let state = ask(&rig, Request::ReadOnboard { slot: None })
            .await
            .unwrap();
        assert!(!state.busy && state.error.is_none(), "{state:?}");
        let dir = rig.dir.0.join("onboard-backups");
        assert_eq!(mode(&dir), 0o700);
        for (i, slot) in state.slots.iter().enumerate() {
            assert_eq!(slot.index as usize, i);
            assert_eq!(
                slot.name.as_deref(),
                Some(format!("Config {}", i + 1).as_str())
            );
            assert_eq!(slot.valid, Some(true));
            let path = PathBuf::from(slot.backup.as_ref().unwrap());
            assert_eq!(path.parent(), Some(dir.as_path()));
            let name = path.file_name().unwrap().to_string_lossy().into_owned();
            assert!(
                name.starts_with(&format!("slot{}-20", i + 1)) && name.ends_with("Z.bin"),
                "{name}"
            );
            assert_eq!(mode(&path), 0o600);
            assert_eq!(fs::read(&path).unwrap(), rig.slots.slot(i as u8));
        }
        assert_eq!(slot_ops(&rig.slots), vec![0x83; 125], "reads only");
        // Busy from the start to the end, announced both times.
        let mut seen = Vec::new();
        while let Ok(event) = rig.events.try_recv() {
            if let Event::OnboardChanged { state } = event {
                seen.push(state.busy);
            }
        }
        assert_eq!(seen, [true, false]);
        // A worker started later finds the backups.
        let again = Store::load(&rig.dir.0);
        assert_eq!(again.state.slots[2].backup, state.slots[2].backup);
    }

    #[tokio::test]
    async fn a_slot_that_reads_bad_is_marked_and_not_backed_up() {
        let rig = rig("onboard-bad", true, |_| {});
        let mut bad = rig.slots.slot(1);
        bad[300] ^= 0xFF;
        rig.slots.set_slot(1, &bad);
        let e = ask(&rig, Request::ReadOnboard { slot: Some(1) })
            .await
            .unwrap_err();
        assert!(e.contains("slot 2") && e.contains("checksum"), "{e}");
        let state = ask(&rig, Request::GetOnboard).await.unwrap();
        assert_eq!(state.slots[1].valid, Some(false));
        assert_eq!(state.slots[1].backup, None);
        assert_eq!(state.error.as_deref(), Some(e.as_str()));
        // And a write to it stops before the erase.
        let e = ask(
            &rig,
            Request::WriteOnboard {
                slot: 1,
                from: OnboardSource::Current,
            },
        )
        .await
        .unwrap_err();
        assert!(e.contains("wasn't changed"), "{e}");
        assert!(!slot_ops(&rig.slots).contains(&0x02));
        assert_eq!(rig.slots.slot(1), bad);
    }

    #[tokio::test]
    async fn writing_gglinuxs_settings_puts_in_the_picture_and_rapid_tap_and_keeps_the_rest() {
        let switches = Switches {
            enabled: true,
            rapid_tap: RapidTap {
                enabled: true,
                pairs: vec![TapPair {
                    a: 0x04,
                    b: 0x07,
                    mode: TapMode::Neither,
                    report_both: true,
                }],
            },
            ..Switches::default()
        };
        let screen = Screen::Text {
            text: "gglinux".into(),
        };
        let rig = rig("onboard-current", true, |dir| {
            fs::write(
                dir.join("switches.json"),
                serde_json::to_vec(&switches).unwrap(),
            )
            .unwrap();
            fs::write(
                dir.join("screen.json"),
                serde_json::to_vec(&screen).unwrap(),
            )
            .unwrap();
        });
        let old = rig.slots.slot(4);
        let state = ask(
            &rig,
            Request::WriteOnboard {
                slot: 4,
                from: OnboardSource::Current,
            },
        )
        .await
        .unwrap();
        assert!(state.error.is_none() && !state.busy, "{state:?}");
        let new = rig.slots.slot(4);
        let image = Image::parse(&new).unwrap();
        assert_eq!(image.oled(), &oled::text("gglinux").packed()[..]);
        assert_eq!(
            &new[at::RAPID_TAP..at::RAPID_TAP + 5],
            &[0x04, 0x07, 0x81, 0, 0]
        );
        assert_eq!(new[at::RAPID_TAP_ENABLE], 1);
        // Everything else as it was: names, switch tables, macros, GUID.
        for (i, (a, b)) in old.iter().zip(&new).enumerate().take(at::CRC) {
            let ours = (at::OLED..at::RAPID_TAP_ENABLE + 1).contains(&i);
            if !ours {
                assert_eq!(a, b, "byte {i}");
            }
        }
        // The backup holds the slot as it was, and is the newest.
        let backup = PathBuf::from(state.slots[4].backup.as_ref().unwrap());
        assert_eq!(fs::read(&backup).unwrap(), old);
        assert_eq!(state.slots[4].name.as_deref(), Some("Config 5"));
        assert_eq!(state.slots[4].valid, Some(true));
        let o = slot_ops(&rig.slots);
        assert_eq!(o.iter().filter(|&&op| op == 0x02).count(), 1);
        assert_eq!(o.iter().filter(|&&op| op == 0x03).count(), 25);
        assert_eq!(o.iter().filter(|&&op| op == 0xB1).count(), 1);
    }

    #[tokio::test]
    async fn a_backup_is_written_back_only_if_it_checks_out() {
        let rig = rig("onboard-restore", true, |_| {});
        let state = ask(&rig, Request::ReadOnboard { slot: Some(0) })
            .await
            .unwrap();
        let backup = state.slots[0].backup.clone().unwrap();
        // Something else changes the slot; the backup brings it back.
        let mut other = Image::parse(&rig.slots.slot(0)).unwrap();
        other.set_name("Someone else").unwrap();
        rig.slots.set_slot(0, other.bytes());
        let state = ask(
            &rig,
            Request::WriteOnboard {
                slot: 0,
                from: OnboardSource::Backup {
                    path: backup.clone(),
                },
            },
        )
        .await
        .unwrap();
        assert_eq!(state.slots[0].name.as_deref(), Some("Config 1"));
        assert_eq!(rig.slots.slot(0), fs::read(&backup).unwrap());
        // A second backup was made first, of what the slot held.
        let newest = state.slots[0].backup.clone().unwrap();
        assert_ne!(newest, backup);
        assert_eq!(fs::read(&newest).unwrap(), other.serialize());

        // A damaged or missing file is refused before anything is sent.
        let damaged = rig.dir.0.join("damaged.bin");
        let mut bytes = fs::read(&backup).unwrap();
        bytes[5000] ^= 1;
        fs::write(&damaged, &bytes).unwrap();
        let short = rig.dir.0.join("short.bin");
        fs::write(&short, &bytes[..IMAGE_BYTES - 4]).unwrap();
        rig.slots.wire().clear_sent();
        for path in [damaged, short, rig.dir.0.join("missing.bin")] {
            let e = ask(
                &rig,
                Request::WriteOnboard {
                    slot: 0,
                    from: OnboardSource::Backup {
                        path: path.display().to_string(),
                    },
                },
            )
            .await
            .unwrap_err();
            assert!(e.contains(&path.display().to_string()), "{e}");
        }
        assert!(slot_ops(&rig.slots).is_empty());
    }

    #[tokio::test]
    async fn loading_a_slot_sends_b2_and_remembers_it() {
        let rig = rig("onboard-load", true, |_| {});
        let state = ask(&rig, Request::LoadOnboard { slot: 2 }).await.unwrap();
        assert_eq!(state.active, Some(2));
        assert_eq!(rig.slots.loaded(), Some(2));
        assert_eq!(slot_ops(&rig.slots), [0xB2]);
    }

    #[test]
    fn backup_names_carry_the_utc_time() {
        assert_eq!(utc_stamp(0), "19700101T000000Z");
        assert_eq!(utc_stamp(951_782_400), "20000229T000000Z");
        assert_eq!(utc_stamp(1_793_975_696), "20261106T143456Z");
        assert_eq!(utc_stamp(4_102_444_799), "20991231T235959Z");
    }

    #[test]
    fn two_backups_in_one_second_both_stay() {
        let dir = Scratch::new("onboard-twice");
        let backups = dir.0.join("onboard-backups");
        // A folder made too open by something else is closed up.
        fs::create_dir(&backups).unwrap();
        fs::set_permissions(&backups, fs::Permissions::from_mode(0o755)).unwrap();
        let image = gg_device_keyboard::onboard::fake::synthesized("Config 1");
        let (a, bytes) = save_backup(&backups, 0, &image).unwrap();
        let (b, _) = save_backup(&backups, 0, &image).unwrap();
        assert_ne!(a, b);
        assert_eq!(bytes, image.serialize());
        assert_eq!(mode(&backups), 0o700);
        assert!(newest_backup(&backups, 0).is_some());
        assert_eq!(newest_backup(&backups, 1), None);
    }
}
