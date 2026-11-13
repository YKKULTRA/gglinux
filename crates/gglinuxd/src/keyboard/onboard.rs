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
//!   is mode 700, the files 600, each synced with its folder). A slot that doesn't check
//!   out is saved as `slot<N>-<UTC>-damaged.bin` instead, to look at later; that file is
//!   never taken for a backup.
//! - **Load** makes the keyboard run a slot (`B2`).
//! - **Write** is [`Keyboard::write_slot`]'s checked transaction, with a backup saved and
//!   read back first. `Current` puts in what gglinux sets that a slot can hold at known
//!   places: the Rapid Tap pairs (if gglinux sets the switches) and the screen picture (text
//!   or a picture). Everything else stays as the slot has it: the per-key actuation points,
//!   Rapid Trigger and its distance are stored by the firmware's own key order, which isn't
//!   published, and the key bindings aren't the worker's to read. `Backup` writes a backup
//!   file, checked before anything is sent. `Recover` puts a backup into a slot that
//!   doesn't check out any more ([`Keyboard::recover_slot`]), its bytes saved first as a
//!   `-damaged.bin`. What the keyboard's own check answered, when it isn't `B1 00`, is
//!   kept as the state's `note`.
//! - A write counts in [`Writes`] from its request to its end, so `main` waits for it
//!   before it stops or restarts the daemon (at most [`longest_write`]).
//!
//! The reports are framed in [`gg_device_keyboard::onboard`].

use std::fs;
use std::io::Write as _;
use std::os::unix::fs::{DirBuilderExt, OpenOptionsExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use anyhow::Context as _;
use gg_device_keyboard::onboard::{Image, Restore, Timing, Written, SLOTS};
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

/// Onboard slot writes asked for and not ended yet, counted from the request (so one still
/// waiting for the worker counts) to the worker's end of it. Clones share the count.
#[derive(Clone, Default)]
pub struct Writes(Arc<AtomicUsize>);

impl Writes {
    /// Whether any is asked for or running.
    pub fn running(&self) -> bool {
        self.0.load(Ordering::SeqCst) > 0
    }

    /// Count one from now until the returned token is dropped.
    pub fn start(&self) -> Running {
        self.0.fetch_add(1, Ordering::SeqCst);
        Running(Arc::clone(&self.0))
    }
}

/// One counted slot write: dropping it ends it.
pub struct Running(Arc<AtomicUsize>);

impl Drop for Running {
    fn drop(&mut self) {
        self.0.fetch_sub(1, Ordering::SeqCst);
    }
}

/// About the longest a slot write runs when every wait runs out: the most `main` waits for
/// one before it stops the daemon anyway. ggctl and the app wait a minute for theirs.
pub fn longest_write() -> Duration {
    Timing::KEYBOARD.longest_write()
}

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
        /// Counted in the handle's [`Writes`] until the write ends.
        running: Running,
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
        Cmd::Write {
            slot,
            from,
            reply,
            running,
        } => {
            let answer = run(store, device, exp, event_tx, |s, kb| {
                write(s, kb, slot, from)
            });
            drop(running);
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
                // Counted from here, so a write still waiting for the worker holds up the
                // daemon's stop too.
                let running = kb.slot_writes().start();
                kb.send(|reply| {
                    Command::Onboard(Cmd::Write {
                        slot,
                        from,
                        reply,
                        running,
                    })
                })
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
    store.state.note = None;
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
        let bytes = match kb.read_slot(s, &Timing::KEYBOARD) {
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
                let kept = match save_damaged(&dir, s, &bytes) {
                    Ok(path) => format!("its bytes were saved as {}", path.display()),
                    Err(e) => format!("its bytes couldn't be saved ({e:#})"),
                };
                errors.push(format!(
                    "slot {n} doesn't check out ({e:#}): {kept}, and nothing was written. If a \
                     write to it was cut short, put its backup back with `ggctl onboard restore \
                     {n} <backup file> --damaged`"
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
        OnboardSource::Recover { path } => return recover(store, kb, slot, &path),
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
    let backup = saved.map(|path| path.display().to_string());
    let s = store.slot(slot);
    if backup.is_some() {
        s.backup.clone_from(&backup);
    }
    match result {
        Ok(written) => {
            info!(slot = slot + 1, ?written, "keyboard slot written");
            s.valid = Some(true);
            s.name = made;
            if let Written::Written(check) = written {
                store.state.note = check.warning();
            }
            Ok(())
        }
        Err(e) => {
            warn!(error = %e, "keyboard slot write failed");
            let mut message = e.to_string();
            if matches!(e.restore, Restore::Failed(_)) {
                // It may be erased or half written now.
                s.valid = Some(false);
                if let Some(path) = backup {
                    message.push_str(&format!(" (its backup: {path})"));
                }
            }
            Err(message)
        }
    }
}

/// Put the backup at `path` into a slot that doesn't check out ([`Keyboard::recover_slot`]),
/// what the slot holds saved first as a `-damaged.bin`.
fn recover(store: &mut Store, kb: &Keyboard, slot: u8, path: &str) -> Result<(), String> {
    let image = read_backup(Path::new(path)).map_err(|e| format!("{e:#}"))?;
    let dir = backups(&store.config_dir);
    let mut kept = None;
    let result = kb.recover_slot(slot, &Timing::KEYBOARD, &image, |raw| {
        let saved = save_damaged(&dir, slot, raw)?;
        let back = fs::read(&saved).with_context(|| format!("reading {} back", saved.display()))?;
        kept = Some(saved);
        Ok(back)
    });
    let s = store.slot(slot);
    match result {
        Ok(check) => {
            info!(slot = slot + 1, from = path, damaged = ?kept, "keyboard slot put back from a backup");
            s.valid = Some(true);
            s.name = Some(image.name());
            store.state.note = check.warning();
            Ok(())
        }
        Err(e) => {
            warn!(error = %e, "putting a backup into a damaged keyboard slot failed");
            let mut message = e.to_string();
            if let Some(saved) = kept {
                message.push_str(&format!(
                    " (what the slot held is saved as {})",
                    saved.display()
                ));
            }
            Err(message)
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

/// What a damaged slot's saved bytes end with, so they're never taken for a backup.
const DAMAGED: &str = "-damaged";

/// Save `bytes` as a new file of `slot`'s in `dir` (made, or closed up, to mode 700):
/// `slot<N>-<UTC><tail>.bin` (a second one in the same second gets `-2` after the time),
/// mode 600, synced, and its folder synced so the new name lasts too. Returns its path.
fn save_new(dir: &Path, slot: u8, tail: &str, bytes: &[u8]) -> anyhow::Result<PathBuf> {
    fs::DirBuilder::new()
        .recursive(true)
        .mode(0o700)
        .create(dir)
        .with_context(|| format!("making {}", dir.display()))?;
    // Made earlier by something else, perhaps with a looser mode.
    fs::set_permissions(dir, fs::Permissions::from_mode(0o700))?;
    let stamp = utc_stamp(SystemTime::now().duration_since(UNIX_EPOCH)?.as_secs());
    let n = u16::from(slot) + 1;
    let mut path = dir.join(format!("slot{n}-{stamp}{tail}.bin"));
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
                path = dir.join(format!("slot{n}-{stamp}-{tries}{tail}.bin"));
            }
            Err(e) => return Err(e).with_context(|| format!("making {}", path.display())),
        }
    };
    file.write_all(bytes)?;
    file.sync_all()?;
    drop(file);
    fs::File::open(dir)
        .and_then(|folder| folder.sync_all())
        .with_context(|| format!("syncing {}", dir.display()))?;
    Ok(path)
}

/// Save `image` as a new backup of `slot` in `dir`, and read the file back: its path and
/// what it holds.
pub(super) fn save_backup(dir: &Path, slot: u8, image: &Image) -> anyhow::Result<(PathBuf, Vec<u8>)> {
    let path = save_new(dir, slot, "", image.bytes())?;
    let back = fs::read(&path).with_context(|| format!("reading {} back", path.display()))?;
    Ok((path, back))
}

/// Save what a slot that doesn't check out holds, as read, to look at later.
fn save_damaged(dir: &Path, slot: u8, bytes: &[u8]) -> anyhow::Result<PathBuf> {
    save_new(dir, slot, DAMAGED, bytes)
}

/// The newest backup of `slot` in `dir`, by when it was written (damaged slots' bytes
/// aren't backups).
fn newest_backup(dir: &Path, slot: u8) -> Option<PathBuf> {
    let prefix = format!("slot{}-", u16::from(slot) + 1);
    let damaged = format!("{DAMAGED}.bin");
    fs::read_dir(dir)
        .ok()?
        .filter_map(Result::ok)
        .filter(|e| {
            let name = e.file_name();
            let name = name.to_string_lossy();
            name.starts_with(&prefix) && name.ends_with(".bin") && !name.ends_with(&damaged)
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
            Request::WriteOnboard {
                slot: 4,
                from: OnboardSource::Recover {
                    path: "/nowhere/slot5.bin".into(),
                },
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

    /// The files in the backups folder whose names end with `-damaged.bin`.
    fn damaged_files(rig: &Rig) -> Vec<PathBuf> {
        let dir = rig.dir.0.join("onboard-backups");
        fs::read_dir(&dir)
            .map(|d| {
                d.map(|e| e.unwrap().path())
                    .filter(|p| p.to_string_lossy().ends_with("-damaged.bin"))
                    .collect()
            })
            .unwrap_or_default()
    }

    #[tokio::test]
    async fn a_slot_that_reads_bad_is_marked_saved_raw_and_not_backed_up() {
        let rig = rig("onboard-bad", true, |_| {});
        let mut bad = rig.slots.slot(1);
        bad[300] ^= 0xFF;
        rig.slots.set_slot(1, &bad);
        let e = ask(&rig, Request::ReadOnboard { slot: Some(1) })
            .await
            .unwrap_err();
        assert!(e.contains("slot 2") && e.contains("checksum"), "{e}");
        // Its bytes are kept to look at later, apart from the backups.
        let damaged = damaged_files(&rig);
        assert_eq!(damaged.len(), 1, "{damaged:?}");
        let name = damaged[0]
            .file_name()
            .unwrap()
            .to_string_lossy()
            .into_owned();
        assert!(name.starts_with("slot2-20"), "{name}");
        assert_eq!(fs::read(&damaged[0]).unwrap(), bad);
        assert_eq!(mode(&damaged[0]), 0o600);
        assert!(e.contains(&damaged[0].display().to_string()), "{e}");
        assert!(
            e.contains("ggctl onboard restore 2 <backup file> --damaged"),
            "{e}"
        );
        let state = ask(&rig, Request::GetOnboard).await.unwrap();
        assert_eq!(state.slots[1].valid, Some(false));
        assert_eq!(state.slots[1].backup, None, "not a backup");
        assert_eq!(state.error.as_deref(), Some(e.as_str()));
        assert!(
            Store::load(&rig.dir.0).state.slots.is_empty(),
            "nor taken for one later"
        );
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
    async fn a_slot_left_damaged_takes_its_backup_back_only_as_damaged() {
        let rig = rig("onboard-recover", true, |_| {});
        let state = ask(&rig, Request::ReadOnboard { slot: Some(4) })
            .await
            .unwrap();
        let backup = state.slots[4].backup.clone().unwrap();
        let good = rig.slots.slot(4);
        // A write cut short: the first chunks in, the rest erased.
        let mut half = good.clone();
        half[3000..].fill(0xFF);
        rig.slots.set_slot(4, &half);
        let write = |from| Request::WriteOnboard { slot: 4, from };
        let restore = |path: &str| {
            write(OnboardSource::Backup {
                path: path.to_owned(),
            })
        };
        let recover = |path: &str| {
            write(OnboardSource::Recover {
                path: path.to_owned(),
            })
        };
        let e = ask(&rig, restore(&backup)).await.unwrap_err();
        assert!(e.contains("--damaged"), "{e}");
        rig.slots.wire().clear_sent();
        let state = ask(&rig, recover(&backup)).await.unwrap();
        assert!(state.error.is_none(), "{state:?}");
        assert_eq!(rig.slots.slot(4), good);
        assert_eq!(state.slots[4].valid, Some(true));
        assert_eq!(state.slots[4].name.as_deref(), Some("Config 5"));
        assert_eq!(
            state.slots[4].backup.as_deref(),
            Some(backup.as_str()),
            "still the newest backup"
        );
        assert_eq!(
            slot_ops(&rig.slots)
                .iter()
                .filter(|&&op| op == 0x02)
                .count(),
            1
        );
        // What the slot held was saved first.
        let damaged = damaged_files(&rig);
        assert_eq!(damaged.len(), 1, "{damaged:?}");
        assert_eq!(fs::read(&damaged[0]).unwrap(), half);
        // A slot that checks out isn't recovered...
        let e = ask(&rig, recover(&backup)).await.unwrap_err();
        assert!(e.contains("without --damaged"), "{e}");
        // ... and a backup that doesn't check out is refused before anything is sent.
        let broken = rig.dir.0.join("broken.bin");
        let mut bytes = good.clone();
        bytes[100] ^= 1;
        fs::write(&broken, &bytes).unwrap();
        rig.slots.set_slot(4, &half);
        rig.slots.wire().clear_sent();
        let e = ask(&rig, recover(&broken.display().to_string()))
            .await
            .unwrap_err();
        assert!(e.contains("broken.bin"), "{e}");
        assert!(slot_ops(&rig.slots).is_empty(), "nothing sent");
    }

    /// A worker whose saved screen setting is a text, so a `Current` write changes a slot.
    fn rig_with_text(tag: &str) -> Rig {
        rig(tag, true, |dir| {
            let screen = Screen::Text {
                text: "gglinux".into(),
            };
            fs::write(
                dir.join("screen.json"),
                serde_json::to_vec(&screen).unwrap(),
            )
            .unwrap();
        })
    }

    fn current(slot: u8) -> Request {
        Request::WriteOnboard {
            slot,
            from: OnboardSource::Current,
        }
    }

    #[tokio::test]
    async fn what_the_keyboards_own_check_said_is_kept_as_a_note() {
        let rig = rig_with_text("onboard-note");
        rig.slots.refuse_next_check();
        let state = ask(&rig, current(3)).await.unwrap();
        assert!(state.error.is_none(), "{state:?}");
        let note = state.note.unwrap_or_default();
        assert!(note.contains("answered 0x01"), "{note}");
        assert_eq!(state.slots[3].valid, Some(true));
        // The next operation starts without it.
        let state = ask(&rig, Request::LoadOnboard { slot: 3 }).await.unwrap();
        assert_eq!(state.note, None);
    }

    #[tokio::test]
    async fn a_write_that_cant_be_put_back_names_its_backup_and_the_command() {
        let rig = rig_with_text("onboard-failed");
        // Every read-back sees byte 7777 wrong: the write and its three restores.
        rig.slots.corrupt_reads_after_erase(7_777, 1_000);
        let e = ask(&rig, current(4)).await.unwrap_err();
        let state = ask(&rig, Request::GetOnboard).await.unwrap();
        let backup = state.slots[4].backup.clone().unwrap();
        assert!(e.contains("failed too"), "{e}");
        assert!(
            e.contains("ggctl onboard restore 5 <backup file> --damaged"),
            "{e}"
        );
        assert!(e.contains(&format!("its backup: {backup}")), "{e}");
        assert_eq!(state.slots[4].valid, Some(false), "it may be half written");
    }

    #[tokio::test]
    async fn a_write_counts_as_running_from_its_request_to_its_end() {
        let rig = rig_with_text("onboard-running");
        let writes = rig.kb.slot_writes().clone();
        assert!(!writes.running());
        let kb = rig.kb.clone();
        let write = tokio::spawn(async move { serve(current(2), &kb).await });
        let start = std::time::Instant::now();
        while !writes.running() {
            assert!(start.elapsed() < Duration::from_secs(5), "never counted");
            tokio::time::sleep(Duration::from_millis(2)).await;
        }
        let answer = write.await.unwrap();
        assert!(matches!(answer, ResponseData::Onboard { .. }), "{answer:?}");
        assert!(!writes.running(), "ended");
        // A read isn't a write.
        ask(&rig, Request::ReadOnboard { slot: Some(0) })
            .await
            .unwrap();
        assert!(!writes.running());
        assert!(
            longest_write() < Duration::from_secs(60),
            "ggctl waits a minute"
        );
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
