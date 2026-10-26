// SPDX-License-Identifier: GPL-3.0-or-later
//! Reads key presses from the Apex's typing interfaces, for reactive lighting.
//!
//! Only while reactive lighting is on: the worker starts a [`Reader`] then,
//! and dropping it closes both nodes. They're opened read-only; nothing is
//! ever written to them. Which keys are pressed is private — the codes go to
//! the worker and nowhere else, and nothing here logs them.

use std::fs::{File, OpenOptions};
use std::io::{self, Read};
use std::os::fd::AsRawFd;
use std::os::unix::fs::OpenOptionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::Sender;
use std::sync::Arc;
use std::thread::JoinHandle;
use std::time::Instant;

use gg_device_keyboard::keys::{self, KeySet};
use gg_device_keyboard::{PRODUCT_ID, VENDOR_ID};
use hidapi::HidApi;

use super::Command;

/// How long one poll waits before checking whether to stop.
const POLL_MS: i32 = 200;
/// The boot keyboard interface and the N-key-rollover one.
const TYPING_INTERFACES: [(i32, Kind); 2] = [(0, Kind::Boot), (2, Kind::Nkro)];

#[derive(Debug)]
pub enum OpenError {
    /// Permission denied: the optional udev rule isn't installed.
    NoAccess,
    /// Anything else (not plugged in, node missing); worth retrying.
    Unavailable(String),
}

/// Which report an interface sends.
#[derive(Clone, Copy, Debug)]
enum Kind {
    Boot,
    Nkro,
}

pub struct Reader {
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl Reader {
    /// Open the Apex's typing interfaces and start reading them.
    pub fn start(api: &HidApi, tx: Sender<Command>) -> Result<Reader, OpenError> {
        let files = open_all(&typing_paths(api)?)?;
        Ok(Self::spawn(files, tx))
    }

    fn spawn(files: Vec<(File, Kind)>, tx: Sender<Command>) -> Reader {
        let stop = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&stop);
        let thread = std::thread::Builder::new()
            .name("gg-keys".into())
            .spawn(move || {
                if !run(&files, &flag, &tx) {
                    let _ = tx.send(Command::ReaderStopped);
                }
            })
            .expect("spawn gg-keys thread");
        Reader {
            stop,
            thread: Some(thread),
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

/// The hidraw paths of both typing interfaces, or why not.
fn typing_paths(api: &HidApi) -> Result<Vec<(PathBuf, Kind)>, OpenError> {
    TYPING_INTERFACES
        .iter()
        .map(|&(iface, kind)| {
            api.device_list()
                .find(|d| {
                    d.vendor_id() == VENDOR_ID
                        && d.product_id() == PRODUCT_ID
                        && d.interface_number() == iface
                })
                .and_then(|d| d.path().to_str().ok().map(PathBuf::from))
                .map(|p| (p, kind))
                .ok_or_else(|| {
                    OpenError::Unavailable(format!("typing interface {iface} not found"))
                })
        })
        .collect()
}

/// Open every path read-only, or none: one denied node means no access at all.
fn open_all(paths: &[(PathBuf, Kind)]) -> Result<Vec<(File, Kind)>, OpenError> {
    paths
        .iter()
        .map(|(path, kind)| {
            OpenOptions::new()
                .read(true)
                .custom_flags(libc::O_NONBLOCK)
                .open(path)
                .map(|f| (f, *kind))
                .map_err(|e| match e.kind() {
                    io::ErrorKind::PermissionDenied => OpenError::NoAccess,
                    _ => OpenError::Unavailable(format!("{}: {e}", path.display())),
                })
        })
        .collect() // on the first error the files opened so far are dropped, so closed
}

/// Read until told to stop (`true`) or the device fails (`false`).
fn run(files: &[(File, Kind)], stop: &AtomicBool, tx: &Sender<Command>) -> bool {
    let mut last = vec![KeySet::default(); files.len()];
    let mut buf = [0u8; 64];
    while !stop.load(Ordering::Relaxed) {
        let mut fds: Vec<libc::pollfd> = files
            .iter()
            .map(|(f, _)| libc::pollfd {
                fd: f.as_raw_fd(),
                events: libc::POLLIN,
                revents: 0,
            })
            .collect();
        let n = unsafe { libc::poll(fds.as_mut_ptr(), fds.len() as libc::nfds_t, POLL_MS) };
        if n < 0 {
            if io::Error::last_os_error().kind() == io::ErrorKind::Interrupted {
                continue;
            }
            return false;
        }
        for (i, p) in fds.iter().enumerate() {
            if p.revents & libc::POLLIN != 0 {
                let len = match (&files[i].0).read(&mut buf) {
                    Ok(0) => return false, // end of file: the device is gone
                    Ok(len) => len,
                    Err(e)
                        if matches!(
                            e.kind(),
                            io::ErrorKind::WouldBlock | io::ErrorKind::Interrupted
                        ) =>
                    {
                        continue
                    }
                    Err(_) => return false,
                };
                let now = match files[i].1 {
                    Kind::Boot => keys::boot_keys(&buf[..len]),
                    Kind::Nkro => keys::nkro_keys(&buf[..len]),
                };
                let Some(now) = now else { continue }; // rollover error or odd length: no change
                let at = Instant::now();
                for usage in keys::pressed(last[i], now) {
                    if tx
                        .send(Command::Pressed {
                            code: keys::led(usage),
                            at,
                        })
                        .is_err()
                    {
                        return true; // the worker is gone; nobody to tell
                    }
                }
                last[i] = now;
            } else if p.revents & (libc::POLLHUP | libc::POLLERR | libc::POLLNVAL) != 0 {
                return false;
            }
        }
    }
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_keyboard::layout::ANSI_TKL;
    use std::io::Write;
    use std::os::fd::FromRawFd;
    use std::os::unix::fs::PermissionsExt;
    use std::sync::mpsc::{self, Receiver, RecvTimeoutError};
    use std::time::{Duration, Instant};

    /// A connected pair of packet sockets: one write is one read, like a hidraw report.
    fn packet_pair() -> (File, File) {
        let mut fds = [0; 2];
        let rc = unsafe {
            libc::socketpair(
                libc::AF_UNIX,
                libc::SOCK_SEQPACKET | libc::SOCK_CLOEXEC,
                0,
                fds.as_mut_ptr(),
            )
        };
        assert_eq!(rc, 0, "socketpair");
        unsafe { (File::from_raw_fd(fds[0]), File::from_raw_fd(fds[1])) }
    }

    fn next(rx: &Receiver<Command>) -> Option<Command> {
        match rx.recv_timeout(Duration::from_millis(600)) {
            Ok(c) => Some(c),
            Err(RecvTimeoutError::Timeout) => None,
            Err(RecvTimeoutError::Disconnected) => panic!("reader dropped its sender"),
        }
    }

    fn code(c: Option<Command>) -> u8 {
        match c {
            Some(Command::Pressed { code, .. }) => code,
            other => panic!("expected a press, got {other:?}"),
        }
    }

    #[test]
    fn held_keys_and_rollover_errors_do_not_re_fire() {
        let (ours, mut kb) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let _reader = Reader::spawn(vec![(ours, Kind::Boot)], tx);
        kb.write_all(&[0, 0, 0x04, 0, 0, 0, 0, 0]).unwrap(); // A down
        assert_eq!(code(next(&rx)), 0x04);
        kb.write_all(&[0, 0, 0x04, 0x05, 0, 0, 0, 0]).unwrap(); // B down, A held
        assert_eq!(code(next(&rx)), 0x05);
        kb.write_all(&[0, 0, 1, 1, 1, 1, 1, 1]).unwrap(); // rollover error
        kb.write_all(&[0, 0, 0x04, 0x05, 0x06, 0, 0, 0]).unwrap(); // C down
        assert_eq!(
            code(next(&rx)),
            0x06,
            "only C: the rollover error changed nothing"
        );
        kb.write_all(&[0; 8]).unwrap(); // all up
        assert!(next(&rx).is_none(), "releases are not presses");
    }

    #[test]
    fn nkro_reports_are_read_from_their_own_interface() {
        let (boot, _kb0) = packet_pair();
        let (nkro, mut kb2) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let _reader = Reader::spawn(vec![(boot, Kind::Boot), (nkro, Kind::Nkro)], tx);
        let mut report = [0u8; 32];
        report[0x2C / 8] |= 1 << (0x2C % 8); // Space
        kb2.write_all(&report).unwrap();
        assert_eq!(code(next(&rx)), 0x2C);
    }

    #[test]
    fn the_menu_key_presses_the_key_the_layout_calls_menu() {
        let (ours, mut kb) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let _reader = Reader::spawn(vec![(ours, Kind::Boot)], tx);
        kb.write_all(&[0, 0, 0x65, 0, 0, 0, 0, 0]).unwrap(); // Menu types Application, 0x65
        let menu = ANSI_TKL.iter().find(|k| k.1 == "Menu").unwrap().0;
        assert_eq!(code(next(&rx)), menu, "a press the lighting can find");
    }

    #[test]
    fn peer_closing_stops_the_reader_and_reports_it() {
        let (ours, kb) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let reader = Reader::spawn(vec![(ours, Kind::Boot)], tx);
        drop(kb); // the keyboard went away
        assert!(matches!(next(&rx), Some(Command::ReaderStopped)));
        let started = Instant::now();
        drop(reader);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "the thread had already ended"
        );
    }

    #[test]
    fn dropping_the_reader_stops_it_quietly() {
        let (ours, _kb) = packet_pair();
        let (tx, rx) = mpsc::channel();
        let reader = Reader::spawn(vec![(ours, Kind::Boot)], tx);
        let started = Instant::now();
        drop(reader);
        assert!(
            started.elapsed() < Duration::from_secs(1),
            "stops within a poll interval"
        );
        assert!(
            matches!(
                rx.recv_timeout(Duration::from_millis(100)),
                Err(RecvTimeoutError::Disconnected)
            ),
            "no ReaderStopped for a stop we asked for"
        );
    }

    #[test]
    fn one_denied_node_means_no_access_and_nothing_stays_open() {
        if unsafe { libc::geteuid() } == 0 {
            return; // root reads anything; the check means nothing
        }
        let dir = std::env::temp_dir().join(format!("gglinux-reader-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let open = dir.join("iface0");
        let denied = dir.join("iface2");
        std::fs::write(&open, b"").unwrap();
        std::fs::write(&denied, b"").unwrap();
        std::fs::set_permissions(&denied, std::fs::Permissions::from_mode(0o000)).unwrap();
        let result = open_all(&[(open, Kind::Boot), (denied, Kind::Nkro)]);
        std::fs::remove_dir_all(&dir).unwrap();
        assert!(matches!(result, Err(OpenError::NoAccess)), "got {result:?}");
    }

    #[test]
    fn a_press_never_prints_its_key() {
        let c = Command::Pressed {
            code: 0x04,
            at: Instant::now(),
        };
        assert_eq!(format!("{c:?}"), "Pressed");
    }
}
