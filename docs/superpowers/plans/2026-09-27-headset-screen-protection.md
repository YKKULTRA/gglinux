# Base-station screen burn-in protection — Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Keep the Arctis Nova Pro Wireless base-station OLED dark by default, lighting it for a few seconds only when something real happens.

**Architecture:** `gg-device-headset` gains the OLED commands and an event-preserving `poll()`. A new `gglinuxd` headset worker thread owns the device, polls it every second, and drives a pure screen-policy state machine (`headset/screen.rs`) whose `Blank`/`ShowStock` actions it carries out. New IPC methods expose battery and screen settings to a `HeadsetPanel` in the Svelte UI.

**Tech Stack:** Rust 2021 (hidapi 2, tokio 1, serde), Svelte 5 + TypeScript (Tauri 2 shell).

**Spec:** `docs/superpowers/specs/2026-09-27-headset-screen-protection-design.md`

## Global Constraints

- License GPL-3.0-or-later; MSRV `rust-version = "1.85"` (workspace `Cargo.toml`).
- Lit period `T`: default **10 s**, range **3..=600 s**. Brightness **1..=10** or unset; gglinux never pushes a default brightness.
- Status poll every **1 s**; safety re-blank every **10 min** while dark; reopen attempt every **2 s** while absent; shutdown waits at most **1 s**.
- Blank = one all-black `06 93` frame (two 1024-byte **feature** reports, strips x=0 and x=64, 64×64 each). Show = `06 95`. Never send `06 09` (save) from any of this.
- Firmware event = `07 25` report or a change in `06 20` byte 3. Every other `07 ..` report is an *our* event.
- Nothing about the base-station knob: no special handling (owner's decision).
- Default settings at daemon start: `protect: true, lit_seconds: 10, brightness: None`. Not persisted (config file is M7).

## Review Focus

1. **A late reply to a different request** (e.g. an `06 b0` answer arriving while waiting on `06 20`) must be dropped, never treated as an event that wakes the screen — test in Task 2 (`a_late_reply_to_another_request_is_dropped`).
2. **A client sending out-of-range settings** (lit 2 s, brightness 0 or 11) gets an error and the settings stay unchanged — test in Task 3 (`screen_settings_reject_out_of_range_values`); worker validates before applying (Task 6).
3. **Brightness 0 from any caller** is refused with an explanation, since it does not turn the screen off — test in Task 2 (`oled_brightness_is_one_to_ten`).
4. **The base station is replugged while the policy thinks it's dark** — the screen must come back to stock and blank again after `T` — test in Task 4 (`device_opened_always_shows_stock`).
5. **Protection disabled while lit** must not blank later and must not send a redundant Show — test in Task 4 (`disabling_while_lit_sends_nothing_and_never_blanks`).

---

### Task 1: Hardware gate — does polling relight a black screen?

The whole design detects firmware-initiated redraws by polling. If a `06 b0`/`06 20` query itself relights the panel, detection must fall back to interrupts only and the spec changes. Uses the throwaway spike tool (scratchpad, not in the repo).

**Files:** none.

- [ ] **Step 1: Blank, then poll for 60 s while a person watches the screen**

Run (spike tool path: `$SCRATCH/oledspike/target/debug/oledspike`):
```bash
S=$SCRATCH/oledspike/target/debug/oledspike
$S black 1 && $S poll 60 && $S ui 1
```
Expected: the screen goes black and **stays black for the full 60 s** of polling, then returns to stock.

- [ ] **Step 2: Decide**

If it stayed black: continue with Task 2 unchanged. If it relit: stop, record the finding in the spec's hardware table, and revise the detection design before continuing.

---

### Task 2: `gg-device-headset` — OLED commands, volume read, event-preserving poll

**Files:**
- Modify: `crates/gg-device-headset/src/proto.rs` (module doc, opcodes, new builders/decoders, tests)
- Modify: `crates/gg-device-headset/src/lib.rs` (replace `drain`/`recv`/`status_once`/`read_event` with `read_event_timeout`/`request`/`poll`; add OLED methods, `send_feature`, `Poll`)

**Interfaces:**
- Produces (proto): `REPORT_EVENT: u8`, `OLED_FRAME_PAYLOAD: usize`, `audio_settings_packet() -> Vec<u8>`, `decode_volume(&[u8]) -> Option<u8>`, `decode_volume_event(&[u8]) -> Option<u8>`, `oled_blank_packets() -> [Vec<u8>; 2]`, `oled_brightness_packet(u8) -> Result<Vec<u8>>`, `oled_return_to_ui_packet() -> Vec<u8>`, `is_event(&[u8]) -> bool`, `sort_report(Vec<u8>, Option<u8>, &mut Vec<Vec<u8>>) -> Option<Vec<u8>>`
- Produces (lib): `pub struct Poll { status: Option<Status>, volume: Option<u8>, events: Vec<Vec<u8>> }`, `Headset::poll(&self) -> Result<Poll>`, `Headset::read_event_timeout(&self, i32) -> Result<Option<Vec<u8>>>`, `Headset::oled_blank(&self) -> Result<()>`, `Headset::oled_show_stock(&self) -> Result<()>`, `Headset::set_oled_brightness(&self, u8) -> Result<()>`

(The spec's `classify(report, awaited_opcode)` is realised as `sort_report`, which also files events.)

- [ ] **Step 1: Write the failing tests** — append inside `mod tests` in `proto.rs`:

```rust
    #[test]
    fn oled_brightness_is_one_to_ten() {
        assert_eq!(oled_brightness_packet(1).unwrap(), vec![0x85, 0x01]);
        assert_eq!(oled_brightness_packet(10).unwrap(), vec![0x85, 0x0a]);
        assert!(oled_brightness_packet(0).is_err(), "0 does not turn the screen off");
        assert!(oled_brightness_packet(11).is_err());
        assert_eq!(oled_return_to_ui_packet(), vec![0x95]);
        assert_eq!(audio_settings_packet(), vec![0x20]);
    }

    /// Two 64-px strips; with the report id each is a 1024-byte feature report.
    #[test]
    fn oled_blank_is_two_all_black_strips() {
        let [left, right] = oled_blank_packets();
        for (strip, x) in [(&left, 0x00), (&right, 0x40)] {
            assert_eq!(strip.len(), OLED_FRAME_PAYLOAD);
            assert_eq!(&strip[..5], &[0x93, x, 0x00, 0x40, 0x40]);
            assert!(strip[5..].iter().all(|&b| b == 0), "every pixel off");
        }
        assert_eq!(OLED_FRAME_PAYLOAD + 1, 1024);
    }

    #[test]
    fn volume_decodes_from_both_the_reply_and_the_event() {
        // Captured on this unit 2026-09-27: 06 20 <input> <volume> ...
        let reply = [0x06, 0x20, 0x00, 0x06, 0x01, 0x00, 0x00, 0x14, 0x14];
        assert_eq!(decode_volume(&reply), Some(6));
        assert_eq!(decode_volume(&[0x06, 0x20, 0x00, 57]), None, "past silent is not a volume");
        assert_eq!(decode_volume(&[0x06, 0xb0, 0x00, 0x06]), None);
        assert_eq!(decode_volume_event(&[0x07, 0x25, 0x07]), Some(7));
        assert_eq!(decode_volume_event(&[0x06, 0x25, 0x07]), None, "a reply, not an event");
    }

    #[test]
    fn events_that_arrive_before_a_reply_are_kept() {
        let mut events = Vec::new();
        let wheel = vec![0x07, 0x25, 0x06];
        let status = vec![0x06, 0xb0, 0x00, 0x00, 0x01];
        assert_eq!(sort_report(wheel.clone(), Some(0xb0), &mut events), None);
        assert_eq!(sort_report(status.clone(), Some(0xb0), &mut events), Some(status));
        assert_eq!(events, vec![wheel]);
    }

    #[test]
    fn a_late_reply_to_another_request_is_dropped() {
        let mut events = Vec::new();
        let stale_status = vec![0x06, 0xb0, 0x00, 0x00, 0x01];
        assert_eq!(sort_report(stale_status, Some(0x20), &mut events), None);
        assert!(events.is_empty(), "a stale reply must not wake the screen");
    }

    #[test]
    fn queued_reports_sort_with_nothing_awaited() {
        let mut events = Vec::new();
        assert_eq!(sort_report(vec![0x06, 0x20, 0x00, 0x06], None, &mut events), None);
        assert_eq!(sort_report(vec![0x07, 0xbd, 0x01], None, &mut events), None);
        assert_eq!(sort_report(vec![0x07], None, &mut events), None, "too short to be an event");
        assert_eq!(events, vec![vec![0x07, 0xbd, 0x01]]);
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p gg-device-headset`
Expected: compile errors — `oled_brightness_packet`, `sort_report`, etc. not found.

- [ ] **Step 3: Implement in `proto.rs`**

Replace the paragraph in the module doc that begins "**Replies come back on report `0x06` too**" through "…as whatever reply was being waited for." with:

```rust
//! **Replies come back on report `0x06` too**, echoing the opcode, which is not
//! what the reference implementations describe (they document `07 b0`). Report
//! `0x07` carries the *unsolicited* stream instead: `07 25 <attenuation>` whenever
//! the volume wheel moves — and only then — plus other device-side changes. The
//! steady ~16 ms `07 25` stream seen on 2026-08-12 was a wheel in motion, not a
//! periodic report. Verified on hardware; see `docs/PROTOCOLS.md`.
//!
//! Decoding therefore keys on the opcode rather than the report id, and accepts
//! either id: matching on arrival order instead would decode an ambient interrupt
//! as whatever reply was being waited for.
```

Add to the opcode list (keep alphabetical-by-value order as the file does):

```rust
const CMD_AUDIO_SETTINGS: u8 = 0x20;
const CMD_OLED_BRIGHTNESS: u8 = 0x85;
const CMD_OLED_FRAME: u8 = 0x93;
const CMD_OLED_RETURN_TO_UI: u8 = 0x95;
```

After `const BATTERY_STEPS`, add:

```rust
/// Report id of the unsolicited event stream (commands and replies use `0x06`).
pub const REPORT_EVENT: u8 = 0x07;

/// Bytes in one OLED frame report after its report id. Frame reports are
/// 1024-byte *feature* reports, unlike the 64-byte output reports commands use.
pub const OLED_FRAME_PAYLOAD: usize = 1023;

/// Widest strip one frame report can carry, and the panel height.
const OLED_STRIP_WIDTH: u8 = 64;
const OLED_HEIGHT: u8 = 64;

/// OLED brightness range. There is no "off": 0 is refused by ggoled and behaves
/// like 1 on this unit (verified 2026-09-27) — blank with a black frame instead.
pub const OLED_BRIGHTNESS_MIN: u8 = 1;
pub const OLED_BRIGHTNESS_MAX: u8 = 10;
```

After `volume_packet`, add:

```rust
/// Ask for the audio settings, whose reply carries the master volume.
pub fn audio_settings_packet() -> Vec<u8> {
    vec![CMD_AUDIO_SETTINGS]
}

/// Master volume from a `06 20` reply, as attenuation (0 loudest, 56 silent).
///
/// Byte 2 is the audio input; byte 3 is the volume, on the same scale as the
/// `07 25` event, so it is known before the wheel first moves.
pub fn decode_volume(report: &[u8]) -> Option<u8> {
    match report {
        [_, CMD_AUDIO_SETTINGS, _, atten, ..] if *atten <= VOLUME_ATTEN_MAX => Some(*atten),
        _ => None,
    }
}

/// The volume carried by a `07 25 <attenuation>` event.
pub fn decode_volume_event(report: &[u8]) -> Option<u8> {
    match report {
        [REPORT_EVENT, CMD_VOLUME, atten, ..] => Some(*atten),
        _ => None,
    }
}

/// OLED brightness, [`OLED_BRIGHTNESS_MIN`]..=[`OLED_BRIGHTNESS_MAX`].
pub fn oled_brightness_packet(level: u8) -> Result<Vec<u8>> {
    ensure!(
        (OLED_BRIGHTNESS_MIN..=OLED_BRIGHTNESS_MAX).contains(&level),
        "screen brightness {level} out of range ({OLED_BRIGHTNESS_MIN}..={OLED_BRIGHTNESS_MAX}) \
         — the lowest level is still lit, so there is no brightness that turns it off"
    );
    Ok(vec![CMD_OLED_BRIGHTNESS, level])
}

/// Hand the screen back to the firmware's own display.
pub fn oled_return_to_ui_packet() -> Vec<u8> {
    vec![CMD_OLED_RETURN_TO_UI]
}

/// The two strips of an all-black frame, each the payload of one frame report.
///
/// Frames go out in strips at most 64 px wide — `93 <x> 00 <width> <height>`, then
/// the pixels column-major, LSB at the top — and here every pixel is off, which is
/// the point: an unlit OLED pixel does not wear. One frame holds until something
/// makes the firmware redraw (a volume change does).
pub fn oled_blank_packets() -> [Vec<u8>; 2] {
    [0, OLED_STRIP_WIDTH].map(|x| {
        let mut packet = vec![0u8; OLED_FRAME_PAYLOAD];
        packet[..5].copy_from_slice(&[CMD_OLED_FRAME, x, 0, OLED_STRIP_WIDTH, OLED_HEIGHT]);
        packet
    })
}

/// Whether a report came from the unsolicited event stream.
pub fn is_event(report: &[u8]) -> bool {
    report.len() >= 2 && report[0] == REPORT_EVENT
}

/// Sort one report read while waiting for the reply to `awaiting`.
///
/// Returns the report if it is that reply. Otherwise an event is appended to
/// `events` and anything else — a late reply to an earlier request — is dropped,
/// so it can neither be mistaken for this reply nor for something that happened
/// on the device.
pub fn sort_report(
    report: Vec<u8>,
    awaiting: Option<u8>,
    events: &mut Vec<Vec<u8>>,
) -> Option<Vec<u8>> {
    if awaiting.is_some() && report.get(1) == awaiting.as_ref() {
        return Some(report);
    }
    if is_event(&report) {
        events.push(report);
    }
    None
}
```

- [ ] **Step 4: Implement in `lib.rs`**

Update the `pub use` line to also export the new item used by callers:

```rust
pub use proto::{AutoOff, PowerStatus, Status, WirelessMode, EQ_BANDS, EQ_CUSTOM_PRESET};
```
(unchanged — `Poll` is defined in `lib.rs` itself).

Replace the `REPLY_DEADLINE` doc comment with:

```rust
/// How long to keep reading while hunting for a command's reply.
///
/// Unsolicited reports — a turning volume wheel sends one every ~16 ms — can land
/// ahead of the reply, so it is not reliably the first report to arrive. This has
/// to be a deadline rather than a count of reports: a fixed budget is spent by
/// whatever the stream happens to be emitting, which makes the read succeed or
/// fail depending on timing.
```

Add after the `Headset` struct definition:

```rust
/// One polling round: see [`Headset::poll`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Poll {
    /// `None` if the base station did not answer in time.
    pub status: Option<Status>,
    /// Master volume as attenuation, 0 loudest; `None` if unanswered.
    pub volume: Option<u8>,
    /// Unsolicited `07 ..` reports read along the way, oldest first.
    pub events: Vec<Vec<u8>>,
}
```

Replace everything from the `drain` method's doc comment through the end of `read_event` (i.e. `drain`, `recv`, `status`, `status_once`, `read_event`) with:

```rust
    /// Send a frame payload as a feature report. Frames are 1024 bytes, far past
    /// the 64-byte output reports [`Headset::send`] writes.
    fn send_feature(&self, payload: &[u8]) -> Result<()> {
        let mut buf = Vec::with_capacity(payload.len() + 1);
        buf.push(REPORT_TX);
        buf.extend_from_slice(payload);
        self.device
            .send_feature_report(&buf)
            .with_context(|| format!("writing frame command 0x{:02X}", payload[0]))
    }

    /// Read one input report, waiting at most `timeout_ms`. `None` on timeout.
    pub fn read_event_timeout(&self, timeout_ms: i32) -> Result<Option<Vec<u8>>> {
        let mut buf = [0u8; READ_SIZE];
        let n = self
            .device
            .read_timeout(&mut buf, timeout_ms)
            .context("reading from the base station")?;
        Ok((n > 0).then(|| buf[..n].to_vec()))
    }

    /// Read one input report without sending anything, for the event stream
    /// (volume, ANC changes made on the device itself).
    pub fn read_event(&self) -> Result<Option<Vec<u8>>> {
        self.read_event_timeout(READ_TIMEOUT_MS)
    }

    /// Query the base station: battery for both packs, power state, and the
    /// settings it reports back. `None` if it never answered.
    pub fn status(&self) -> Result<Option<Status>> {
        let reply = self.request(&proto::status_packet(), &mut Vec::new())?;
        Ok(reply.and_then(|r| Status::decode(&r)))
    }

    /// Status and volume in one round, plus every unsolicited report that arrived
    /// meanwhile — which [`Headset::status`] throws away, and a caller watching the
    /// device cannot afford to lose.
    pub fn poll(&self) -> Result<Poll> {
        let mut events = Vec::new();
        let status = self
            .request(&proto::status_packet(), &mut events)?
            .and_then(|r| Status::decode(&r));
        let volume = self
            .request(&proto::audio_settings_packet(), &mut events)?
            .and_then(|r| proto::decode_volume(&r));
        Ok(Poll { status, volume, events })
    }

    /// Send a request and return its reply, filing events that arrive around it.
    ///
    /// The protocol carries no correlation id, so a reply is recognised by the
    /// opcode it echoes. Reports already queued predate the request and are sorted
    /// first, so a leftover reply to an earlier exchange is never taken for this
    /// one. Nothing acknowledges a write, so a request that drew no reply is sent
    /// again: reading longer cannot recover one the base station never acted on.
    fn request(&self, payload: &[u8], events: &mut Vec<Vec<u8>>) -> Result<Option<Vec<u8>>> {
        let opcode = payload[0];
        for attempt in 1..=REQUEST_ATTEMPTS {
            for _ in 0..MAX_DRAIN_REPORTS {
                match self.read_event_timeout(0)? {
                    Some(report) => {
                        proto::sort_report(report, None, events);
                    }
                    None => break,
                }
            }
            self.send(payload)?;

            let deadline = Instant::now() + REPLY_DEADLINE;
            let mut seen = 0usize;
            while Instant::now() < deadline && seen < MAX_REPLY_REPORTS {
                let Some(report) = self.read_event_timeout(READ_TIMEOUT_MS)? else { continue };
                seen += 1;
                if let Some(reply) = proto::sort_report(report, Some(opcode), events) {
                    if attempt > 1 {
                        debug!(attempt, opcode, "request needed a retry");
                    }
                    return Ok(Some(reply));
                }
            }
        }
        debug!(attempts = REQUEST_ATTEMPTS, opcode, "no reply");
        Ok(None)
    }
```

Rename the doc of `MAX_DRAIN_REPORTS` to: `/// Upper bound on queued reports sorted before each request.`

Append to the `impl Headset` block, after `set_volume`:

```rust
    /// Blank the screen with an all-black frame. Brightness cannot do this — its
    /// floor is still lit — and an unlit OLED pixel does not wear.
    pub fn oled_blank(&self) -> Result<()> {
        for strip in proto::oled_blank_packets() {
            self.send_feature(&strip)?;
        }
        Ok(())
    }

    /// Hand the screen back to the firmware's own display.
    pub fn oled_show_stock(&self) -> Result<()> {
        self.send(&proto::oled_return_to_ui_packet())
    }

    /// OLED brightness, 1..=10. Not readable back, so callers track what they set.
    pub fn set_oled_brightness(&self, level: u8) -> Result<()> {
        self.send(&proto::oled_brightness_packet(level)?)
    }
```

- [ ] **Step 5: Run tests and build examples**

Run: `cargo test -p gg-device-headset && cargo build -p gg-device-headset --examples`
Expected: all tests pass (12 existing + 6 new); examples build (they call `status()`/`read_event()`, whose signatures are unchanged).

- [ ] **Step 6: Commit**

```bash
git add crates/gg-device-headset
git commit -m "Headset: OLED blank/show/brightness, volume read, event-preserving poll"
```

---

### Task 3: `gg-ipc` — headset state, screen settings, requests, event

**Files:**
- Modify: `crates/gg-ipc/src/lib.rs`
- Modify: `crates/gglinuxd/examples/fakemouse.rs` (exhaustive `Request` matches)

**Interfaces:**
- Produces: `ScreenSettings { protect: bool, lit_seconds: u16, brightness: Option<u8> }` (`Copy`, `Default`, `validate() -> Result<(), String>`, consts `LIT_SECONDS`, `BRIGHTNESS`), `HeadsetPower { Offline, Charging, Online, Unknown }`, `HeadsetState { connected, power, headset_battery: Option<u8>, spare_battery: Option<u8>, screen: ScreenSettings, screen_dark: bool }`, `Request::GetHeadsetState`, `Request::SetHeadsetScreen { settings }`, `ResponseData::Headset { state }`, `Event::HeadsetChanged { state }`

- [ ] **Step 1: Write the failing tests** — append inside `mod tests`:

```rust
    #[test]
    fn headset_requests_parse_from_their_documented_json() {
        let set = r#"{"id":5,"req":{"method":"set_headset_screen","params":
            {"settings":{"protect":true,"lit_seconds":10,"brightness":null}}}}"#;
        match serde_json::from_str::<RequestFrame>(set).unwrap().req {
            Request::SetHeadsetScreen { settings } => assert_eq!(settings, ScreenSettings::default()),
            other => panic!("parsed as {other:?}"),
        }
        serde_json::from_str::<RequestFrame>(r#"{"id":6,"req":{"method":"get_headset_state"}}"#)
            .expect("get_headset_state should parse");
    }

    #[test]
    fn headset_state_serializes_for_the_ui() {
        let state = HeadsetState {
            connected: true,
            power: HeadsetPower::Online,
            headset_battery: Some(12),
            spare_battery: None,
            screen: ScreenSettings::default(),
            screen_dark: true,
        };
        let json = serde_json::to_string(&ServerMessage::notify(Event::HeadsetChanged { state })).unwrap();
        assert!(json.contains(r#""event":"headset_changed""#), "{json}");
        assert!(json.contains(r#""power":"online""#), "{json}");
        assert!(json.contains(r#""spare_battery":null"#), "{json}");
        assert!(json.contains(r#""screen_dark":true"#), "{json}");
        assert!(json.contains(r#""lit_seconds":10"#), "{json}");
    }

    #[test]
    fn screen_settings_reject_out_of_range_values() {
        let ok = ScreenSettings::default();
        assert!(ok.validate().is_ok());
        assert!(ScreenSettings { lit_seconds: 2, ..ok }.validate().is_err());
        assert!(ScreenSettings { lit_seconds: 601, ..ok }.validate().is_err());
        assert!(ScreenSettings { brightness: Some(0), ..ok }.validate().is_err(), "0 is not off");
        assert!(ScreenSettings { brightness: Some(11), ..ok }.validate().is_err());
        assert!(ScreenSettings { brightness: Some(1), lit_seconds: 600, ..ok }.validate().is_ok());
    }
```

- [ ] **Step 2: Run to verify they fail**

Run: `cargo test -p gg-ipc`
Expected: compile errors — `ScreenSettings`, `HeadsetState` not found.

- [ ] **Step 3: Implement** — in `lib.rs`, after `MouseState`:

```rust
/// Burn-in protection for the headset base station's screen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenSettings {
    /// Keep the screen dark, lighting it briefly after events.
    pub protect: bool,
    /// How long the screen stays lit after an event.
    pub lit_seconds: u16,
    /// OLED brightness, or `None` to leave the device's own level alone — it
    /// cannot be read back, so gglinux never assumes one.
    pub brightness: Option<u8>,
}

impl ScreenSettings {
    pub const LIT_SECONDS: std::ops::RangeInclusive<u16> = 3..=600;
    /// There is no "off" level; see `gg_device_headset::proto::OLED_BRIGHTNESS_MIN`.
    pub const BRIGHTNESS: std::ops::RangeInclusive<u8> = 1..=10;

    /// Check ranges, with a message meant for the user.
    pub fn validate(&self) -> Result<(), String> {
        if !Self::LIT_SECONDS.contains(&self.lit_seconds) {
            return Err(format!(
                "screen lit period {} s out of range ({}..={})",
                self.lit_seconds,
                Self::LIT_SECONDS.start(),
                Self::LIT_SECONDS.end()
            ));
        }
        if let Some(level) = self.brightness {
            if !Self::BRIGHTNESS.contains(&level) {
                return Err(format!(
                    "screen brightness {level} out of range ({}..={}) — the lowest level is \
                     still lit, so no brightness turns the screen off",
                    Self::BRIGHTNESS.start(),
                    Self::BRIGHTNESS.end()
                ));
            }
        }
        Ok(())
    }
}

impl Default for ScreenSettings {
    fn default() -> Self {
        ScreenSettings { protect: true, lit_seconds: 10, brightness: None }
    }
}

/// Where the headset is, as the base station reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HeadsetPower {
    Offline,
    Charging,
    Online,
    /// Not reported yet, or a value we have not seen.
    Unknown,
}

/// What the daemon knows about the headset and its base station.
///
/// Unlike [`MouseState`], these are readings: the base station reports them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HeadsetState {
    /// Whether the base station is plugged in and open.
    pub connected: bool,
    pub power: HeadsetPower,
    /// Percent; `None` while the headset is off.
    pub headset_battery: Option<u8>,
    /// Percent; `None` when no spare pack is seated.
    pub spare_battery: Option<u8>,
    pub screen: ScreenSettings,
    /// Whether gglinux has the screen blanked right now.
    pub screen_dark: bool,
}
```

Add to `Request`, after `SaveMouse`:

```rust
    /// Headset battery, power, and screen settings.
    GetHeadsetState,
    /// Replace the base-station screen settings.
    SetHeadsetScreen { settings: ScreenSettings },
```

Add to `ResponseData`, after `Mouse`: `Headset { state: HeadsetState },`

Add to `Event`, after `MouseBattery`:

```rust
    /// Headset state changed: battery, power, connection, or screen.
    HeadsetChanged { state: HeadsetState },
```

- [ ] **Step 4: Keep `fakemouse` compiling** — in `crates/gglinuxd/examples/fakemouse.rs`:

Extend the `gg_ipc` import with `HeadsetPower, HeadsetState, ScreenSettings`. Add a field to `Fake` after `applied`:

```rust
    /// Screen settings as last sent, echoed back unvalidated.
    screen: ScreenSettings,
```

In `main`, initialise it after `applied: None,`: `screen: ScreenSettings::default(),`

In `handle`, before the closing brace of `match req`:

```rust
        Request::GetHeadsetState => ResponseData::Headset { state: headset_state(&s) },
        Request::SetHeadsetScreen { settings } => {
            s.screen = settings;
            ResponseData::Success
        }
```

After `mouse_state`, add:

```rust
fn headset_state(s: &Fake) -> HeadsetState {
    HeadsetState {
        connected: s.devices.iter().any(|d| d.kind == DeviceKind::Headset),
        power: HeadsetPower::Online,
        headset_battery: Some(75),
        spare_battery: Some(100),
        screen: s.screen,
        screen_dark: false,
    }
}
```

In `method_name`, add `Request::GetHeadsetState => "get_headset_state",` and `Request::SetHeadsetScreen { .. } => "set_headset_screen",`. In `describe`, before `other =>`, add:

```rust
        Request::SetHeadsetScreen { settings } => format!(
            "set_headset_screen protect={} lit={}s brightness={:?}",
            settings.protect, settings.lit_seconds, settings.brightness
        ),
```

- [ ] **Step 5: Keep `gglinuxd` compiling** — its `dispatch` match is exhaustive. In `crates/gglinuxd/src/main.rs`, add after the `Request::SaveMouse` arm a temporary arm that Task 6 replaces:

```rust
        Request::GetHeadsetState | Request::SetHeadsetScreen { .. } => ResponseData::Error {
            message: "headset support is not wired up yet".into(),
        },
```

- [ ] **Step 6: Run tests and build**

Run: `cargo test -p gg-ipc && cargo build --workspace --examples`
Expected: gg-ipc tests pass (4 existing + 3 new); the whole workspace, examples included, builds.

- [ ] **Step 7: Commit**

```bash
git add crates/gg-ipc crates/gglinuxd/examples/fakemouse.rs crates/gglinuxd/src/main.rs
git commit -m "IPC: headset state, screen settings, and their requests"
```

---

### Task 4: Screen policy state machine

**Files:**
- Create: `crates/gglinuxd/src/headset.rs` (module shell for now: doc + `mod screen;`)
- Create: `crates/gglinuxd/src/headset/screen.rs`
- Modify: `crates/gglinuxd/src/main.rs` (add `mod headset;`)

**Interfaces:**
- Consumes: `gg_ipc::ScreenSettings`
- Produces: `screen::Input { DeviceOpened, OurEvent, FirmwareEvent, Tick, Shutdown }`, `screen::Action { Blank, ShowStock }`, `screen::Screen::new(&ScreenSettings, Instant) -> Screen`, `Screen::on(&mut self, Input, Instant) -> Option<Action>`, `Screen::apply(&mut self, &ScreenSettings, Instant) -> Option<Action>`, `Screen::is_dark(&self) -> bool`, `screen::SAFETY_REBLANK: Duration`

- [ ] **Step 1: Create the module shell** — `crates/gglinuxd/src/headset.rs`:

```rust
//! Headset worker: the Arctis Nova Pro Wireless base station.

mod screen;
```

and in `main.rs`, after `mod mouse;`: `mod headset;`

- [ ] **Step 2: Write the failing tests** — `crates/gglinuxd/src/headset/screen.rs` containing only the test module for now:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const T: Duration = Duration::from_secs(10);

    fn secs(n: u64) -> Duration {
        Duration::from_secs(n)
    }

    fn on() -> ScreenSettings {
        ScreenSettings::default()
    }

    fn off() -> ScreenSettings {
        ScreenSettings { protect: false, ..ScreenSettings::default() }
    }

    /// A screen that has just gone dark at `t0 + T`.
    fn dark(t0: Instant) -> Screen {
        let mut s = Screen::new(&on(), t0);
        assert_eq!(s.on(Input::Tick, t0 + T), Some(Action::Blank));
        s
    }

    #[test]
    fn starts_lit_then_blanks_after_the_lit_period() {
        let t0 = Instant::now();
        let mut s = Screen::new(&on(), t0);
        assert!(!s.is_dark());
        assert_eq!(s.on(Input::Tick, t0 + secs(9)), None);
        assert_eq!(s.on(Input::Tick, t0 + T), Some(Action::Blank));
        assert!(s.is_dark());
        assert_eq!(s.on(Input::Tick, t0 + T + secs(1)), None, "blank once, not every tick");
    }

    #[test]
    fn our_event_while_dark_shows_stock_then_blanks_again() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let t = t0 + secs(60);
        assert_eq!(s.on(Input::OurEvent, t), Some(Action::ShowStock));
        assert!(!s.is_dark());
        assert_eq!(s.on(Input::Tick, t + secs(9)), None);
        assert_eq!(s.on(Input::Tick, t + T), Some(Action::Blank));
    }

    #[test]
    fn our_event_while_lit_extends_without_resending() {
        let t0 = Instant::now();
        let mut s = Screen::new(&on(), t0);
        assert_eq!(s.on(Input::OurEvent, t0 + secs(5)), None);
        assert_eq!(s.on(Input::Tick, t0 + T), None);
        assert_eq!(s.on(Input::Tick, t0 + secs(15)), Some(Action::Blank));
    }

    #[test]
    fn firmware_event_relights_without_sending_anything() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let t = t0 + secs(60);
        assert_eq!(s.on(Input::FirmwareEvent, t), None, "the firmware already drew its popup");
        assert!(!s.is_dark());
        assert_eq!(s.on(Input::Tick, t + T), Some(Action::Blank));
    }

    #[test]
    fn repeated_firmware_events_blank_after_the_last_one() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let t = t0 + secs(60);
        s.on(Input::FirmwareEvent, t);
        s.on(Input::FirmwareEvent, t + secs(5));
        assert_eq!(s.on(Input::Tick, t + T), None);
        assert_eq!(s.on(Input::Tick, t + secs(15)), Some(Action::Blank));
    }

    #[test]
    fn dark_screen_is_reblanked_every_ten_minutes() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let went_dark = t0 + T;
        assert_eq!(s.on(Input::Tick, went_dark + SAFETY_REBLANK - secs(1)), None);
        assert_eq!(s.on(Input::Tick, went_dark + SAFETY_REBLANK), Some(Action::Blank));
        assert_eq!(s.on(Input::Tick, went_dark + SAFETY_REBLANK * 2 - secs(1)), None);
        assert_eq!(s.on(Input::Tick, went_dark + SAFETY_REBLANK * 2), Some(Action::Blank));
    }

    #[test]
    fn device_opened_always_shows_stock() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        assert_eq!(s.on(Input::DeviceOpened, t0 + secs(60)), Some(Action::ShowStock));
        let mut lit = Screen::new(&on(), t0);
        assert_eq!(lit.on(Input::DeviceOpened, t0 + secs(1)), Some(Action::ShowStock));
        assert_eq!(lit.on(Input::Tick, t0 + secs(1) + T), Some(Action::Blank));
    }

    #[test]
    fn shutdown_blanks_unless_disabled() {
        let t0 = Instant::now();
        let mut s = Screen::new(&on(), t0);
        assert_eq!(s.on(Input::Shutdown, t0 + secs(1)), Some(Action::Blank));
        let mut d = Screen::new(&off(), t0);
        assert_eq!(d.on(Input::Shutdown, t0 + secs(1)), None);
    }

    #[test]
    fn disabled_never_touches_the_screen() {
        let t0 = Instant::now();
        let mut s = Screen::new(&off(), t0);
        for input in [Input::Tick, Input::OurEvent, Input::FirmwareEvent, Input::DeviceOpened] {
            assert_eq!(s.on(input, t0 + secs(3600)), None, "{input:?}");
        }
        assert!(!s.is_dark());
    }

    #[test]
    fn disabling_while_dark_shows_stock() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        assert_eq!(s.apply(&off(), t0 + secs(60)), Some(Action::ShowStock));
        assert_eq!(s.on(Input::Tick, t0 + secs(3600)), None);
    }

    #[test]
    fn disabling_while_lit_sends_nothing_and_never_blanks() {
        let t0 = Instant::now();
        let mut s = Screen::new(&on(), t0);
        assert_eq!(s.apply(&off(), t0 + secs(1)), None, "the stock screen is already up");
        assert_eq!(s.on(Input::Tick, t0 + secs(3600)), None);
    }

    #[test]
    fn reenabling_lights_for_one_period_then_blanks() {
        let t0 = Instant::now();
        let mut s = Screen::new(&off(), t0);
        let t = t0 + secs(60);
        assert_eq!(s.apply(&on(), t), None);
        assert_eq!(s.on(Input::Tick, t + T), Some(Action::Blank));
    }

    #[test]
    fn a_settings_change_while_dark_is_shown_for_the_new_period() {
        let t0 = Instant::now();
        let mut s = dark(t0);
        let t = t0 + secs(60);
        let longer = ScreenSettings { lit_seconds: 30, ..on() };
        assert_eq!(s.apply(&longer, t), Some(Action::ShowStock));
        assert_eq!(s.on(Input::Tick, t + secs(29)), None);
        assert_eq!(s.on(Input::Tick, t + secs(30)), Some(Action::Blank));
    }
}
```

- [ ] **Step 3: Run to verify they fail**

Run: `cargo test -p gglinuxd screen`
Expected: compile errors — `Screen`, `Input`, `Action` not found.

- [ ] **Step 4: Implement** — prepend to `screen.rs`, above the tests:

```rust
//! When the base-station screen should be lit.
//!
//! Pure: no device access, and the clock is passed in, so every rule here is
//! unit-tested without hardware. The worker feeds it [`Input`]s and carries out
//! the [`Action`]s it returns.
//!
//! The screen cannot be read back, so this tracks what it last *made* it show,
//! and treats two kinds of event differently: ones the firmware already puts on
//! screen itself (a volume change), where it only starts the countdown, and ones
//! only gglinux sees, where it brings the stock screen back first.

use std::time::{Duration, Instant};

use gg_ipc::ScreenSettings;

/// While dark, re-send the black frame this often, in case the firmware put its
/// own screen back for a reason we cannot observe. A re-send can briefly flash
/// the stock screen (seen 2026-09-27 at 2 s intervals), so this is a backstop,
/// not a refresh.
pub const SAFETY_REBLANK: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Input {
    /// The base station was just opened: daemon start or replug.
    DeviceOpened,
    /// Something only gglinux knows about; the firmware shows nothing for it.
    OurEvent,
    /// Something the firmware already drew by itself (a volume change).
    FirmwareEvent,
    /// Time passed.
    Tick,
    /// The daemon is exiting.
    Shutdown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    /// Draw the black frame.
    Blank,
    /// Return to the firmware's own screen.
    ShowStock,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Mode {
    /// Showing the stock screen until `until`.
    Lit { until: Instant },
    /// Blanked at `last_blank` (the latest blank or re-blank).
    Dark { last_blank: Instant },
    /// Protection off: the firmware's screen, always.
    Disabled,
}

#[derive(Debug)]
pub struct Screen {
    mode: Mode,
    lit_for: Duration,
}

impl Screen {
    /// Starts lit: whatever is on screen now stays up for one lit period.
    pub fn new(settings: &ScreenSettings, now: Instant) -> Self {
        let lit_for = lit_duration(settings);
        let mode = if settings.protect {
            Mode::Lit { until: now + lit_for }
        } else {
            Mode::Disabled
        };
        Screen { mode, lit_for }
    }

    /// Whether the screen was last blanked (and not woken since).
    pub fn is_dark(&self) -> bool {
        matches!(self.mode, Mode::Dark { .. })
    }

    pub fn on(&mut self, input: Input, now: Instant) -> Option<Action> {
        match (self.mode, input) {
            (Mode::Disabled, _) => None,
            (_, Input::Shutdown) => self.blank(now),
            // After a (re)open the panel may be black from an earlier run or stock;
            // show stock either way so the countdown starts from a known state.
            (_, Input::DeviceOpened) | (Mode::Dark { .. }, Input::OurEvent) => {
                self.light(now);
                Some(Action::ShowStock)
            }
            (Mode::Lit { .. }, Input::OurEvent) | (_, Input::FirmwareEvent) => {
                self.light(now);
                None
            }
            (Mode::Lit { until }, Input::Tick) if now >= until => self.blank(now),
            (Mode::Dark { last_blank }, Input::Tick)
                if now.duration_since(last_blank) >= SAFETY_REBLANK =>
            {
                self.blank(now)
            }
            (_, Input::Tick) => None,
        }
    }

    /// Take new settings from a client.
    pub fn apply(&mut self, settings: &ScreenSettings, now: Instant) -> Option<Action> {
        self.lit_for = lit_duration(settings);
        if !settings.protect {
            let was_dark = self.is_dark();
            self.mode = Mode::Disabled;
            return was_dark.then_some(Action::ShowStock);
        }
        if self.mode == Mode::Disabled {
            // The stock screen is already up; count from now.
            self.light(now);
            return None;
        }
        // A change made from gglinux is an event in its own right, so its effect
        // (a new brightness) is visible before the screen goes dark again.
        self.on(Input::OurEvent, now)
    }

    fn light(&mut self, now: Instant) {
        self.mode = Mode::Lit { until: now + self.lit_for };
    }

    fn blank(&mut self, now: Instant) -> Option<Action> {
        self.mode = Mode::Dark { last_blank: now };
        Some(Action::Blank)
    }
}

fn lit_duration(settings: &ScreenSettings) -> Duration {
    Duration::from_secs(settings.lit_seconds.into())
}
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p gglinuxd screen`
Expected: 13 tests pass. (Dead-code warnings for `Screen` are expected until Task 6 uses it.)

- [ ] **Step 6: Commit**

```bash
git add crates/gglinuxd/src/headset.rs crates/gglinuxd/src/headset/screen.rs crates/gglinuxd/src/main.rs
git commit -m "Screen policy: dark by default, lit briefly after events"
```

---

### Task 5: `wake_reason` — which status changes light the screen (owner-written)

Learning-mode contribution: the owner writes the function body; the tests below pin the rules the spec fixed. The owner decides what the tests leave open (e.g. whether a battery reading that flickers across a step should wake the screen twice).

**Files:**
- Create: `crates/gglinuxd/src/headset/wake.rs`
- Modify: `crates/gglinuxd/src/headset.rs` (add `mod wake;`)

**Interfaces:**
- Consumes: `gg_device_headset::{Status, PowerStatus}`
- Produces: `wake::WakeReason { Power, Battery, Spare, Controls }`, `wake::wake_reason(&Status, &Status) -> Option<WakeReason>`

- [ ] **Step 1: Create the file with signature, docs, and tests; body marked for the owner**

```rust
//! Which status changes are worth lighting the base-station screen for.
//!
//! The worker compares consecutive `06 b0` reads with [`wake_reason`]. A `Some`
//! lights the screen for one lit period; `None` leaves it as it is.

use gg_device_headset::Status;

/// Why a status change lights the screen. Only used for logging.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WakeReason {
    /// Headset turned on or off, or put on / taken off the charge cable.
    Power,
    /// Headset battery dropped a step while low.
    Battery,
    /// Spare pack inserted or removed.
    Spare,
    /// ANC, transparency, or mic mute changed.
    Controls,
}

/// Headset battery at or below this is "low": each drop lights the screen.
pub const LOW_BATTERY: u8 = 25;

/// Compare two consecutive status reads.
///
/// Battery is reported in eighths (0, 12, 25, 37, 50, 62, 75, 87, 100 %), and is
/// `None` while the headset is off.
pub fn wake_reason(prev: &Status, next: &Status) -> Option<WakeReason> {
    // TODO(owner): decide which changes wake the screen. See the tests below.
    let _ = (prev, next);
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use gg_device_headset::PowerStatus;

    fn status() -> Status {
        Status {
            headset_battery: Some(100),
            spare_battery: Some(100),
            power: PowerStatus::Online,
            auto_off: None,
            wireless_mode: None,
            mic_led_brightness: 10,
            mic_status_raw: 0,
            anc_raw: 2,
            transparency_raw: 8,
        }
    }

    #[test]
    fn identical_reads_do_not_wake() {
        assert_eq!(wake_reason(&status(), &status()), None);
    }

    #[test]
    fn power_changes_wake() {
        let off = Status { power: PowerStatus::Offline, headset_battery: None, ..status() };
        assert_eq!(wake_reason(&status(), &off), Some(WakeReason::Power));
        assert_eq!(wake_reason(&off, &status()), Some(WakeReason::Power));
        let cable = Status { power: PowerStatus::Charging, ..status() };
        assert_eq!(wake_reason(&status(), &cable), Some(WakeReason::Power));
    }

    #[test]
    fn battery_wakes_only_when_dropping_while_low() {
        let at = |pct| Status { headset_battery: Some(pct), ..status() };
        assert_eq!(wake_reason(&at(50), &at(37)), None, "not low yet");
        assert_eq!(wake_reason(&at(37), &at(25)), Some(WakeReason::Battery));
        assert_eq!(wake_reason(&at(25), &at(12)), Some(WakeReason::Battery));
        assert_eq!(wake_reason(&at(12), &at(25)), None, "rising is not news");
    }

    #[test]
    fn spare_pack_in_or_out_wakes() {
        let none = Status { spare_battery: None, ..status() };
        assert_eq!(wake_reason(&status(), &none), Some(WakeReason::Spare));
        assert_eq!(wake_reason(&none, &status()), Some(WakeReason::Spare));
        let lower = Status { spare_battery: Some(87), ..status() };
        assert_eq!(wake_reason(&status(), &lower), None, "a charging spare is not news");
    }

    #[test]
    fn control_changes_wake() {
        assert_eq!(wake_reason(&status(), &Status { anc_raw: 0, ..status() }), Some(WakeReason::Controls));
        assert_eq!(wake_reason(&status(), &Status { mic_status_raw: 1, ..status() }), Some(WakeReason::Controls));
        assert_eq!(wake_reason(&status(), &Status { transparency_raw: 5, ..status() }), Some(WakeReason::Controls));
    }
}
```

and in `headset.rs` add `mod wake;` after `mod screen;`.

- [ ] **Step 2: Run to verify the tests fail**

Run: `cargo test -p gglinuxd wake`
Expected: `identical_reads_do_not_wake` passes; the other four FAIL (`left: None`).

- [ ] **Step 3: Owner writes the body.** If the owner prefers not to, use this reference implementation:

```rust
pub fn wake_reason(prev: &Status, next: &Status) -> Option<WakeReason> {
    if prev.power != next.power {
        return Some(WakeReason::Power);
    }
    if prev.spare_battery.is_some() != next.spare_battery.is_some() {
        return Some(WakeReason::Spare);
    }
    if let (Some(before), Some(after)) = (prev.headset_battery, next.headset_battery) {
        if after < before && after <= LOW_BATTERY {
            return Some(WakeReason::Battery);
        }
    }
    let controls = |s: &Status| (s.anc_raw, s.transparency_raw, s.mic_status_raw);
    (controls(prev) != controls(next)).then_some(WakeReason::Controls)
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p gglinuxd wake`
Expected: 5 pass.

- [ ] **Step 5: Commit**

```bash
git add crates/gglinuxd/src/headset.rs crates/gglinuxd/src/headset/wake.rs
git commit -m "Wake rules: which headset status changes light the screen"
```

---

### Task 6: Headset worker and daemon wiring

**Files:**
- Create: `crates/gglinuxd/src/clock.rs`
- Modify: `crates/gglinuxd/src/headset.rs` (the worker)
- Modify: `crates/gglinuxd/src/mouse.rs` (use `clock::is_older_than`, drop its private copy)
- Modify: `crates/gglinuxd/src/main.rs` (spawn, dispatch, shutdown)
- Modify: `crates/gglinuxd/Cargo.toml` (depend on `gg-device-headset`)

**Interfaces:**
- Consumes: Task 2 `Headset::{open, poll, read_event_timeout, oled_blank, oled_show_stock, set_oled_brightness}`, `proto::{is_event, decode_volume_event}`; Task 3 IPC types; Task 4 `Screen`; Task 5 `wake_reason`
- Produces: `headset::spawn(broadcast::Sender<Event>) -> Handle`, `headset::Handle::send`, `headset::Command { GetState, SetScreen { settings, reply }, Shutdown }`, `clock::is_older_than(Option<Instant>, Duration) -> bool`

- [ ] **Step 1: Shared clock helper** — `crates/gglinuxd/src/clock.rs`:

```rust
//! Small timing helper shared by the device workers.

use std::time::{Duration, Instant};

/// Whether `at` is unset or longer ago than `age`.
pub fn is_older_than(at: Option<Instant>, age: Duration) -> bool {
    at.is_none_or(|t| t.elapsed() >= age)
}
```

In `mouse.rs`: delete the private `is_older_than` fn (and its doc comment) and add `use crate::clock::is_older_than;` to the imports. In `main.rs`: add `mod clock;` before `mod headset;`.

- [ ] **Step 2: Dependency** — `crates/gglinuxd/Cargo.toml`, after `gg-device-mouse`:

```toml
gg-device-headset = { path = "../gg-device-headset" }
```

- [ ] **Step 3: The worker** — replace `crates/gglinuxd/src/headset.rs` with:

```rust
//! Headset worker: the Arctis Nova Pro Wireless base station.
//!
//! Like the mouse, all device I/O happens on one dedicated OS thread, because
//! hidapi handles are neither `Send` nor `Sync` and every call blocks. Unlike the
//! mouse, the device stays open: volume changes arrive as unsolicited interrupts,
//! and the screen policy has to hear them.
//!
//! The thread runs the base-station screen's burn-in protection: it polls status
//! and volume every second, turns what changed into [`screen::Input`]s, and
//! carries out the [`screen::Action`]s that come back.

mod screen;
mod wake;

use std::sync::mpsc::{self, Receiver, Sender, TryRecvError};
use std::time::{Duration, Instant};

use gg_device_headset::{proto, Headset, PowerStatus, Status};
use gg_ipc::{Event, HeadsetPower, HeadsetState, ScreenSettings};
use hidapi::HidApi;
use tokio::sync::{broadcast, oneshot};
use tracing::{debug, error, info, warn};

use crate::clock::is_older_than;
use screen::{Action, Input, Screen};

/// How often to ask for status and volume while connected.
const POLL_INTERVAL: Duration = Duration::from_secs(1);

/// How often to look for the base station while it is absent.
const REOPEN_INTERVAL: Duration = Duration::from_secs(2);

/// Longest one loop iteration waits for an interrupt. Bounds command latency.
const EVENT_WAIT_MS: i32 = 100;

/// Reply channel for a command. `Err` carries a message meant for the user.
type Reply<T> = oneshot::Sender<Result<T, String>>;

pub enum Command {
    GetState(Reply<HeadsetState>),
    SetScreen { settings: ScreenSettings, reply: Reply<()> },
    /// Leave the screen dark and stop. Replies once the device has been told.
    Shutdown(Reply<()>),
}

/// Cloneable handle used by client tasks to reach the worker.
#[derive(Clone)]
pub struct Handle {
    tx: Sender<Command>,
}

impl Handle {
    /// Send a command and await its reply. A worker that has gone away reports
    /// as an error rather than hanging the client.
    pub async fn send<T>(&self, make: impl FnOnce(Reply<T>) -> Command) -> Result<T, String> {
        let (reply_tx, reply_rx) = oneshot::channel();
        self.tx
            .send(make(reply_tx))
            .map_err(|_| "headset worker is not running".to_string())?;
        reply_rx
            .await
            .map_err(|_| "headset worker dropped the request".to_string())?
    }
}

/// Start the worker thread and return a handle to it.
pub fn spawn(event_tx: broadcast::Sender<Event>) -> Handle {
    let (tx, rx) = mpsc::channel();
    std::thread::Builder::new()
        .name("gg-headset".into())
        .spawn(move || worker(rx, event_tx))
        .expect("spawn gg-headset thread");
    Handle { tx }
}

/// Everything the worker remembers between iterations.
struct State {
    api: HidApi,
    device: Option<Headset>,
    last_open_attempt: Option<Instant>,
    last_poll: Option<Instant>,
    /// Latest status and volume; cleared when the device goes away, so a
    /// reconnect sets a fresh baseline instead of waking on stale differences.
    status: Option<Status>,
    volume: Option<u8>,
    settings: ScreenSettings,
    screen: Screen,
    /// Last state sent to clients, so unchanged iterations stay quiet.
    published: Option<HeadsetState>,
}

fn worker(rx: Receiver<Command>, event_tx: broadcast::Sender<Event>) {
    let api = match HidApi::new() {
        Ok(api) => api,
        Err(e) => {
            error!(error = %e, "headset worker could not initialise hidapi");
            return;
        }
    };
    let settings = ScreenSettings::default();
    let mut state = State {
        api,
        device: None,
        last_open_attempt: None,
        last_poll: None,
        status: None,
        volume: None,
        settings,
        screen: Screen::new(&settings, Instant::now()),
        published: None,
    };

    loop {
        loop {
            match rx.try_recv() {
                Ok(command) => {
                    if !handle(command, &mut state, &event_tx) {
                        return;
                    }
                }
                Err(TryRecvError::Empty) => break,
                // Every handle dropped without a Shutdown: still leave it dark.
                Err(TryRecvError::Disconnected) => {
                    shutdown(&mut state);
                    return;
                }
            }
        }

        if state.device.is_none() {
            try_open(&mut state);
        }
        if state.device.is_some() {
            if let Some(report) = read_event(&mut state) {
                on_report(&mut state, &report);
            }
            if is_older_than(state.last_poll, POLL_INTERVAL) {
                poll(&mut state);
            }
        } else {
            // Nothing to wait on; don't spin while the base station is absent.
            std::thread::sleep(Duration::from_millis(EVENT_WAIT_MS as u64));
        }

        let action = state.screen.on(Input::Tick, Instant::now());
        perform(&mut state, action);
        publish(&mut state, &event_tx);
    }
}

/// Handle one client command. Returns `false` when the worker should stop.
fn handle(command: Command, state: &mut State, event_tx: &broadcast::Sender<Event>) -> bool {
    match command {
        Command::GetState(reply) => {
            let _ = reply.send(Ok(snapshot(state)));
        }
        Command::SetScreen { settings, reply } => {
            if let Err(message) = settings.validate() {
                let _ = reply.send(Err(message));
                return true;
            }
            let brightness_changed = settings.brightness != state.settings.brightness;
            state.settings = settings;
            if brightness_changed {
                apply_brightness(state);
            }
            let action = state.screen.apply(&settings, Instant::now());
            perform(state, action);
            // Stored either way: a disconnected base station gets them on reconnect.
            let _ = reply.send(Ok(()));
            publish(state, event_tx);
        }
        Command::Shutdown(reply) => {
            shutdown(state);
            let _ = reply.send(Ok(()));
            return false;
        }
    }
    true
}

/// Open the base station if it has appeared, at most every [`REOPEN_INTERVAL`].
fn try_open(state: &mut State) {
    if !is_older_than(state.last_open_attempt, REOPEN_INTERVAL) {
        return;
    }
    state.last_open_attempt = Some(Instant::now());
    if let Err(e) = state.api.refresh_devices() {
        debug!(error = %e, "could not refresh HID devices");
        return;
    }
    match Headset::open(&state.api) {
        Ok(headset) => {
            info!("base station connected");
            state.device = Some(headset);
            state.last_poll = None;
            // The device forgets brightness on power loss; re-apply what the user set.
            apply_brightness(state);
            let action = state.screen.on(Input::DeviceOpened, Instant::now());
            perform(state, action);
        }
        // Absent is the normal state for an unplugged base station: stay quiet.
        Err(e) => debug!(error = %format!("{e:#}"), "base station not available"),
    }
}

/// Wait briefly for one unsolicited report.
fn read_event(state: &mut State) -> Option<Vec<u8>> {
    let result = state.device.as_ref()?.read_event_timeout(EVENT_WAIT_MS);
    match result {
        Ok(report) => report,
        Err(e) => {
            info!(error = %format!("{e:#}"), "base station disconnected");
            drop_device(state);
            None
        }
    }
}

/// Status, volume, and any events that arrived around them.
fn poll(state: &mut State) {
    state.last_poll = Some(Instant::now());
    let Some(device) = state.device.as_ref() else { return };
    let polled = match device.poll() {
        Ok(p) => p,
        Err(e) => {
            info!(error = %format!("{e:#}"), "base station disconnected");
            drop_device(state);
            return;
        }
    };
    for report in &polled.events {
        on_report(state, report);
    }

    let now = Instant::now();
    if let Some(next) = polled.status {
        if let Some(reason) = state.status.and_then(|prev| wake::wake_reason(&prev, &next)) {
            debug!(?reason, "status change lights the screen");
            let action = state.screen.on(Input::OurEvent, now);
            perform(state, action);
        }
        state.status = Some(next);
    }
    if let Some(volume) = polled.volume {
        // The firmware draws its own volume popup (verified 2026-09-27).
        if state.volume.is_some_and(|v| v != volume) {
            let action = state.screen.on(Input::FirmwareEvent, now);
            perform(state, action);
        }
        state.volume = Some(volume);
    }
}

/// Feed one unsolicited report to the screen policy.
fn on_report(state: &mut State, report: &[u8]) {
    if !proto::is_event(report) {
        return; // a late reply to an earlier request, not something that happened
    }
    let input = if let Some(volume) = proto::decode_volume_event(report) {
        state.volume = Some(volume);
        Input::FirmwareEvent
    } else {
        // Nothing confirms the firmware shows these itself, and showing the stock
        // screen when it already is costs nothing.
        debug!(report = ?&report[..report.len().min(4)], "base station event");
        Input::OurEvent
    };
    let action = state.screen.on(input, Instant::now());
    perform(state, action);
}

/// Carry out a policy action. A failed write drops the device; the policy
/// re-syncs the screen through `DeviceOpened` when it comes back.
fn perform(state: &mut State, action: Option<Action>) {
    let (Some(action), Some(device)) = (action, state.device.as_ref()) else { return };
    let result = match action {
        Action::Blank => device.oled_blank(),
        Action::ShowStock => device.oled_show_stock(),
    };
    match result {
        Ok(()) => debug!(?action, "screen"),
        Err(e) => {
            warn!(?action, error = %format!("{e:#}"), "screen write failed; reopening");
            drop_device(state);
        }
    }
}

fn apply_brightness(state: &mut State) {
    let (Some(level), Some(device)) = (state.settings.brightness, state.device.as_ref()) else {
        return;
    };
    if let Err(e) = device.set_oled_brightness(level) {
        warn!(level, error = %format!("{e:#}"), "brightness write failed; reopening");
        drop_device(state);
    }
}

fn drop_device(state: &mut State) {
    state.device = None;
    state.status = None;
    state.volume = None;
}

fn shutdown(state: &mut State) {
    let action = state.screen.on(Input::Shutdown, Instant::now());
    perform(state, action);
    debug!("headset worker stopped");
}

fn snapshot(state: &State) -> HeadsetState {
    let connected = state.device.is_some();
    let status = state.status.filter(|_| connected);
    HeadsetState {
        connected,
        power: status.map_or(HeadsetPower::Unknown, |s| power(s.power)),
        headset_battery: status.and_then(|s| s.headset_battery),
        spare_battery: status.and_then(|s| s.spare_battery),
        screen: state.settings,
        screen_dark: connected && state.screen.is_dark(),
    }
}

fn power(p: PowerStatus) -> HeadsetPower {
    match p {
        PowerStatus::Offline => HeadsetPower::Offline,
        PowerStatus::Charging => HeadsetPower::Charging,
        PowerStatus::Online => HeadsetPower::Online,
        PowerStatus::Unknown(_) => HeadsetPower::Unknown,
    }
}

/// Broadcast the state if it changed since the last broadcast.
fn publish(state: &mut State, event_tx: &broadcast::Sender<Event>) {
    let now = snapshot(state);
    if state.published.as_ref() != Some(&now) {
        state.published = Some(now.clone());
        let _ = event_tx.send(Event::HeadsetChanged { state: now });
    }
}
```

- [ ] **Step 4: Wire into `main.rs`**

Imports: add `use std::time::Duration;` and `use tracing::{debug, error, info, warn};` (add `warn`), plus:

```rust
mod clock;
mod headset;
mod mouse;

use headset::{Command as HeadsetCommand, Handle as HeadsetHandle};
use mouse::{Command as MouseCommand, Handle as MouseHandle};

/// Longest the daemon waits, on exit, for the base-station screen to be blanked.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(1);

/// Handles to the device workers, passed to every client task.
#[derive(Clone)]
struct Workers {
    mouse: MouseHandle,
    headset: HeadsetHandle,
}
```

In `main`, replace the spawn/accept lines with:

```rust
    spawn_hid_monitor(snapshot_tx, event_tx.clone());
    let workers = Workers {
        mouse: mouse::spawn(event_tx.clone()),
        headset: headset::spawn(event_tx.clone()),
    };

    let result = accept_loop(&listener, snapshot_rx, event_tx, workers.clone()).await;

    // Leave the base-station screen dark: at logout or shutdown its USB power
    // often stays on, and nobody is there to see it.
    shutdown_headset(&workers.headset).await;
```

Change `accept_loop`, `handle_client`, and `dispatch` to take `workers: Workers` / `workers: &Workers` in place of `mouse: MouseHandle` / `mouse: &MouseHandle` (clone `workers` where `mouse` was cloned; pass `&workers` to `dispatch`), and replace `mouse.send(` with `workers.mouse.send(` in the mouse arms. In `dispatch`, replace the temporary headset arm from Task 3 with:

```rust
        Request::GetHeadsetState => match workers.headset.send(HeadsetCommand::GetState).await {
            Ok(state) => ResponseData::Headset { state },
            Err(message) => ResponseData::Error { message },
        },
        Request::SetHeadsetScreen { settings } => outcome(
            workers
                .headset
                .send::<()>(|reply| HeadsetCommand::SetScreen { settings, reply })
                .await,
        ),
```

Add after `outcome`:

```rust
/// Ask the headset worker to leave the screen dark, waiting at most [`SHUTDOWN_GRACE`].
async fn shutdown_headset(headset: &HeadsetHandle) {
    let request = headset.send::<()>(HeadsetCommand::Shutdown);
    match tokio::time::timeout(SHUTDOWN_GRACE, request).await {
        Ok(Ok(())) => debug!("base-station screen left dark"),
        Ok(Err(e)) => warn!(error = %e, "headset shutdown failed"),
        Err(_) => warn!("headset worker did not confirm shutdown in time"),
    }
}
```

Update the crate doc's "Current scope" sentence to: "…control the Rival 3 Wireless Gen 2, keep the Arctis Nova Pro Wireless base-station screen from burning in, and serve all of it over…".

- [ ] **Step 5: Build and test everything**

Run: `cargo test --workspace && cargo build --workspace --examples && cargo clippy --workspace --all-targets 2>&1 | grep -E "^(warning|error)" | sort | uniq -c`
Expected: all tests pass; no new clippy warnings in touched files.

- [ ] **Step 6: Smoke-run against the real base station** (no UI yet)

Run: `RUST_LOG=gglinuxd=debug cargo run -p gglinuxd` for ~20 s, then Ctrl-C.
Expected log lines: `base station connected`, `screen action=ShowStock`, ~10 s later `screen action=Blank`; on Ctrl-C `screen action=Blank` then `base-station screen left dark`. A person confirms the screen went black ~10 s after start and stayed black after exit.

- [ ] **Step 7: Commit**

```bash
git add crates/gglinuxd
git commit -m "gglinuxd: headset worker keeps the base-station screen dark"
```

---

### Task 7: UI — headset panel with burn-in protection controls

**Files:**
- Modify: `ui/src/lib/types.ts`
- Modify: `ui/src/lib/daemon.svelte.ts`
- Create: `ui/src/lib/HeadsetPanel.svelte`
- Modify: `ui/src/App.svelte`

**Interfaces:**
- Consumes: Task 3 wire shapes (`get_headset_state`, `set_headset_screen`, `headset_changed`)
- Produces: `daemon.headset: HeadsetState | null`, `daemon.getHeadsetState()`, `daemon.setHeadsetScreen(settings)`

- [ ] **Step 1: Types** — in `types.ts`, after `MouseState`:

```ts
export interface ScreenSettings {
  /** Keep the base-station screen dark, lighting it briefly after events. */
  protect: boolean;
  /** Seconds the screen stays lit after an event, 3..=600. */
  lit_seconds: number;
  /** 1..=10, or null to leave the device's own level alone (it can't be read back). */
  brightness: number | null;
}

export type HeadsetPower = "offline" | "charging" | "online" | "unknown";

/** Readings, unlike MouseState: the base station reports these. */
export interface HeadsetState {
  connected: boolean;
  power: HeadsetPower;
  /** Percent; null while the headset is off. */
  headset_battery: number | null;
  /** Percent; null when no spare pack is seated. */
  spare_battery: number | null;
  screen: ScreenSettings;
  /** Whether gglinux has the screen blanked right now. */
  screen_dark: boolean;
}
```

Extend the unions: `Request` gains `| { method: "get_headset_state" }` and `| { method: "set_headset_screen"; params: { settings: ScreenSettings } }`; `ResponseData` gains `| { type: "headset"; state: HeadsetState }`; `DaemonEvent` gains `| { event: "headset_changed"; data: { state: HeadsetState } }`.

- [ ] **Step 2: Client** — in `daemon.svelte.ts`: import `HeadsetState, ScreenSettings`; add after `mouse`:

```ts
  /** Headset battery, power, and screen settings; null until first fetched. */
  headset = $state<HeadsetState | null>(null);
```

In the `daemon://status` disconnect branch, after `this.mouse = null;`: `this.headset = null;`. Add after `saveMouse`:

```ts
  async getHeadsetState(): Promise<HeadsetState> {
    const res = await this.request({ method: "get_headset_state" });
    if (res.type !== "headset") throw new Error(`unexpected response: ${res.type}`);
    this.headset = res.state;
    return res.state;
  }

  async setHeadsetScreen(settings: ScreenSettings): Promise<void> {
    const res = await this.request({ method: "set_headset_screen", params: { settings } });
    if (res.type !== "success") throw new Error(`unexpected response: ${res.type}`);
    await this.getHeadsetState();
  }
```

In `#onMessage`'s switch, add:

```ts
      case "headset_changed":
        this.headset = ev.data.state;
        break;
```

- [ ] **Step 3: Panel** — create `ui/src/lib/HeadsetPanel.svelte`:

```svelte
<script lang="ts">
  // Panel for the Arctis Nova Pro Wireless: battery, and burn-in protection for
  // the base station's screen.
  //
  // Unlike the mouse, the base station reports its state, so what this shows are
  // readings, pushed by the daemon as they change.

  import Icon from "./Icon.svelte";
  import { daemon } from "./daemon.svelte";
  import type { HeadsetPower, ScreenSettings } from "./types";

  /** Mirrors ScreenSettings::LIT_SECONDS and ::BRIGHTNESS in gg-ipc. */
  const LIT_MIN = 3;
  const LIT_MAX = 600;
  const BRIGHTNESS_MIN = 1;
  const BRIGHTNESS_MAX = 10;

  const POWER_LABEL: Record<HeadsetPower, string> = {
    online: "headset on",
    charging: "headset charging",
    offline: "headset off",
    unknown: "headset state unknown",
  };

  let headset = $derived(daemon.headset);
  let screen = $derived(daemon.headset?.screen ?? null);

  let busy = $state(false);
  let error = $state<string | null>(null);

  // Depends on `connected` so a daemon restart refetches.
  $effect(() => {
    if (!daemon.connected) return;
    void daemon.getHeadsetState().catch((e: unknown) => {
      error = e instanceof Error ? e.message : String(e);
    });
  });

  /** Send the current settings with one field changed. Each control applies at
   *  once: the changes are cheap and reversible, and the screen shows the result. */
  async function update(patch: Partial<ScreenSettings>) {
    if (!screen) return;
    busy = true;
    error = null;
    try {
      await daemon.setHeadsetScreen({ ...screen, ...patch });
    } catch (e: unknown) {
      error = e instanceof Error ? e.message : String(e);
    } finally {
      busy = false;
    }
  }

  function setLitSeconds(raw: number) {
    if (!Number.isFinite(raw)) return;
    void update({ lit_seconds: Math.min(LIT_MAX, Math.max(LIT_MIN, Math.round(raw))) });
  }
</script>

<article class="panel">
  <header class="top">
    <div class="glyph"><Icon name="headset" size={26} /></div>
    <div class="ident">
      <h3>Arctis Nova Pro Wireless</h3>
      <p class="meta">
        {#if !headset?.connected}
          base station not connected
        {:else}
          {POWER_LABEL[headset.power]}
          {#if headset.headset_battery !== null}
            <span class="dot">·</span>
            <span class="battery" class:low={headset.headset_battery <= 25}>
              <span class="bar"><span class="fill" style:width="{headset.headset_battery}%"></span></span>
              {headset.headset_battery}%
            </span>
          {/if}
          <span class="dot">·</span>
          {headset.spare_battery === null ? "no spare battery" : `spare ${headset.spare_battery}%`}
        {/if}
      </p>
    </div>
  </header>

  {#if screen}
    <section>
      <div class="head">
        <h4>Screen burn-in protection</h4>
        {#if headset?.connected && screen.protect}
          <span class="state">{headset.screen_dark ? "screen dark" : "screen lit"}</span>
        {/if}
      </div>
      <label class="toggle">
        <input
          type="checkbox"
          checked={screen.protect}
          disabled={busy}
          onchange={(e) => update({ protect: e.currentTarget.checked })}
        />
        Keep the base-station screen dark, lighting it briefly for battery, headset and volume
        changes
      </label>
      <div class="row">
        <span class="label">Stay lit for</span>
        <input
          class="num"
          type="number"
          min={LIT_MIN}
          max={LIT_MAX}
          value={screen.lit_seconds}
          disabled={busy || !screen.protect}
          onchange={(e) => setLitSeconds(e.currentTarget.valueAsNumber)}
        />
        <span class="unit">seconds</span>
      </div>
      <div class="row">
        <span class="label">Brightness</span>
        <input
          type="range"
          min={BRIGHTNESS_MIN}
          max={BRIGHTNESS_MAX}
          step="1"
          value={screen.brightness ?? BRIGHTNESS_MAX}
          disabled={busy}
          onchange={(e) => update({ brightness: e.currentTarget.valueAsNumber })}
        />
        <span class="unit">{screen.brightness ?? "unchanged"}</span>
      </div>
      <p class="hint">
        No brightness switches the screen off — the lowest level is still lit — so protection
        blanks it with a black image instead. These settings reset when the daemon restarts.
      </p>
    </section>
  {/if}

  {#if error}
    <p class="error">{error}</p>
  {/if}
</article>

<style>
  .panel {
    padding: 1rem 1.1rem;
    background: var(--surface);
    border: 1px solid var(--line);
    border-radius: 12px;
  }
  .top {
    display: flex;
    gap: 1rem;
    align-items: center;
  }
  .glyph {
    flex: none;
    width: 46px;
    height: 46px;
    display: grid;
    place-items: center;
    border-radius: 10px;
    background: var(--surface-2);
    color: var(--accent);
  }
  .ident {
    min-width: 0;
  }
  h3 {
    margin: 0;
    font-size: 0.98rem;
    font-weight: 600;
  }
  h4 {
    margin: 0;
    font-size: 0.8rem;
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.05em;
    color: var(--text-dim);
  }
  .meta {
    margin: 0.25rem 0 0;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  .dot {
    opacity: 0.5;
    margin: 0 0.2rem;
  }
  .battery {
    display: inline-flex;
    align-items: center;
    gap: 0.4rem;
    color: var(--ok);
  }
  .battery.low {
    color: var(--danger);
  }
  .bar {
    display: inline-block;
    width: 54px;
    height: 6px;
    border-radius: 999px;
    background: var(--surface-2);
    overflow: hidden;
  }
  .fill {
    display: block;
    height: 100%;
    background: currentColor;
  }

  section {
    margin-top: 1.1rem;
    padding-top: 1rem;
    border-top: 1px solid var(--line);
  }
  .head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: 0.6rem;
    margin-bottom: 0.6rem;
  }
  .state {
    font-size: 0.76rem;
    color: var(--text-dim);
  }
  .toggle {
    display: flex;
    align-items: flex-start;
    gap: 0.5rem;
    font-size: 0.85rem;
    cursor: pointer;
  }
  .row {
    display: flex;
    align-items: center;
    gap: 0.6rem;
    margin-top: 0.7rem;
  }
  .label {
    width: 6.5rem;
    font-size: 0.82rem;
    color: var(--text-dim);
  }
  .num {
    width: 5rem;
    padding: 0.3rem 0.45rem;
    font-family: var(--mono);
    font-size: 0.85rem;
    color: var(--text);
    background: var(--surface-2);
    border: 1px solid var(--line);
    border-radius: 7px;
  }
  .unit {
    font-size: 0.78rem;
    color: var(--text-dim);
  }
  .hint {
    margin: 0.7rem 0 0;
    font-size: 0.76rem;
    color: var(--text-dim);
  }
  .error {
    margin: 0.8rem 0 0;
    padding: 0.5rem 0.7rem;
    font-size: 0.8rem;
    color: var(--danger);
    background: color-mix(in srgb, var(--danger) 12%, transparent);
    border-radius: 8px;
  }
</style>
```

- [ ] **Step 4: Mount it** — in `App.svelte`: `import HeadsetPanel from "./lib/HeadsetPanel.svelte";`; replace the `RIVAL3_PID` comment and add the Nova Pro check:

```ts
  /** Devices with a control panel so far. */
  const RIVAL3_PID = 0x1872;
  const NOVA_PRO_PID = 0x12e0;
  let hasRival3 = $derived(daemon.devices.some((d) => d.product_id === RIVAL3_PID));
  let hasNovaPro = $derived(daemon.devices.some((d) => d.product_id === NOVA_PRO_PID));
```

and after the `{#if hasRival3}…{/if}` block:

```svelte
        {#if hasNovaPro}
          <section class="controls">
            <HeadsetPanel />
          </section>
        {/if}
```

- [ ] **Step 5: Type-check**

Run: `cd ui && npm run check`
Expected: `svelte-check found 0 errors and 0 warnings`.

- [ ] **Step 6: See it** (needs a person)

With the real daemon running (`cargo run -p gglinuxd`), start the UI with `cd ui && npm run tauri dev` (never the debug binary directly — it loads a blank page without Vite). Expected: the headset panel shows battery and power; toggling protection off brings the stock screen back; moving brightness changes the screen and shows it lit for 10 s.

- [ ] **Step 7: Commit**

```bash
git add ui/src
git commit -m "UI: headset panel with base-station screen protection"
```

---

### Task 8: Docs — protocol corrections and M2 status

**Files:**
- Modify: `docs/PROTOCOLS.md` (headset section)
- Modify: `PLAN.md` (M2 section)

- [ ] **Step 1: `PROTOCOLS.md`** — replace note **2.** in the "Verified on hardware 2026-08-12" box with:

```markdown
> **2. `07 25 <n>` is the volume wheel, sent only while it moves.** On 2026-08-12 it arrived every
> ~16 ms with `n` wandering 2–8 and was logged as a possible level meter; on 2026-09-27 it was
> silent for minutes and burst only while the headset wheel turned. `n` is attenuation on the
> 0–56 scale (0 = loudest), the same value `06 20` reports at byte 3.
```

Replace the three OLED rows of the setter table with:

```markdown
| OLED frame | `06 93 <x> 00 <w> <h> <pixels>` | **1024-byte feature report** per strip, strips ≤ 64 px wide (x = 0, 64), column-major LSB-top. One all-black frame blanks the panel and holds 3+ min; re-sending every 2 s flashes the stock screen through. **Verified 2026-09-27** |
| OLED brightness | `06 85 <1-10>` | 64-byte output report. **No off level**: `0` behaves like `1` (ggoled rejects it). Verified 2026-09-27 |
| OLED return-to-UI | `06 95` | stock screen back immediately. A headset-wheel volume change also returns to it, permanently. Verified 2026-09-27 |
| Audio settings | `06 20` | reply byte 2 = audio input, **byte 3 = volume** (attenuation 0–56). Verified 2026-09-27 |
```

In "Interrupt-IN events", change `Master volume: \`07 25 <vol>\`` to `Master volume: \`07 25 <atten>\` (only while the wheel moves)`.

- [ ] **Step 2: `PLAN.md`** — under the M2 heading's paragraph, before M3, add:

```markdown
Progress 2026-09-27: `gg-device-headset` reads status and volume (verified on hardware
2026-08-12) and a setter is confirmed by readback. The daemon now has a headset worker, IPC and a
UI panel, whose first feature is **base-station screen burn-in protection** — this unit's OLED
never sleeps on its own, so gglinux keeps it black and lights it briefly on events (spec:
`docs/superpowers/specs/2026-09-27-headset-screen-protection-design.md`). Still to do for M2: the
setters in the UI (sidetone, ANC, EQ, mic, auto-off), the ChatMix dial, and the base station's
damaged knob is out of scope by the owner's choice.
```

- [ ] **Step 3: Commit**

```bash
git add docs/PROTOCOLS.md PLAN.md
git commit -m "docs: 07 25 is the volume wheel; OLED facts; M2 progress"
```

---

### Task 9: Hardware acceptance (a person watching the screen)

**Files:** none (results go in the final report; failures become fixes).

- [ ] **Step 1:** `RUST_LOG=gglinuxd=debug cargo run -p gglinuxd`. Expected: stock screen, black after 10 s.
- [ ] **Step 2:** Turn the headset wheel a few notches. Expected: volume popup, black again 10 s after the last notch; log shows `FirmwareEvent`-driven relight and `Blank`.
- [ ] **Step 3:** Press ANC once, then mic mute once (≥15 s apart). Expected: if `06 b0`/`07` reflect them, stock screen for 10 s then black. Record whether each one woke the screen — this resolves the spec's open question.
- [ ] **Step 4:** Turn the headset off, then on. Expected: stock screen for 10 s each time.
- [ ] **Step 5:** In the UI, set brightness 3, then lit period 20 s, then turn protection off and on. Expected: each change shows the stock screen (at the new brightness); off keeps it lit; on blanks it 20 s later.
- [ ] **Step 6:** Ctrl-C the daemon. Expected: screen stays black.
- [ ] **Step 7:** Unplug and replug the base station while the daemon runs. Expected: log `base station disconnected`, then `connected`; stock screen for 10 s, then black.
