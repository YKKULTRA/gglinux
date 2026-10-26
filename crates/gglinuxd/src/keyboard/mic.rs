// SPDX-License-Identifier: GPL-3.0-or-later
//! Watches whether the default microphone is muted, for the mute light.

use std::process::Command as Process;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use tracing::debug;

use super::Command;

pub const WPCTL: [&str; 3] = ["wpctl", "get-volume", "@DEFAULT_AUDIO_SOURCE@"];
const STEP: Duration = Duration::from_millis(25);

/// `wpctl get-volume` appends `[MUTED]` when muted.
pub fn muted(output: &str) -> bool {
    output.contains("[MUTED]")
}

pub struct Watch {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Watch {
    pub fn start(argv: Vec<String>, every: Duration, tx: Sender<Command>) -> Watch {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("gg-mic".into())
            .spawn(move || {
                let mut last: Option<bool> = None;
                let mut warned = false;
                while !flag.load(Ordering::Relaxed) {
                    let now = match Process::new(&argv[0]).args(&argv[1..]).output() {
                        Ok(out) => muted(&String::from_utf8_lossy(&out.stdout)),
                        Err(e) => {
                            if !warned {
                                debug!(error = %e, "could not read the microphone's mute state");
                                warned = true;
                            }
                            false
                        }
                    };
                    if last != Some(now) {
                        last = Some(now);
                        if tx.send(Command::MicMuted(now)).is_err() {
                            return;
                        }
                    }
                    let until = Instant::now() + every;
                    while Instant::now() < until && !flag.load(Ordering::Relaxed) {
                        std::thread::sleep(STEP);
                    }
                }
            })
            .expect("spawn gg-mic thread");
        Watch {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Watch {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;

    fn argv(parts: &[&str]) -> Vec<String> {
        parts.iter().map(|s| s.to_string()).collect()
    }

    fn next(rx: &mpsc::Receiver<Command>) -> Option<bool> {
        match rx.recv_timeout(Duration::from_millis(400)) {
            Ok(Command::MicMuted(m)) => Some(m),
            Ok(other) => panic!("unexpected {other:?}"),
            Err(_) => None,
        }
    }

    #[test]
    fn wpctl_output_says_muted_in_brackets() {
        assert!(muted("Volume: 1.00 [MUTED]\n"));
        assert!(!muted("Volume: 0.40\n"));
        assert!(!muted(""));
    }

    #[test]
    fn the_state_is_sent_once_then_only_on_change() {
        let (tx, rx) = mpsc::channel();
        let _w = Watch::start(
            argv(&["echo", "Volume: 0.40 [MUTED]"]),
            Duration::from_millis(50),
            tx,
        );
        assert_eq!(next(&rx), Some(true));
        assert_eq!(next(&rx), None, "polled again, unchanged: nothing sent");
    }

    #[test]
    fn a_failing_command_counts_as_unmuted_and_is_sent_once() {
        let (tx, rx) = mpsc::channel();
        let _w = Watch::start(argv(&["/nonexistent/wpctl"]), Duration::from_millis(50), tx);
        assert_eq!(next(&rx), Some(false));
        assert_eq!(next(&rx), None);
    }
}
