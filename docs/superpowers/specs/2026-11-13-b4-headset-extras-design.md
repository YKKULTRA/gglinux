# B4 headset-extras: the base station's own settings, and real read-backs

Unit b4-headset-extras of M11 wave B (audit rows H01–H09; H10, the ChatMix dial, is left out). Built against fakes
only: nothing here has been sent to the owner's base station. Every new write, and the new `06 80` query, stays behind
the `headset_extras` experimental gate (off by default).

## What

- `gg-device-headset/src/base.rs` (new): byte builders for the seven base-station settings GG 123 has and gglinux
  lacked, the `06 80` query, and decoders for the `06 20` reply (audio settings) and the `06 80` reply (screen
  settings). `proto.rs`: the `06 b0` decode gains Bluetooth at power-on, call behaviour, Bluetooth on/off, the
  Bluetooth link and the 2.4 GHz pairing state; `Setting` gains the seven settings and `Setting::saved()`.
  `lib.rs`: a `Port` trait (the hidraw node, or a fake), a 50 ms gap between commands, `Headset::poll` returns the
  whole `06 20` reply, `Headset::ux_settings` asks `06 80`. `testing.rs` (new): `FakeBase`, a base station that
  answers `b0`/`20`/`80` and lags one read per reply after a change.
- `gg-ipc`: `HeadsetChange::{BluetoothAtPowerOn, BluetoothCall, Gain, LineOut, LineOutMix, HomeScreen,
  ScreenDimMinutes}`, `HeadsetChange::feature()`, ten new `HeadsetControls` fields (all `#[serde(default)]`), enums
  `BluetoothCall`, `HeadsetGain`, `LineOut`, `HomeScreen`, `BluetoothLink`, `HeadsetPairing`, struct `LineOutMix`,
  `SCREEN_DIM_MINUTES`, `LINE_OUT_LEVEL_MAX`.
- Daemon `headset.rs` + `headset/{controls,gates}.rs`: the gate check before anything is sent, read-wins merge of
  what the base station reports with what gglinux wrote, the gated slow `06 80` poll (logged, not shown).
- `ggctl headset` (state and every setting), the fake daemon's headset, and the app's headset card (BLUETOOTH, OUTPUT,
  BASE-STATION SCREEN groups).

## Sources

- **Our captures** `arctis-1.pcap` … `arctis-3.pcap` (2026-11-13, `~/projects/gg-m11/captures/`,
  `GG-CAPTURES-2026-11-13.md` "Arctis"), base station `12e0`, headset off. Every write was seen going out from GG,
  both ways: gain `27 02`/`27 01`; Bluetooth at power-on `b2 01`/`b2 00`; Bluetooth call `b3 01` (lower 12 dB),
  `b3 02` (mute game), `b3 00` (nothing); home screen `89 01`/`89 00`; line out `43 02`/`43 01`; stream mix
  `47 <main> 00 <aux> <mic>` (connect push `47 64 00 64 64`, main 50 % = `32`); dim timeout `83 00`…`83 05`
  (off, 1, 5, 10, 15, 30 min). GG sends no save (`09`) after any of them.
- **Arctis Sound Manager** device file, commit ea410e0 (`evidence/a1b3c54352806e97.txt`): `settings` (the same
  writes, except the dim timeout; it sends `47 main main aux mic`), `status.response_mapping` for `06b0`,
  `status_parse` (Bluetooth power `00` = on, `01` = off; link `0/1/2`; pairing `1/4/8`), `settings_readback` and
  `probe_queries.audio_settings` for `06 20`, `probe_queries.ux_settings` for `06 80` (field numbers there count
  from the opcode: field N is byte N+1), `time_between_commands_ms: 50`.
- `docs/PROTOCOLS.md` §2 and its "Captured from GG 123" part; the 2026-08-12 `b0` sample and the 2026-09-27 `20` read
  (`06 20 00 06 01 00 00 14 14`, first nine bytes) quoted there and in the old tests.

## Decisions

1. **GG's bytes where GG and Arctis Sound Manager differ**: the stream mix goes out `47 <main> 00 <aux> <mic>`.
   60 min dim (`83 06`?) was never captured, so it isn't offered anywhere (`DimTimeout` has no such value, the IPC
   list stops at 30, ggctl and the daemon refuse 60 with a message saying why).
2. **No save after the new settings** (`Setting::saved()` is false for them): GG sends each one alone, and nothing
   writes flash unless asked. The first milestones' settings keep their save, unchanged. Whether the base station
   keeps the new ones over a power cycle without a save is unverified (hands-on check 9 in the report).
3. **A 50 ms gap between commands** (`COMMAND_GAP`), as GG keeps for this base station; before, a setting and its save
   went out 0 ms apart.
4. **Read wins.** `controls::merged` shows what the `b0`/`20` replies say wherever they say it, and what gglinux wrote
   this session only where they don't (not yet read, or never: home screen, dim timeout). A byte outside a setting's
   values reads as unknown, never as a guess. GG's game EQ presets (numbered after Custom) read as unknown.
5. **The `06 80` query is asked only while `headset_extras` is on**, every 30 s, and 5 s after gglinux changes a
   screen setting. No capture of ours holds an `80` reply, and none shows a written value coming back, so **nothing
   from it is shown**: the daemon logs each changed reply at info level (`base station screen settings (06 80;
   offsets not checked on hardware yet)`, decoded and raw) so the owner can check the offsets once.
6. **The gate is followed on the event bus** (`headset/gates.rs`): `headset::spawn` still takes only the event
   sender, so `main.rs` needn't change. The saved file at start, then each `ExperimentalChanged`; a change is saved
   and announced before its "ok", and the worker looks before each command, so the next request finds it. If the bus
   drops news, the file (saved before each announcement) has the latest.
7. **The gate is checked before the values and the connection**, so a refused change sends nothing and says why even
   with the base station unplugged. Decoding more of the replies gglinux already asked for is not gated.
8. **Nothing is pushed at connect** (GG pushes its whole profile; we don't): a setting goes out only when the user
   changes it. A poll sends only `b0` and `20` (test `a_poll_reads_status_and_the_whole_audio_reply`).
9. **The app shows what it sent until the daemon shows it** (`experimental::Pending` + two read-backs 300 ms apart),
   because the base station may answer the first read after a write with the old value. A stream-mix level is sent
   only once the whole mix is known, so a guess for the other two levels is never sent. The Segmented choices are
   dimmed while the gate is off (the widget has no disabled look).
10. **The fake daemon lags one read** after a headset change and refuses the gated changes as the daemon does; with
    `--reported` it reports GG's defaults for everything the `b0`/`20` replies carry.

## What each read-back rests on

| Field | Offset | Our own evidence | Shown? |
|---|---|---|---|
| Bluetooth at power-on | `b0` byte 2 | arctis-1: `00` after GG sent `b2 00` | yes |
| Bluetooth call | `b0` byte 3 | arctis-1: `00` after GG sent `b3 00` | yes |
| Bluetooth on | `b0` byte 4 (`00` on) | arctis-1 and 2026-08-12: `01` (off), as expected with nothing paired | yes, read only |
| Bluetooth link | `b0` byte 5 | `00` (off) in both | yes, read only |
| 2.4 GHz pairing | `b0` byte 14 | `04` headset off (arctis-1), `08` headset on (2026-08-12) | yes, read only |
| Gain | `20` byte 4 | 2026-09-27: `01` (low, GG's default) | yes |
| EQ preset, bands 1–2 | `20` bytes 6, 7–8 | 2026-09-27: `00`, `14 14` (Flat, 0 dB) | yes |
| EQ bands 3–10, mic volume, sidetone, line out, stream mix | `20` bytes 9–16, `11`, `12`, `13`, `16`/`18`/`19` | none: Arctis Sound Manager only ("confirmed on a 0x12e0 by asm-cli tools probe") | yes, range-checked |
| Dim timeout, brightness, home screen | `80` bytes 2, 3, 5 | none: no `80` reply in any capture | **no** (logged only) |

None of the captured values contradicts the file. Our pcaps hold no `06 20` reply at all (GG didn't ask for one
while recording).

## Unverified on hardware, and a two-minute check

- Every new write (the owner runs the report's hands-on checks; each says what the base station's screen should show
  and how to put it back). Start with gain at low volume.
- The `20` offsets past byte 8: `ggctl experimental headset_extras on`, `ggctl headset line-out stream`, wait 3 s,
  `ggctl headset` must say `line out: stream (main 100, aux 100, mic 100)`; then `ggctl headset line-out speakers`.
- The `80` offsets: with the gate on, `ggctl headset screen-dim 5`, wait 40 s (the first ask, 5 s after the change,
  may still get the old reply; the next is 30 s later), then
  `journalctl --user -u gglinuxd --since -1min | grep "06 80"`: the decoded line should say `dim_timeout:
  Some(Min5)` and the raw bytes start `06 80 02`; then `ggctl headset screen-dim 1`. If it does, the
  dim timeout and home screen can be shown from the reply (one line each in `controls::merged`).
- Whether the new settings survive unplugging the base station without a save.
