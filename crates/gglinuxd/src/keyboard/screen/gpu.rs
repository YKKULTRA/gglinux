// SPDX-License-Identifier: GPL-3.0-or-later
//! The graphics card's readings for the System Monitor, without ever waking the card.
//!
//! An NVIDIA card that runtime power management has put to sleep wakes up the moment
//! anything opens `/dev/nvidia*`, and `nvidia-smi` does exactly that. So the card's
//! `power/runtime_status` under its PCI device in `/sys` is read first; this file is
//! the kernel's, and reading it never touches the card. Only when every NVIDIA card
//! already reads `active` does one `nvidia-smi` run, spawned without a shell, with a
//! time limit, and on its own thread so the keyboard's loop never waits for it.
//! Otherwise the monitor says "GPU idle".
//!
//! Asking too often could also keep an awake card from going back to sleep, since each
//! `nvidia-smi` counts as use. So it's asked at most every [`EVERY`], and never sooner
//! than the card's own `power/autosuspend_delay_ms` (plus a second) after the last time:
//! a card nobody else is using gets its full idle time to fall asleep in between.

use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Receiver, TryRecvError};
use std::sync::Arc;
use std::time::{Duration, Instant};

/// The least time between two `nvidia-smi` runs.
pub const EVERY: Duration = Duration::from_secs(2);
/// How long one `nvidia-smi` run may take before it's stopped.
pub const TIME_LIMIT: Duration = Duration::from_millis(1500);
/// The program and what it's asked: use in %, then temperature in °C, one card a line.
pub const PROGRAM: &str = "nvidia-smi";
pub const ARGS: [&str; 2] = [
    "--query-gpu=utilization.gpu,temperature.gpu",
    "--format=csv,noheader,nounits",
];

const NVIDIA: &str = "0x10de";

/// Runs a program and gives back what it printed, or `None` if it failed or ran out of
/// time. The monitor's only way to start one: tests hand in a fake.
pub trait Runner: Send + Sync {
    fn run(&self, program: &str, args: &[&str], limit: Duration) -> Option<String>;
}

/// The real thing: spawned directly (no shell), stopped at the time limit.
pub struct System;

impl Runner for System {
    fn run(&self, program: &str, args: &[&str], limit: Duration) -> Option<String> {
        if cfg!(test) {
            // The owner's card must never be woken by a test run: tests use a fake.
            panic!("a test tried to run {program}");
        }
        let mut child = Command::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()
            .ok()?;
        let until = Instant::now() + limit;
        loop {
            match child.try_wait() {
                Ok(Some(status)) => {
                    let mut out = String::new();
                    child.stdout.take()?.read_to_string(&mut out).ok()?;
                    return status.success().then_some(out);
                }
                Ok(None) if Instant::now() < until => std::thread::sleep(Duration::from_millis(10)),
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }
}

/// What the monitor shows for the card.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Shown {
    /// Asleep, or not every NVIDIA card is awake: nothing is asked.
    Idle,
    /// No NVIDIA card, none with the `nvidia` driver, or no answer yet.
    Unknown,
    /// The last answer: use in %, temperature in °C, each if the card gave it.
    Read { busy: Option<u8>, celsius: Option<i32> },
}

/// One NVIDIA graphics card's PCI device in `/sys`.
#[derive(Debug, Clone, PartialEq, Eq)]
struct Card {
    dir: PathBuf,
}

impl Card {
    fn awake(&self) -> bool {
        read(&self.dir.join("power/runtime_status")).as_deref() == Some("active")
    }

    /// Bound to NVIDIA's own driver, the only one `nvidia-smi` can talk to.
    fn on_nvidia_driver(&self) -> bool {
        std::fs::read_link(self.dir.join("driver"))
            .ok()
            .and_then(|p| p.file_name().map(|n| n == "nvidia"))
            .unwrap_or(false)
    }

    /// How long the card stays awake after its last use before it may sleep.
    fn idle_time(&self) -> Duration {
        read(&self.dir.join("power/autosuspend_delay_ms"))
            .and_then(|s| s.parse::<u64>().ok())
            .map_or(Duration::ZERO, Duration::from_millis)
    }
}

/// The NVIDIA display controllers (VGA or 3D) under `<sys>/bus/pci/devices`. Reads only
/// each device's `vendor` and `class`, which the kernel keeps.
fn cards(sys: &Path) -> Vec<Card> {
    let Ok(dir) = std::fs::read_dir(sys.join("bus/pci/devices")) else {
        return Vec::new();
    };
    let mut cards: Vec<Card> = dir
        .flatten()
        .map(|e| e.path())
        .filter(|d| {
            read(&d.join("vendor")).as_deref() == Some(NVIDIA)
                && read(&d.join("class")).is_some_and(|c| c.starts_with("0x03"))
        })
        .map(|dir| Card { dir })
        .collect();
    cards.sort_by(|a, b| a.dir.cmp(&b.dir));
    cards
}

/// The card's readings, asked for only as the rule above allows.
pub struct Gpu {
    sys: PathBuf,
    runner: Arc<dyn Runner>,
    /// Found once; looked for again if one goes away.
    cards: Option<Vec<Card>>,
    asked: Option<Instant>,
    waiting: Option<Receiver<Option<String>>>,
    last: Shown,
}

impl Gpu {
    pub fn new(sys: PathBuf, runner: Arc<dyn Runner>) -> Gpu {
        Gpu {
            sys,
            runner,
            cards: None,
            asked: None,
            waiting: None,
            last: Shown::Unknown,
        }
    }

    /// What to show at `now`; starts a new question when one is due and allowed.
    pub fn read(&mut self, now: Instant) -> Shown {
        self.collect();
        let cards = self.cards();
        if cards.is_empty() {
            return Shown::Unknown;
        }
        if !cards.iter().all(Card::awake) {
            // Forget the old answer, so waking later doesn't show it as new.
            self.last = Shown::Unknown;
            return Shown::Idle;
        }
        let asked: Vec<&Card> = cards.iter().filter(|c| c.on_nvidia_driver()).collect();
        if asked.is_empty() {
            return Shown::Unknown;
        }
        let gap = asked
            .iter()
            .map(|c| c.idle_time() + Duration::from_secs(1))
            .fold(EVERY, Duration::max);
        let due = self
            .asked
            .is_none_or(|t| now.saturating_duration_since(t) >= gap);
        if due && self.waiting.is_none() {
            self.ask(now);
        }
        self.last
    }

    fn cards(&mut self) -> Vec<Card> {
        if let Some(cards) = &self.cards {
            if cards.iter().all(|c| c.dir.exists()) {
                return cards.clone();
            }
        }
        let found = cards(&self.sys);
        self.cards = Some(found.clone());
        found
    }

    fn ask(&mut self, now: Instant) {
        let (tx, rx) = mpsc::channel();
        let runner = Arc::clone(&self.runner);
        let spawned = std::thread::Builder::new()
            .name("gg-gpu-query".into())
            .spawn(move || {
                let _ = tx.send(runner.run(PROGRAM, &ARGS, TIME_LIMIT));
            });
        self.asked = Some(now);
        if spawned.is_ok() {
            self.waiting = Some(rx);
        }
    }

    /// Take in an answer that came since the last look.
    fn collect(&mut self) {
        let Some(rx) = &self.waiting else { return };
        let answer = match rx.try_recv() {
            Ok(answer) => answer,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => None,
        };
        self.waiting = None;
        self.last = answer.as_deref().and_then(parse).unwrap_or(Shown::Unknown);
    }
}

/// `nvidia-smi`'s first line, "37, 52": use and temperature. A field the card doesn't
/// give ("[N/A]") is left out.
fn parse(out: &str) -> Option<Shown> {
    let line = out.lines().next()?;
    let mut fields = line.split(',').map(str::trim);
    let busy = fields.next()?.parse::<u8>().ok().filter(|b| *b <= 100);
    let celsius = fields.next().and_then(|t| t.parse::<i32>().ok());
    (busy.is_some() || celsius.is_some()).then_some(Shown::Read { busy, celsius })
}

fn read(path: &Path) -> Option<String> {
    std::fs::read_to_string(path)
        .ok()
        .map(|s| s.trim().to_owned())
}

#[cfg(test)]
pub(super) mod tests {
    use super::*;
    use crate::scratch::Scratch;
    use std::sync::Mutex;

    /// A runner that counts its calls and answers as told; it never starts anything.
    #[derive(Default)]
    pub struct Fake {
        pub calls: Mutex<Vec<(String, Vec<String>)>>,
        pub answer: Mutex<Option<String>>,
    }

    impl Fake {
        pub fn answering(answer: &str) -> Arc<Fake> {
            let f = Fake::default();
            *f.answer.lock().unwrap() = Some(answer.to_owned());
            Arc::new(f)
        }

        pub fn count(&self) -> usize {
            self.calls.lock().unwrap().len()
        }
    }

    impl Runner for Fake {
        fn run(&self, program: &str, args: &[&str], _limit: Duration) -> Option<String> {
            self.calls.lock().unwrap().push((
                program.to_owned(),
                args.iter().map(|a| (*a).to_owned()).collect(),
            ));
            self.answer.lock().unwrap().clone()
        }
    }

    /// A runner that fails the test if it's ever called.
    pub struct Never;

    impl Runner for Never {
        fn run(&self, program: &str, _args: &[&str], _limit: Duration) -> Option<String> {
            panic!("{program} was run while the card was asleep");
        }
    }

    /// A PCI device under `<sys>/bus/pci/devices/<slot>`.
    pub fn device(sys: &Path, slot: &str, vendor: &str, class: &str, status: &str, driver: &str) {
        let dir = sys.join("bus/pci/devices").join(slot);
        std::fs::create_dir_all(dir.join("power")).unwrap();
        std::fs::write(dir.join("vendor"), format!("{vendor}\n")).unwrap();
        std::fs::write(dir.join("class"), format!("{class}\n")).unwrap();
        std::fs::write(dir.join("power/runtime_status"), format!("{status}\n")).unwrap();
        let drivers = sys.join("bus/pci/drivers").join(driver);
        std::fs::create_dir_all(&drivers).unwrap();
        std::os::unix::fs::symlink(&drivers, dir.join("driver")).unwrap();
    }

    pub fn set_status(sys: &Path, slot: &str, status: &str) {
        let file = sys
            .join("bus/pci/devices")
            .join(slot)
            .join("power/runtime_status");
        std::fs::write(file, format!("{status}\n")).unwrap();
    }

    /// Read at `now`, then look again once the answer from any question that started is
    /// in, the clock still standing at `now`.
    fn settled(gpu: &mut Gpu, now: Instant) -> Shown {
        gpu.read(now);
        let give_up = Instant::now() + Duration::from_secs(5);
        while gpu.waiting.is_some() && Instant::now() < give_up {
            std::thread::sleep(Duration::from_millis(5));
            gpu.collect();
        }
        gpu.read(now)
    }

    #[test]
    fn a_sleeping_card_is_never_asked() {
        let d = Scratch::new("gpu-asleep");
        device(&d.0, "0000:01:00.0", NVIDIA, "0x030000", "suspended", "nvidia");
        device(&d.0, "0000:00:02.0", "0x8086", "0x030000", "active", "i915");
        let mut gpu = Gpu::new(d.0.clone(), Arc::new(Never));
        let base = Instant::now();
        for s in 0..30 {
            assert_eq!(gpu.read(base + Duration::from_secs(s)), Shown::Idle);
        }
        // Waking up or falling asleep is not awake either.
        for status in ["resuming", "suspending", "error", "unsupported"] {
            set_status(&d.0, "0000:01:00.0", status);
            assert_eq!(gpu.read(base), Shown::Idle, "{status}");
        }
    }

    #[test]
    fn one_sleeping_card_of_two_keeps_both_unasked() {
        let d = Scratch::new("gpu-two");
        device(&d.0, "0000:01:00.0", NVIDIA, "0x030000", "active", "nvidia");
        device(&d.0, "0000:02:00.0", NVIDIA, "0x030200", "suspended", "nvidia");
        let mut gpu = Gpu::new(d.0.clone(), Arc::new(Never));
        assert_eq!(gpu.read(Instant::now()), Shown::Idle);
    }

    #[test]
    fn an_awake_card_is_asked_at_most_every_two_seconds_without_a_shell() {
        let d = Scratch::new("gpu-awake");
        device(&d.0, "0000:01:00.0", NVIDIA, "0x030000", "active", "nvidia");
        // The audio function of the same card isn't a display controller.
        device(&d.0, "0000:01:00.1", NVIDIA, "0x040300", "suspended", "snd_hda_intel");
        let fake = Fake::answering("37, 52\n");
        let mut gpu = Gpu::new(d.0.clone(), fake.clone());
        let base = Instant::now();
        assert_eq!(gpu.read(base), Shown::Unknown, "no answer yet");
        assert_eq!(
            settled(&mut gpu, base + Duration::from_millis(500)),
            Shown::Read {
                busy: Some(37),
                celsius: Some(52)
            }
        );
        for ms in (500..2000).step_by(100) {
            gpu.read(base + Duration::from_millis(ms));
        }
        assert_eq!(fake.count(), 1, "once in the first two seconds");
        settled(&mut gpu, base + Duration::from_secs(2));
        assert_eq!(fake.count(), 2);
        let calls = fake.calls.lock().unwrap();
        assert_eq!(calls[0].0, "nvidia-smi", "the program itself, no shell");
        assert_eq!(
            calls[0].1,
            [
                "--query-gpu=utilization.gpu,temperature.gpu",
                "--format=csv,noheader,nounits"
            ]
        );
    }

    #[test]
    fn a_card_that_falls_asleep_stops_being_asked_and_its_old_answer_goes() {
        let d = Scratch::new("gpu-sleeps");
        device(&d.0, "0000:01:00.0", NVIDIA, "0x030000", "active", "nvidia");
        let fake = Fake::answering("5, 40");
        let mut gpu = Gpu::new(d.0.clone(), fake.clone());
        let base = Instant::now();
        gpu.read(base);
        assert!(matches!(settled(&mut gpu, base), Shown::Read { .. }));
        set_status(&d.0, "0000:01:00.0", "suspended");
        for s in 2..20 {
            assert_eq!(gpu.read(base + Duration::from_secs(s)), Shown::Idle);
        }
        assert_eq!(fake.count(), 1);
        set_status(&d.0, "0000:01:00.0", "active");
        assert_eq!(
            gpu.read(base + Duration::from_secs(20)),
            Shown::Unknown,
            "awake again: not the answer from before"
        );
    }

    #[test]
    fn the_card_gets_its_whole_idle_time_between_two_questions() {
        let d = Scratch::new("gpu-idle-time");
        device(&d.0, "0000:01:00.0", NVIDIA, "0x030000", "active", "nvidia");
        let delay = d.0.join("bus/pci/devices/0000:01:00.0/power/autosuspend_delay_ms");
        std::fs::write(delay, "5000\n").unwrap();
        let fake = Fake::answering("1, 35");
        let mut gpu = Gpu::new(d.0.clone(), fake.clone());
        let base = Instant::now();
        gpu.read(base);
        settled(&mut gpu, base);
        settled(&mut gpu, base + Duration::from_millis(5999));
        assert_eq!(fake.count(), 1, "5 s idle time + 1 s");
        settled(&mut gpu, base + Duration::from_secs(6));
        assert_eq!(fake.count(), 2);
    }

    #[test]
    fn no_nvidia_card_or_another_driver_is_unknown_and_never_asked() {
        let d = Scratch::new("gpu-none");
        device(&d.0, "0000:03:00.0", "0x1002", "0x030000", "active", "amdgpu");
        let mut gpu = Gpu::new(d.0.clone(), Arc::new(Never));
        assert_eq!(gpu.read(Instant::now()), Shown::Unknown);
        // Handed to a virtual machine: nvidia-smi can't see it.
        device(&d.0, "0000:01:00.0", NVIDIA, "0x030000", "active", "vfio-pci");
        let mut gpu = Gpu::new(d.0.clone(), Arc::new(Never));
        assert_eq!(gpu.read(Instant::now()), Shown::Unknown);
        let mut nothing = Gpu::new(d.0.join("missing"), Arc::new(Never));
        assert_eq!(nothing.read(Instant::now()), Shown::Unknown);
    }

    #[test]
    fn nvidia_smi_s_answer_is_read_field_by_field() {
        assert_eq!(
            parse("37, 52\n12, 40\n"),
            Some(Shown::Read {
                busy: Some(37),
                celsius: Some(52)
            }),
            "the first card"
        );
        assert_eq!(
            parse("[N/A], 61"),
            Some(Shown::Read {
                busy: None,
                celsius: Some(61)
            })
        );
        assert_eq!(parse("No devices were found"), None);
        assert_eq!(parse(""), None);
        assert_eq!(parse("250, x"), None, "not a percentage");
    }

    #[test]
    fn a_failed_run_shows_unknown() {
        let d = Scratch::new("gpu-fails");
        device(&d.0, "0000:01:00.0", NVIDIA, "0x030000", "active", "nvidia");
        let fake = Arc::new(Fake::default());
        let mut gpu = Gpu::new(d.0.clone(), fake.clone());
        let base = Instant::now();
        gpu.read(base);
        assert_eq!(settled(&mut gpu, base), Shown::Unknown);
        assert_eq!(fake.count(), 1);
    }
}
