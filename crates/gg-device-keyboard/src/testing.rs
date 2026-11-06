// SPDX-License-Identifier: GPL-3.0-or-later
//! A keyboard that isn't there, for tests. Built for this crate's tests, and for other
//! crates' with the `testing` feature (gglinuxd's tests turn it on).
//!
//! [`FakeTransport`]:
//!
//! - keeps every report sent, in order, with its kind ([`FakeTransport::sent`]);
//! - answers a feature read for the last feature report sent, and an input read for the
//!   last output report, each looked up by that report's opcode and sub-byte ([`Key`]),
//!   the more specific key first. A read is a whole report, zero-padded, as the
//!   keyboard's are. A feature read nobody scripted gets the keyboard's bare
//!   acknowledgement, `<opcode> 00`; an input read nobody scripted times out. Each output
//!   report is answered once, as the keyboard sends one input report per query;
//! - keeps an input queue, as the kernel does for an open hidraw file: reports put there
//!   ([`FakeTransport::queue_input`], or [`FakeTransport::queue_input_after`] once a
//!   command goes out) are read before any answer, so code that takes the next input
//!   report for its own answer gets an old one, as it would on the device;
//! - fails the sends [`FakeTransport::fail_send`] and [`FakeTransport::fail_sends`] name:
//!   they reach nothing and aren't kept; and the reads [`FakeTransport::fail_reads`] names;
//! - **lags** like the keyboard: a change scripted with [`FakeTransport::change_after`]
//!   shows only from the second read after the command that makes it. The first read
//!   still gets the old answer, so code that reads once, straight after a write, and
//!   trusts what it gets fails here as it would on the device.
//!
//! It checks nothing: the allowlist is [`crate::Keyboard`]'s, before the transport.

use std::collections::VecDeque;
use std::ops::{Bound, RangeBounds};
use std::sync::{Arc, Mutex, MutexGuard};

use anyhow::{bail, ensure, Result};

use crate::{proto, Keyboard, Transport};

/// How a report was sent.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Feature,
    Output,
}

/// One report as it reached the fake, report id first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Sent {
    pub kind: Kind,
    pub report: Vec<u8>,
}

/// Which read an answer is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Read {
    /// A feature report read after a feature report.
    Feature,
    /// An input report read after an output report.
    Input,
}

/// Which requests an answer is for: every report with this opcode, or only those with
/// this sub-byte after it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Key {
    pub op: u8,
    pub sub: Option<u8>,
}

impl Key {
    pub fn op(op: u8) -> Key {
        Key { op, sub: None }
    }

    pub fn sub(op: u8, sub: u8) -> Key {
        Key { op, sub: Some(sub) }
    }

    /// Whether `report` (report id first) is a request this key names.
    pub fn matches(self, report: &[u8]) -> bool {
        report.get(1) == Some(&self.op) && self.sub.is_none_or(|s| report.get(2) == Some(&s))
    }
}

/// Makes a reply from the request it answers (report id first).
type Responder = Box<dyn FnMut(&[u8]) -> Vec<u8> + Send>;

enum Answer {
    Fixed(Vec<u8>),
    With(Responder),
}

struct Scripted {
    read: Read,
    key: Key,
    answer: Answer,
}

/// A command that changes what a read answers.
struct Change {
    command: Key,
    read: Read,
    key: Key,
    reply: Vec<u8>,
}

/// A change whose command was sent, waiting out the lag.
struct Pending {
    read: Read,
    key: Key,
    reply: Vec<u8>,
    /// Matching reads since the command.
    reads: u32,
}

/// Counts from 1, the end left out.
type Span = (usize, usize);

fn span(range: impl RangeBounds<usize>) -> Span {
    let start = match range.start_bound() {
        Bound::Included(&n) => n,
        Bound::Excluded(&n) => n + 1,
        Bound::Unbounded => 1,
    };
    let end = match range.end_bound() {
        Bound::Included(&n) => n + 1,
        Bound::Excluded(&n) => n,
        Bound::Unbounded => usize::MAX,
    };
    (start, end)
}

#[derive(Default)]
struct Inner {
    sent: Vec<Sent>,
    /// Sends tried, failed ones too.
    sends: usize,
    fail: Vec<usize>,
    fail_spans: Vec<Span>,
    /// Reads tried (feature and input reads together), failed ones too.
    reads: usize,
    fail_reads: Vec<Span>,
    /// Input reports waiting to be read, oldest first.
    queue: VecDeque<Vec<u8>>,
    /// Input reports that come in once a command matching the key goes out.
    queue_after: Vec<(Key, Vec<u8>)>,
    script: Vec<Scripted>,
    changes: Vec<Change>,
    pending: Vec<Pending>,
    last_feature: Option<Vec<u8>>,
    last_output: Option<Vec<u8>>,
}

/// See the module docs. Clones share one fake: keep one to look at what was sent after
/// handing another to a [`Keyboard`].
#[derive(Clone, Default)]
pub struct FakeTransport {
    inner: Arc<Mutex<Inner>>,
}

impl FakeTransport {
    pub fn new() -> FakeTransport {
        FakeTransport::default()
    }

    /// A keyboard on this fake, every gated family off.
    pub fn keyboard(&self) -> Keyboard {
        Keyboard::with_transport(Box::new(self.clone()))
    }

    /// Answer a feature read after a feature report matching `key` with `reply`: the
    /// bytes after the report id.
    pub fn answer_feature(&self, key: Key, reply: &[u8]) -> &Self {
        self.script(Read::Feature, key, Answer::Fixed(reply.to_vec()))
    }

    /// Answer a feature read after a feature report matching `key` with what `reply`
    /// makes of that report.
    pub fn answer_feature_with(
        &self,
        key: Key,
        reply: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static,
    ) -> &Self {
        self.script(Read::Feature, key, Answer::With(Box::new(reply)))
    }

    /// Answer an output report matching `key` with the input report `reply`.
    pub fn answer_input(&self, key: Key, reply: &[u8]) -> &Self {
        self.script(Read::Input, key, Answer::Fixed(reply.to_vec()))
    }

    /// Answer an output report matching `key` with what `reply` makes of it.
    pub fn answer_input_with(
        &self,
        key: Key,
        reply: impl FnMut(&[u8]) -> Vec<u8> + Send + 'static,
    ) -> &Self {
        self.script(Read::Input, key, Answer::With(Box::new(reply)))
    }

    /// Once a report matching `command` is sent, `read`s after requests matching `key`
    /// answer `reply` instead — from the second such read on. The first still gets the
    /// old answer, as the keyboard serves the state from before a command on the first
    /// read after it.
    pub fn change_after(&self, command: Key, read: Read, key: Key, reply: &[u8]) -> &Self {
        self.lock().changes.push(Change {
            command,
            read,
            key,
            reply: reply.to_vec(),
        });
        self
    }

    /// Make the `n`th send fail, counting from 1, feature and output reports together.
    pub fn fail_send(&self, n: usize) -> &Self {
        self.lock().fail.push(n);
        self
    }

    /// Make every send in `range` fail, counting as [`FakeTransport::fail_send`] does:
    /// `40..` is a keyboard gone from the 40th send on.
    pub fn fail_sends(&self, range: impl RangeBounds<usize>) -> &Self {
        self.lock().fail_spans.push(span(range));
        self
    }

    /// Make every read in `range` fail, counting from 1, feature and input reads together
    /// (each empty read of a drain too).
    pub fn fail_reads(&self, range: impl RangeBounds<usize>) -> &Self {
        self.lock().fail_reads.push(span(range));
        self
    }

    /// Put an input report in the queue now: one nobody read yet, read before any answer.
    pub fn queue_input(&self, report: &[u8]) -> &Self {
        self.lock().queue.push_back(report.to_vec());
        self
    }

    /// Once a report matching `command` goes out, `report` comes into the queue, ahead of
    /// that command's own answer: a late answer to something earlier. Once.
    pub fn queue_input_after(&self, command: Key, report: &[u8]) -> &Self {
        self.lock().queue_after.push((command, report.to_vec()));
        self
    }

    /// How many input reports wait in the queue.
    pub fn queued_input(&self) -> usize {
        self.lock().queue.len()
    }

    /// The oldest input report in the queue, taken out. For fakes built on this one that
    /// answer reads themselves: the queue comes before their answers too.
    pub fn take_queued_input(&self) -> Option<Vec<u8>> {
        self.lock().queue.pop_front()
    }

    /// Count a read, and fail it if [`FakeTransport::fail_reads`] names it. This fake's own
    /// reads do; a fake built on it calls this for each read it answers.
    pub fn count_read(&self) -> Result<()> {
        let mut inner = self.lock();
        inner.reads += 1;
        let n = inner.reads;
        if inner.fail_reads.iter().any(|&(a, b)| (a..b).contains(&n)) {
            bail!("the fake keyboard failed read {n}");
        }
        Ok(())
    }

    /// Answer a feature read as this fake does, without counting it.
    pub fn feature_reply(&self, buf: &mut [u8]) -> Result<usize> {
        ensure!(
            buf.first() == Some(&0),
            "a feature read needs report id 0 in the buffer's first byte"
        );
        let mut inner = self.lock();
        let reply = match inner.last_feature.clone() {
            Some(request) => inner
                .answer(Read::Feature, &request)
                .unwrap_or_else(|| vec![request[1], 0x00]),
            None => Vec::new(),
        };
        Ok(fill(buf, 1, &reply, proto::FEATURE_REPORT))
    }

    /// Answer an input read as this fake does (the queue, then the last output report's
    /// answer), without counting it.
    pub fn input_reply(&self, buf: &mut [u8]) -> usize {
        let mut inner = self.lock();
        if let Some(report) = inner.queue.pop_front() {
            return fill(buf, 0, &report, proto::INPUT_REPORT);
        }
        let Some(request) = inner.last_output.take() else {
            return 0;
        };
        match inner.answer(Read::Input, &request) {
            Some(reply) => fill(buf, 0, &reply, proto::INPUT_REPORT),
            None => 0,
        }
    }

    /// Every report sent so far, in order.
    pub fn sent(&self) -> Vec<Sent> {
        self.lock().sent.clone()
    }

    /// Forget the reports sent so far (the count for [`FakeTransport::fail_send`] goes on).
    pub fn clear_sent(&self) {
        self.lock().sent.clear();
    }

    fn script(&self, read: Read, key: Key, answer: Answer) -> &Self {
        let mut inner = self.lock();
        inner.script.retain(|s| !(s.read == read && s.key == key));
        inner.script.push(Scripted { read, key, answer });
        drop(inner);
        self
    }

    fn lock(&self) -> MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(|e| e.into_inner())
    }
}

impl Inner {
    fn record(&mut self, kind: Kind, report: &[u8]) -> Result<()> {
        self.sends += 1;
        let n = self.sends;
        if self.fail.contains(&n) || self.fail_spans.iter().any(|&(a, b)| (a..b).contains(&n)) {
            bail!("the fake keyboard failed send {n}");
        }
        self.sent.push(Sent {
            kind,
            report: report.to_vec(),
        });
        if let Some(i) = self.queue_after.iter().position(|(k, _)| k.matches(report)) {
            let (_, late) = self.queue_after.remove(i);
            self.queue.push_back(late);
        }
        for c in self.changes.iter().filter(|c| c.command.matches(report)) {
            self.pending.push(Pending {
                read: c.read,
                key: c.key,
                reply: c.reply.clone(),
                reads: 0,
            });
        }
        match kind {
            Kind::Feature => self.last_feature = Some(report.to_vec()),
            Kind::Output => self.last_output = Some(report.to_vec()),
        }
        Ok(())
    }

    /// What a read after `request` gets, if anything is scripted for it.
    fn answer(&mut self, read: Read, request: &[u8]) -> Option<Vec<u8>> {
        // The lag: the first read after a change's command passes it by; the second
        // makes it the answer from then on.
        if let Some(i) = self
            .pending
            .iter()
            .position(|p| p.read == read && p.key.matches(request))
        {
            if self.pending[i].reads == 0 {
                self.pending[i].reads = 1;
            } else {
                let p = self.pending.remove(i);
                self.script.retain(|s| !(s.read == read && s.key == p.key));
                self.script.push(Scripted {
                    read,
                    key: p.key,
                    answer: Answer::Fixed(p.reply),
                });
            }
        }
        let best = self
            .script
            .iter_mut()
            .filter(|s| s.read == read && s.key.matches(request))
            .max_by_key(|s| s.key.sub.is_some())?;
        Some(match &mut best.answer {
            Answer::Fixed(reply) => reply.clone(),
            Answer::With(make) => make(request),
        })
    }
}

/// Copy `reply` into `buf` from `at`, zero the rest of a `size`-byte report, and say how
/// long the report is.
fn fill(buf: &mut [u8], at: usize, reply: &[u8], size: usize) -> usize {
    let len = size.min(buf.len());
    buf[at..len].fill(0);
    let n = reply.len().min(len - at);
    buf[at..at + n].copy_from_slice(&reply[..n]);
    len
}

impl Transport for FakeTransport {
    fn send_feature(&self, report: &[u8]) -> Result<()> {
        self.lock().record(Kind::Feature, report)
    }

    fn get_feature(&self, buf: &mut [u8]) -> Result<usize> {
        self.count_read()?;
        self.feature_reply(buf)
    }

    fn write(&self, report: &[u8]) -> Result<()> {
        self.lock().record(Kind::Output, report)
    }

    fn read_timeout(&self, buf: &mut [u8], _ms: i32) -> Result<usize> {
        self.count_read()?;
        Ok(self.input_reply(buf))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn feature_read(fake: &FakeTransport) -> Vec<u8> {
        let mut buf = vec![0u8; proto::FEATURE_REPORT];
        assert_eq!(fake.get_feature(&mut buf).unwrap(), proto::FEATURE_REPORT);
        buf
    }

    #[test]
    fn every_report_is_kept_in_order_with_its_kind() {
        let fake = FakeTransport::new();
        fake.send_feature(&[0, 0x4B]).unwrap();
        fake.write(&[0, 0x41]).unwrap();
        fake.send_feature(&[0, 0x40, 1]).unwrap();
        let sent = |kind, report: &[u8]| Sent {
            kind,
            report: report.to_vec(),
        };
        assert_eq!(
            fake.sent(),
            [
                sent(Kind::Feature, &[0, 0x4B]),
                sent(Kind::Output, &[0, 0x41]),
                sent(Kind::Feature, &[0, 0x40, 1]),
            ]
        );
        fake.clear_sent();
        assert!(fake.sent().is_empty());
    }

    #[test]
    fn a_feature_read_answers_the_last_feature_report_by_opcode_then_sub_byte() {
        let fake = FakeTransport::new();
        fake.answer_feature(Key::op(0x83), &[0x83, 0x00, 0xAA])
            .answer_feature(Key::sub(0x83, 0x02), &[0x83, 0x00, 0xBB]);
        fake.send_feature(&[0, 0x83, 0x01]).unwrap();
        let read = feature_read(&fake);
        assert_eq!(read[..4], [0, 0x83, 0x00, 0xAA]);
        assert!(
            read[4..].iter().all(|&b| b == 0),
            "a whole report, zero-padded"
        );
        fake.send_feature(&[0, 0x83, 0x02]).unwrap();
        assert_eq!(
            feature_read(&fake)[..4],
            [0, 0x83, 0x00, 0xBB],
            "the sub-byte wins"
        );
        assert_eq!(
            feature_read(&fake)[..4],
            [0, 0x83, 0x00, 0xBB],
            "the slot keeps its answer"
        );
        fake.write(&[0, 0x41]).unwrap();
        assert_eq!(
            feature_read(&fake)[..4],
            [0, 0x83, 0x00, 0xBB],
            "an output report doesn't change the feature slot"
        );
        fake.send_feature(&[0, 0x40, 0x01]).unwrap();
        assert_eq!(
            feature_read(&fake)[..3],
            [0, 0x40, 0x00],
            "unscripted: the bare acknowledgement"
        );
        let mut wrong = vec![1u8; proto::FEATURE_REPORT];
        assert!(
            fake.get_feature(&mut wrong).is_err(),
            "report id 1 in buf[0]"
        );
    }

    #[test]
    fn an_input_report_answers_its_query_once_and_an_unscripted_one_times_out() {
        let fake = FakeTransport::new();
        fake.answer_input(Key::op(0xF5), &[0xF5, 0x00, 0x01]);
        let mut buf = [0xEEu8; proto::INPUT_REPORT];
        assert_eq!(
            fake.read_timeout(&mut buf, 10).unwrap(),
            0,
            "nothing asked yet"
        );
        fake.write(&[0, 0xF5]).unwrap();
        assert_eq!(
            fake.read_timeout(&mut buf, 10).unwrap(),
            proto::INPUT_REPORT
        );
        assert_eq!(buf[..4], [0xF5, 0x00, 0x01, 0x00]);
        assert_eq!(
            fake.read_timeout(&mut buf, 10).unwrap(),
            0,
            "one answer a query"
        );
        fake.write(&[0, 0x41]).unwrap();
        assert_eq!(
            fake.read_timeout(&mut buf, 10).unwrap(),
            0,
            "the hand-back has none"
        );
        fake.send_feature(&[0, 0xF5]).unwrap();
        assert_eq!(
            fake.read_timeout(&mut buf, 10).unwrap(),
            0,
            "a feature report isn't answered with an input report"
        );
    }

    #[test]
    fn a_reply_can_be_made_from_the_request() {
        let fake = FakeTransport::new();
        // Echo a read request's offset back, as a fake flash would use it.
        fake.answer_feature_with(Key::sub(0x83, 0x01), |req| vec![0x83, 0x00, req[6], req[7]]);
        fake.send_feature(&[0, 0x83, 0x01, 0x84, 0xF4, 0x01, 0xE8, 0x03, 0, 0])
            .unwrap();
        assert_eq!(feature_read(&fake)[..5], [0, 0x83, 0x00, 0xE8, 0x03]);
        fake.answer_input_with(Key::op(0xB1), |req| vec![0xB1, req[2]]);
        fake.write(&[0, 0xB1, 0x04]).unwrap();
        let mut buf = [0u8; proto::INPUT_REPORT];
        fake.read_timeout(&mut buf, 10).unwrap();
        assert_eq!(buf[..2], [0xB1, 0x04]);
    }

    #[test]
    fn the_nth_send_fails_and_is_not_kept() {
        let fake = FakeTransport::new();
        fake.fail_send(2);
        fake.send_feature(&[0, 0x4B]).unwrap();
        assert!(fake.write(&[0, 0x41]).is_err());
        fake.write(&[0, 0x41]).unwrap();
        let ops: Vec<(Kind, u8)> = fake.sent().iter().map(|s| (s.kind, s.report[1])).collect();
        assert_eq!(ops, [(Kind::Feature, 0x4B), (Kind::Output, 0x41)]);
    }

    #[test]
    fn sends_and_reads_in_a_range_fail() {
        let fake = FakeTransport::new();
        fake.fail_sends(2..4).fail_reads(2..);
        fake.write(&[0, 0x41]).unwrap();
        assert!(fake.write(&[0, 0x41]).is_err());
        assert!(fake.write(&[0, 0x41]).is_err());
        fake.write(&[0, 0x41]).unwrap();
        assert_eq!(fake.sent().len(), 2);
        let mut buf = [0u8; proto::INPUT_REPORT];
        fake.read_timeout(&mut buf, 0).unwrap();
        assert!(fake.read_timeout(&mut buf, 0).is_err());
        let mut feature = vec![0u8; proto::FEATURE_REPORT];
        assert!(
            fake.get_feature(&mut feature).is_err(),
            "reads count together"
        );
    }

    #[test]
    fn queued_input_reports_come_before_any_answer() {
        let fake = FakeTransport::new();
        fake.answer_input(Key::op(0x02), &[0x02, 0x00])
            .queue_input(&[0x90, 0x00])
            .queue_input_after(Key::op(0x02), &[0xB1, 0x00]);
        assert_eq!(fake.queued_input(), 1);
        fake.write(&[0, 0x02, 0x01, 0x84]).unwrap();
        assert_eq!(
            fake.queued_input(),
            2,
            "the late one came in with the erase"
        );
        let mut buf = [0u8; proto::INPUT_REPORT];
        let mut next = || {
            let n = fake.read_timeout(&mut buf, 10).unwrap();
            buf[..n.min(2)].to_vec()
        };
        assert_eq!(next(), [0x90, 0x00], "the oldest first");
        assert_eq!(next(), [0xB1, 0x00]);
        assert_eq!(next(), [0x02, 0x00], "then the erase's own answer");
        assert_eq!(next(), Vec::<u8>::new());
        fake.write(&[0, 0x02, 0x01, 0x84]).unwrap();
        assert_eq!(fake.queued_input(), 0, "once");
    }

    #[test]
    fn a_change_shows_only_from_the_second_read_after_its_command() {
        let fake = FakeTransport::new();
        fake.answer_feature(Key::op(0xB6), b"\xB6\x00old")
            .change_after(Key::op(0x36), Read::Feature, Key::op(0xB6), b"\xB6\x00new");
        let read = |fake: &FakeTransport| {
            fake.send_feature(&[0, 0xB6, 0x00, 0x01]).unwrap();
            feature_read(fake)[1..6].to_vec()
        };
        assert_eq!(read(&fake), b"\xB6\x00old", "before the write");
        fake.send_feature(&[0, 0x36, 0x00, 0x01]).unwrap();
        assert_eq!(read(&fake), b"\xB6\x00old", "the first read after it lags");
        assert_eq!(read(&fake), b"\xB6\x00new");
        assert_eq!(read(&fake), b"\xB6\x00new", "and the change stays");

        // The same for an input report: the erase's answer, from the second query on.
        fake.answer_input(Key::op(0xB1), &[0xB1, 0x01])
            .change_after(Key::op(0x02), Read::Input, Key::op(0xB1), &[0xB1, 0x00]);
        let validate = |fake: &FakeTransport| {
            fake.write(&[0, 0xB1, 0x04]).unwrap();
            let mut buf = [0u8; proto::INPUT_REPORT];
            fake.read_timeout(&mut buf, 10).unwrap();
            buf[1]
        };
        fake.write(&[0, 0x02, 0x01, 0x84]).unwrap();
        assert_eq!(validate(&fake), 0x01, "still the old answer");
        assert_eq!(validate(&fake), 0x00);
    }
}
