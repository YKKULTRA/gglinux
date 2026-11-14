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
  answers `b0`/`20`/`80` with captured replies and lags one read per reply after a change.
- `gg-ipc`: `HeadsetChange::{BluetoothAtPowerOn, BluetoothCall, Gain, LineOut, LineOutMix, HomeScreen,
  ScreenDimMinutes}`, `HeadsetChange::feature()`, ten new `HeadsetControls` fields (all `#[serde(default)]`), enums
  `BluetoothCall`, `HeadsetGain`, `LineOut`, `HomeScreen`, `BluetoothLink`, `HeadsetPairing`, struct `LineOutMix`,
  `SCREEN_DIM_MINUTES` (0, 1, 5, 10, 15, 30, 60), `LINE_OUT_LEVEL_MAX`.
- Daemon `headset.rs` + `headset/{controls,gates,ux}.rs`: the gate check before anything is sent, the read-wins
  merge of what the base station reports with what gglinux wrote, and the gated slow `06 80` poll.
- `ggctl headset` (state and every setting), the fake daemon's headset, and the app's headset card (BLUETOOTH, OUTPUT,
  BASE-STATION SCREEN groups).

## Sources

- **Our captures** `arctis-1.pcap` … `arctis-3.pcap` (headset off) and `arctis-5.pcap` (headset on), 2026-11-13,
  `~/projects/gg-m11/captures/`, `GG-CAPTURES-2026-11-13.md` "Arctis". Every write was seen going out from GG, both
  ways: gain `27 02`/`27 01`; Bluetooth at power-on `b2 01`/`b2 00`; Bluetooth call `b3 01` (lower 12 dB), `b3 02`
  (mute game), `b3 00` (nothing); home screen `89 01`/`89 00`; line out `43 02`/`43 01`; stream mix
  `47 <main> 00 <aux> <mic>` (connect push `47 64 00 64 64`, main 50 % = `32`); dim timeout `83 00`…`83 05`
  (off, 1, 5, 10, 15, 30 min, arctis-3) and `83 06` (60 min, arctis-5, 21:54:00, back with `83 01`). GG sends no
  save (`09`) after any of them.
- **arctis-5's replies** (GG's reads before its connect push, headset on):
  `06 20 00 03 01 00 00 14×10 0a 00 01 64 64 64 00 64 64`, `06 80 01 0a 01 00`,
  `06 b0 00 00 01 00 01 08 08 00 02 0a 05 00 08 08`; events `07 b5 01 00 04` → `07 b5 01 00 08` and
  `07 b7 00 08 01` → `07 b7 01 08 08` while the headset's link dropped and came back.
- **Arctis Sound Manager** device file, commit ea410e0 (`evidence/a1b3c54352806e97.txt`): `settings` (the same
  writes, except the dim timeout; it sends `47 main main aux mic`), `status.response_mapping` for `06b0`,
  `status_parse` (Bluetooth power `00` = on, `01` = off; link `0/1/2`; pairing `1/4/8`), `settings_readback` and
  `probe_queries.audio_settings` for `06 20`, `probe_queries.ux_settings` for `06 80` (field numbers there count
  from the opcode: field N is byte N+1), `time_between_commands_ms: 50`.
- `docs/PROTOCOLS.md` §2 and its "Captured from GG 123" part.

## Decisions

1. **GG's bytes where GG and Arctis Sound Manager differ**: the stream mix goes out `47 <main> 00 <aux> <mic>`.
   60 min dim is offered since arctis-5 (`83 06`); before that capture it was left out.
2. **No save after the new settings** (`Setting::saved()` is false for them): GG sends each one alone, and nothing
   writes flash unless asked. The first milestones' settings keep their save, unchanged. Whether the base station
   keeps the new ones over a power cycle without a save is unverified (hands-on check 9 in the report).
3. **A 50 ms gap between commands** (`COMMAND_GAP`), as GG keeps for this base station; before, a setting and its save
   went out 0 ms apart.
4. **Read wins.** `controls::merged` shows what the `b0`/`20`/`80` replies say wherever they say it, and what gglinux
   wrote this session only where they don't. A byte outside a setting's values reads as unknown, never as a guess.
   GG's game EQ presets (numbered after Custom) read as unknown.
5. **The `06 80` query is asked only while `headset_extras` is on**, every 30 s (`headset/ux.rs`). Its dim timeout
   and home screen are shown; its brightness is decoded but not shown (gglinux's brightness setting means "leave it
   alone" when unset, and nothing else needs it). When the gate goes off, what was read is forgotten (it would grow
   old), and the written values show again.
6. **After gglinux changes a screen setting, the next `80` reply is skipped** and the one after is asked for 5 s
   later: the base station may still answer with the old value right after a write, and a stale read would stand
   for 30 s. Until then the written value shows.
7. **The gate is followed on the event bus** (`headset/gates.rs`): `headset::spawn` still takes only the event
   sender, so `main.rs` needn't change. The saved file at start, then each `ExperimentalChanged`; a change is saved
   and announced before its "ok", and the worker looks before each command, so the next request finds it. If the bus
   drops news, the file (saved before each announcement) has the latest.
8. **The gate is checked before the values and the connection**, so a refused change sends nothing and says why even
   with the base station unplugged. Decoding more of the replies gglinux already asked for is not gated.
9. **Nothing is pushed at connect** (GG pushes its whole profile; we don't): a setting goes out only when the user
   changes it. A poll sends only `b0` and `20` (test `a_poll_reads_status_and_the_whole_audio_reply`).
10. **The app shows what it sent until the daemon shows it** (`experimental::Pending` + two read-backs 300 ms apart).
    A stream-mix level is sent only once the whole mix is known, so a guess for the other two levels is never sent.
    The Segmented choices are dimmed while the gate is off (the widget has no disabled look).
11. **The fake daemon lags one read** after a headset change and refuses the gated changes as the daemon does; with
    `--reported` it reports GG's defaults for everything the replies carry (the home screen and dim time only while
    the gate is on).

## What each read-back rests on

| Field | Offset | Our own evidence | Shown? |
|---|---|---|---|
| Bluetooth at power-on | `b0` byte 2 | `00` in arctis-1 and arctis-5; GG then pushed `b2 00` | yes |
| Bluetooth call | `b0` byte 3 | `00` in both; GG pushed `b3 00` | yes |
| Bluetooth on | `b0` byte 4 (`00` on) | `01` (off) in both, headset off and on, nothing paired | yes, read only |
| Bluetooth link | `b0` byte 5 | `00` (off) in both | yes, read only |
| 2.4 GHz pairing | `b0` byte 14 | `04` headset off (arctis-1), `08` headset on (arctis-5); `07 b5 01 00 04/08` events carry the same codes | yes, read only |
| Headset battery | `b0` byte 6 | `01` in arctis-5, as the `07 b7 01 08 08` event says | yes (as before) |
| Gain, EQ preset, bands, mic volume, sidetone, line out, stream mix | `20` bytes 4, 6, 7–16, `11`, `12`, `13`, `16`/`18`/`19` | arctis-5's reply: `01`, `00`, `14`×10, `0a`, `00`, `01`, `64`/`64`/`64` — what GG pushed right after (`27 01`, `2e 00`, `33 14×10`, `37 0a`, `39 00`, `43 01`, `47 64 00 64 64`); byte `17` is `00` | yes |
| Dim timeout, home screen | `80` bytes 2, 5 | arctis-5's reply: `01`, `00` — GG pushed `83 01`, `89 00` | yes, gate on |
| Brightness | `80` byte 3 | `0a`; GG pushed `85 0a` | decoded, not shown |

No captured value contradicts the file. No capture shows a *changed* value coming back in a reply (the replies were
read before GG's push, which set the same values): the hands-on checks close that.

## Unverified on hardware, and a two-minute check

- Every new write (the owner runs the report's hands-on checks; each says what the base station's screen should show
  and how to put it back). Start with gain at low volume.
- A changed value coming back: `ggctl experimental headset_extras on`, `ggctl headset line-out stream`, wait 3 s,
  `ggctl headset` must say `line out: stream (main 100, aux 100, mic 100)`; `ggctl headset screen-dim 5`, wait 15 s,
  `ggctl headset` must say `screen dim: after 5 min` (read back by then); put back with `ggctl headset line-out
  speakers` and `ggctl headset screen-dim 1`.
- Whether the new settings survive unplugging the base station without a save.
