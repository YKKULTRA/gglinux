//! Feeds the visualizer: records an output's monitor with `pw-record` — the
//! default output's, or with Sonar on the device its channels play into — and
//! keeps 18 band levels current. Audio is analysed in memory only; nothing is
//! written anywhere.

use std::io::{self, Read};
use std::process::{Child, Command as Process, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;

use super::spectrum::{Analyzer, BANDS, HOP};
use super::Command;

/// Mono 48 kHz f32 of what's playing, raw on stdout: the monitor of `output`
/// (a `node.name`), or of the default output when there's none.
pub fn pw_record(output: Option<&str>) -> Vec<&str> {
    let mut argv = vec![
        "pw-record",
        "--raw",
        "-P",
        "{ stream.capture.sink = true }",
        "--format",
        "f32",
        "--rate",
        "48000",
        "--channels",
        "1",
        "--latency",
        "20ms",
    ];
    if let Some(name) = output {
        argv.extend(["--target", name]);
    }
    argv.push("-");
    argv
}

pub type Levels = Arc<Mutex<[f32; BANDS]>>;

pub struct Capture {
    stop: Arc<AtomicBool>,
    child: Child,
    thread: Option<JoinHandle<()>>,
}

impl Capture {
    /// Run `argv` (raw little-endian f32, mono, 48 kHz on stdout) and keep `levels` current.
    pub fn start(argv: &[&str], levels: Levels, tx: Sender<Command>) -> io::Result<Capture> {
        let (program, args) = argv
            .split_first()
            .ok_or_else(|| io::Error::other("empty command"))?;
        let mut child = Process::new(program)
            .args(args)
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::null())
            .spawn()?;
        let mut out = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("no stdout"))?;
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("gg-audio".into())
            .spawn(move || {
                let mut analyzer = Analyzer::new();
                let mut bytes = vec![0u8; HOP * 4];
                let mut hop = vec![0f32; HOP];
                while out.read_exact(&mut bytes).is_ok() {
                    // `as_chunks` needs Rust 1.88; older clippy (1.97 included) does not know this lint.
                    #[allow(unknown_lints, clippy::chunks_exact_to_as_chunks)]
                    let samples = bytes.chunks_exact(4);
                    for (s, b) in hop.iter_mut().zip(samples) {
                        *s = f32::from_le_bytes([b[0], b[1], b[2], b[3]]);
                    }
                    let new = analyzer.push(&hop);
                    *levels.lock().unwrap_or_else(|e| e.into_inner()) = new;
                }
                if !flag.load(Ordering::Relaxed) {
                    let _ = tx.send(Command::AudioStopped);
                }
            })?;
        Ok(Capture {
            stop,
            child,
            thread: Some(thread),
        })
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(t) = self.thread.take() {
            let _ = t.join(); // the pipe closed with the child, so the read has returned
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    fn levels() -> Levels {
        Arc::new(Mutex::new([0.0; BANDS]))
    }

    #[test]
    fn the_capture_reports_when_its_program_ends() {
        // Four hops of a 1 kHz tone as raw little-endian f32, then end of file.
        let path = std::env::temp_dir().join(format!("gglinux-audio-{}.raw", std::process::id()));
        let bytes: Vec<u8> = (0..4 * HOP)
            .flat_map(|i| {
                (0.5 * (std::f32::consts::TAU * 1000.0 * i as f32 / 48_000.0).sin()).to_le_bytes()
            })
            .collect();
        std::fs::write(&path, bytes).unwrap();
        let (tx, rx) = mpsc::channel();
        let l = levels();
        let _capture =
            Capture::start(&["cat", path.to_str().unwrap()], Arc::clone(&l), tx).unwrap();
        assert!(matches!(
            rx.recv_timeout(Duration::from_secs(2)),
            Ok(Command::AudioStopped)
        ));
        std::fs::remove_file(&path).unwrap();
        let got = *l.lock().unwrap();
        let loudest = (0..BANDS)
            .max_by(|&a, &b| got[a].total_cmp(&got[b]))
            .unwrap();
        assert_eq!(loudest, 9, "the tone was analysed: {got:?}");
    }

    #[test]
    fn the_capture_listens_to_the_output_it_is_given_or_else_the_default() {
        let default = pw_record(None);
        assert!(
            !default.contains(&"--target"),
            "WirePlumber links the default output: {default:?}"
        );
        let sonar = pw_record(Some("alsa_output.usb-SteelSeries"));
        assert!(
            sonar
                .windows(2)
                .any(|w| w == ["--target", "alsa_output.usb-SteelSeries"]),
            "{sonar:?}"
        );
        assert!(
            sonar.contains(&"{ stream.capture.sink = true }"),
            "its monitor, not an input"
        );
        assert_eq!(sonar.last(), Some(&"-"), "samples still go to stdout");
    }

    #[test]
    fn a_missing_program_is_an_error_not_a_panic() {
        let (tx, _rx) = mpsc::channel();
        assert!(Capture::start(&["/nonexistent/pw-record"], levels(), tx).is_err());
    }

    #[test]
    fn dropping_the_capture_kills_its_program_promptly() {
        let (tx, rx) = mpsc::channel();
        let capture = Capture::start(&["sleep", "30"], levels(), tx).unwrap();
        let started = Instant::now();
        drop(capture);
        assert!(started.elapsed() < Duration::from_secs(1));
        assert!(
            rx.recv_timeout(Duration::from_millis(100)).is_err(),
            "no AudioStopped for a stop we asked for"
        );
    }
}
