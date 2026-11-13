// SPDX-License-Identifier: GPL-3.0-or-later
//! A base station that isn't there, for tests (this crate's and gglinuxd's).
//!
//! [`FakeBase`]:
//!
//! - keeps every output report sent, in order, with when it came
//!   ([`FakeBase::reports`], [`FakeBase::commands`], [`FakeBase::gaps`]);
//! - answers the queries `b0`, `20` and `80` with a whole 64-byte reply on report
//!   `0x06`, laid out as Arctis Sound Manager reads them. It starts on GG's
//!   defaults, the settings the owner's base station has had since the captures:
//!   the `b0` reply is the one GG got in `arctis-1` (headset off), the `20` and `80`
//!   replies the ones it got in `arctis-5`;
//! - takes the setters it knows into those replies, but **lags** like a real
//!   device: a change shows only from the second read of the reply that holds it.
//!   The first read after the command still gets the old value, so code that reads
//!   once, straight after a write, and trusts what it gets fails here as it would
//!   on the device;
//! - answers nothing else (a query it doesn't know times out).
//!
//! It checks nothing: ranges are [`crate::proto`]'s, before the port.

use std::collections::VecDeque;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use anyhow::Result;

use crate::Port;

const REPORT: usize = 64;

/// Which reply a byte lives in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Reply {
    Status,
    Audio,
    Ux,
}

#[derive(Debug)]
struct Inner {
    reports: Vec<(Instant, Vec<u8>)>,
    features: Vec<Vec<u8>>,
    queue: VecDeque<Vec<u8>>,
    status: [u8; REPORT],
    audio: [u8; REPORT],
    ux: [u8; REPORT],
    /// Changes not shown yet: each shows once its reply has been read once.
    pending: Vec<(Reply, Vec<(usize, u8)>)>,
}

/// A base station for tests. Clones share one fake: keep one to look at what
/// reached it.
#[derive(Debug, Clone)]
pub struct FakeBase(Arc<Mutex<Inner>>);

impl Default for FakeBase {
    fn default() -> Self {
        Self::new()
    }
}

impl FakeBase {
    pub fn new() -> FakeBase {
        let mut status = [0u8; REPORT];
        // GG's read in arctis-1: Bluetooth off at power-on, calls do nothing,
        // Bluetooth off, spare 100 %, transparency 8, ANC, mute light 10, auto-off
        // 30 min, speed, paired but the headset is off.
        status[..16].copy_from_slice(&[
            0x06, 0xb0, 0x00, 0x00, 0x01, 0x00, 0x00, 0x08, 0x08, 0x00, 0x02, 0x0a, 0x05, 0x00,
            0x04, 0x01,
        ]);
        let mut audio = [0u8; REPORT];
        // GG's read in arctis-5: volume 3, gain low, EQ Flat with a flat Custom
        // slot, mic volume 10, sidetone off, line out to speakers, stream mix
        // 100/100/100.
        audio[..26].copy_from_slice(&[
            0x06, 0x20, 0x00, 0x03, 0x01, 0x00, 0x00, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14, 0x14,
            0x14, 0x14, 0x14, 0x0a, 0x00, 0x01, 0x64, 0x64, 0x64, 0x00, 0x64, 0x64,
        ]);
        let mut ux = [0u8; REPORT];
        // GG's read in arctis-5: dim after 1 min, brightness 10, home screen detailed.
        ux[..6].copy_from_slice(&[0x06, 0x80, 0x01, 0x0a, 0x01, 0x00]);
        FakeBase(Arc::new(Mutex::new(Inner {
            reports: Vec::new(),
            features: Vec::new(),
            queue: VecDeque::new(),
            status,
            audio,
            ux,
            pending: Vec::new(),
        })))
    }

    fn inner(&self) -> MutexGuard<'_, Inner> {
        self.0.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Every output report, whole (64 bytes, report id first), in order.
    pub fn reports(&self) -> Vec<Vec<u8>> {
        self.inner()
            .reports
            .iter()
            .map(|(_, r)| r.clone())
            .collect()
    }

    /// Every output report cut to its command's own length (report id, opcode and
    /// the arguments it takes), so `06 b2 00` stays three bytes.
    pub fn commands(&self) -> Vec<Vec<u8>> {
        self.reports()
            .into_iter()
            .map(|r| {
                let len = command_len(&r);
                r[..len].to_vec()
            })
            .collect()
    }

    /// The time between each output report and the one before it.
    pub fn gaps(&self) -> Vec<Duration> {
        let inner = self.inner();
        inner
            .reports
            .windows(2)
            .map(|w| w[1].0.duration_since(w[0].0))
            .collect()
    }

    /// Every feature report (screen frames), in order.
    pub fn features(&self) -> Vec<Vec<u8>> {
        self.inner().features.clone()
    }
}

/// How many bytes of a report its command uses.
fn command_len(report: &[u8]) -> usize {
    let n = match report.get(1) {
        Some(0x09 | 0x20 | 0x80 | 0x95 | 0xb0 | 0xb7) => 2,
        Some(0x33) => 12,
        Some(0x47) => 6,
        Some(_) => 3,
        None => report.len(),
    };
    n.min(report.len())
}

/// Where a setter's bytes land, and in which reply.
fn landing(report: &[u8]) -> Option<(Reply, Vec<(usize, u8)>)> {
    let arg = |i: usize| report.get(i).copied().unwrap_or(0);
    let one = |reply, at| Some((reply, vec![(at, arg(2))]));
    match report.get(1)? {
        0xb2 => one(Reply::Status, 2),
        0xb3 => one(Reply::Status, 3),
        0xb9 => one(Reply::Status, 8),
        0xbd => one(Reply::Status, 10),
        0xbf => one(Reply::Status, 11),
        0xc1 => one(Reply::Status, 12),
        0xc3 => one(Reply::Status, 13),
        0x25 => one(Reply::Audio, 3),
        0x27 => one(Reply::Audio, 4),
        0x2e => one(Reply::Audio, 6),
        0x37 => one(Reply::Audio, 0x11),
        0x39 => one(Reply::Audio, 0x12),
        0x43 => one(Reply::Audio, 0x13),
        0x33 => Some((Reply::Audio, (0..10).map(|i| (7 + i, arg(2 + i))).collect())),
        0x47 => Some((
            Reply::Audio,
            vec![(0x16, arg(2)), (0x18, arg(4)), (0x19, arg(5))],
        )),
        0x83 => one(Reply::Ux, 2),
        0x85 => one(Reply::Ux, 3),
        0x89 => one(Reply::Ux, 5),
        _ => None,
    }
}

impl Inner {
    fn reply(&mut self, which: Reply) {
        let reply = match which {
            Reply::Status => self.status,
            Reply::Audio => self.audio,
            Reply::Ux => self.ux,
        };
        self.queue.push_back(reply.to_vec());
        // Read once now: the changes waiting for this reply show from the next.
        let (due, waiting) = std::mem::take(&mut self.pending)
            .into_iter()
            .partition::<Vec<_>, _>(|(r, _)| *r == which);
        self.pending = waiting;
        for (_, bytes) in due {
            let target = match which {
                Reply::Status => &mut self.status,
                Reply::Audio => &mut self.audio,
                Reply::Ux => &mut self.ux,
            };
            for (at, value) in bytes {
                target[at] = value;
            }
        }
    }
}

impl Port for FakeBase {
    fn write(&self, report: &[u8]) -> Result<()> {
        let mut inner = self.inner();
        inner.reports.push((Instant::now(), report.to_vec()));
        match report.get(1) {
            Some(0xb0) => inner.reply(Reply::Status),
            Some(0x20) => inner.reply(Reply::Audio),
            Some(0x80) => inner.reply(Reply::Ux),
            _ => {
                if let Some(change) = landing(report) {
                    inner.pending.push(change);
                }
            }
        }
        Ok(())
    }

    fn send_feature(&self, report: &[u8]) -> Result<()> {
        self.inner().features.push(report.to_vec());
        Ok(())
    }

    fn read(&self, buf: &mut [u8], _timeout_ms: i32) -> Result<usize> {
        let Some(report) = self.inner().queue.pop_front() else {
            return Ok(0);
        };
        let n = report.len().min(buf.len());
        buf[..n].copy_from_slice(&report[..n]);
        Ok(n)
    }
}
