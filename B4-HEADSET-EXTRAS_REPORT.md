# B4-HEADSET-EXTRAS report

Unit b4-headset-extras, branch `m11-b4-headset-extras` (from `aeb0193`), commits `8762bdb` … `2f76b8e`.
Built against fakes only. Nothing was sent to any device. Every new write and the new `06 80` query need the
`headset_extras` gate (off by default).

## Result

`~/projects/gg-m11/check.sh b4-headset-extras` on the final code (the tree of `2f76b8e`; that commit holds the
rustfmt patch this run applied, and the design note):

```
tests: 876 passed, 0 failed
CHECK OK
```

(831 at the base; +45.) The fake daemon's tests aren't part of `check.sh`; `cargo test -p gglinuxd --example
fakemouse` passed (6 tests) on `a0a0164`, the last change to the fake.

**Capture arctis-5 (from the lead, mid-task)** changed three things, all in `a0a0164`: 60 min dim (`83 06`) is
offered; the `20`, `80` and `b0` offsets were checked against its replies (all agree, see below); and since our pcaps
now hold an `80` reply whose values match GG's push, the home screen and dim timeout are shown from it (asked only
while the gate is on).

Note: commit `8762bdb` (device crate + IPC) doesn't build the whole workspace on its own; `80406b9` makes the
daemon and the fake match. Merge the branch as a whole.

## Files

Added:
- `crates/gg-device-headset/src/base.rs`: the seven base-station settings' bytes, the `06 80` query, `06 20` and
  `06 80` decoders.
- `crates/gg-device-headset/src/testing.rs`: `FakeBase`, a base station that answers `b0`/`20`/`80` and lags one
  read per reply after a change.
- `crates/gglinuxd/src/headset/gates.rs`: the worker follows the experimental gates on the event bus.
- `crates/gglinuxd/src/headset/ux.rs`: when to ask `06 80`, and skipping the first reply after a screen change here.
- `docs/superpowers/specs/2026-11-13-b4-headset-extras-design.md`: the design note.

Changed:
- `crates/gg-device-headset/src/{lib.rs,proto.rs}`: `Port` trait, `Headset<P = HidDevice>`, 50 ms between
  commands, `poll` returns the whole `06 20` reply, `ux_settings()`, `apply` saves only when `Setting::saved()`;
  `Status` gains Bluetooth and pairing; `analog_gain_packet` replaced by `base::gain_packet` (same bytes, `27 01`
  low / `27 02` high, but it is the output gain, not a mic hi-Z gain as its comment said).
- `crates/gg-ipc/src/lib.rs` (headset items only): new enums, `HeadsetChange` variants, `HeadsetChange::feature()`,
  `HeadsetControls` fields, tests after the existing headset tests.
- `crates/gglinuxd/src/headset.rs`, `headset/controls.rs`, `headset/wake.rs` (test helper only).
- `crates/gglinuxd/src/bin/ggctl/headset.rs`; `ggctl/main.rs`: USAGE lines after `ggctl mouse radio`, and the
  `ResponseData::Headset` print arm after `Mouse`. Nothing else in that file.
- `crates/gglinuxd/examples/fakemouse/headset.rs`.
- `crates/gglinux-ui/src/engine.rs`, `crates/gglinux-ui/ui/devices.slint` (headset card only; `NumberBox`/`Pick`
  untouched). `eq.rs` wasn't needed.

## Public API

- `gg_device_headset`: `Port` (`write`, `send_feature`, `read`), `Headset::with_port`, `COMMAND_GAP`, `Poll.audio`,
  `Headset::ux_settings() -> UxReply { raw, decoded, events }`; `base::{BluetoothCall, Gain, LineOut, StreamMix,
  HomeScreen, DimTimeout, BluetoothLink, Pairing, AudioSettings, UxSettings}` and `*_packet` builders;
  `Setting::{BluetoothAtPowerOn, BluetoothCall, Gain, LineOut, StreamMix, HomeScreen, ScreenDim}`,
  `Setting::saved()`; `Status.{bluetooth_at_power_on, bluetooth_call, bluetooth_on, bluetooth_link, pairing}`;
  `testing::FakeBase` (`reports`, `commands`, `gaps`, `features`). Removed: `analog_gain_packet`,
  `Headset::set_analog_gain_high` (nothing used them).
- `gg_ipc`: `HeadsetChange::{BluetoothAtPowerOn(bool), BluetoothCall(BluetoothCall), Gain(HeadsetGain),
  LineOut(LineOut), LineOutMix(LineOutMix), HomeScreen(HomeScreen), ScreenDimMinutes(u16)}` — JSON e.g.
  `{"setting":"line_out_mix","value":{"main":50,"aux":100,"mic":0}}`; `HeadsetChange::feature() -> Option<Feature>`;
  `HeadsetControls.{bluetooth_at_power_on, bluetooth_call, bluetooth_on, bluetooth_link, pairing, gain, line_out,
  line_out_mix, home_screen, screen_dim_minutes}` (all `Option`, `#[serde(default)]`); `SCREEN_DIM_MINUTES =
  [0,1,5,10,15,30,60]`, `LINE_OUT_LEVEL_MAX = 100`. (`LineOutMix`, not `StreamMix`: Sonar already has a `StreamMix`.)
  No new requests or events: `SetHeadset` / `GetHeadsetState` / `HeadsetChanged` carry it all.
- ggctl: `ggctl headset` prints everything ("unknown" where nothing is known); `ggctl headset <setting> <value>` for
  `anc`, `transparency`, `sidetone`, `mic-volume`, `mic-led`, `auto-off`, `radio`, `eq`, `eq-custom`,
  `bluetooth-at-power-on`, `bluetooth-call`, `gain`, `line-out`, `stream-mix MAIN,AUX,MIC`, `home-screen`,
  `screen-dim`.
- App shot steps: `headset.set-bt-at-power-on`, `set-bt-call`, `set-gain`, `set-line-out`, `set-mix-main|aux|mic`,
  `set-home-screen`, `set-dim`.

## Pictures (in `~/projects/gg-m11/shots-b4/`, all looked at)

- `headset-gate-off.png`: the card with `--reported`; BLUETOOTH / OUTPUT / BASE-STATION SCREEN under the orange
  refusal sentence, their choices dimmed, the toggle and drop-down greyed; "Bluetooth off", "headset linked".
- `headset-gate-on.png`: after `experimental.on:8`: no refusal, all at full brightness (drawn before arctis-5, when
  dim showed "unknown").
- `headset-gate-on-dim60.png`: gate on after arctis-5: home screen "Detailed" read back, dim "60 min" picked (held
  through the fake's one-read lag), the new hint about the 30-second read.
- `headset-stream-lanes.png`: mouse and headset side by side (narrow lane): line out Stream after the fake's
  one-read lag, three stream rows, main 50 % / aux 100 % / mic 100 %; "During a call" fits the lane.
- `headset-off.png`: `--headset off`: "headset off · spare 100%", POWER & RADIO says "paired, headset off or
  away"; the base-station settings still show (they are base-station settings; GG's captures were made with the
  headset off).
- `all-gate-off.png`: the whole Engine page, for layout.

Also run against the fake with the real ggctl (before arctis-5): gate off → the refusal; gate on → `ok`; the first
read after the changes still shows the old values, the second shows `gain: high`, `line out: stream (main 50, aux
100, mic 0)` and the dim timeout as set.

## What is not done, or uncertain

- **Offsets checked against arctis-5's replies** (headset on), read just before GG pushed the same values:
  `06 20 00 03 01 00 00 14×10 0a 00 01 64 64 64 00 64 64` → gain low, Flat, flat bands, mic volume 10, sidetone off,
  line out speakers, stream 100/100/100 (GG then sent `27 01`, `2e 00`, `33 14×10`, `37 0a`, `39 00`, `43 01`,
  `47 64 00 64 64`); `06 80 01 0a 01 00` → dim 1 min, brightness 10, home detailed (GG sent `83 01`, `85 0a`,
  `89 00`); `06 b0 00 00 01 00 01 08 08 00 02 0a 05 00 08 08` → Bluetooth bytes as with the headset off, pairing
  `08` connected, power `08` online, battery `01` (12 %), matching the `07 b7 01 08 08` event. Nothing contradicts
  the file. Still, **no capture shows a changed value coming back** in a reply: the hands-on checks close that.
- The `80` reply's brightness byte is decoded but not shown; its byte 4 ("idle brightness") isn't used.
- The `07 b5 01 00 <04|08>` events in arctis-5 carry the pairing codes as the headset's link drops and returns;
  they are not decoded (the `b0` poll each second shows the same).
- No save (`09`) follows the new settings, as GG sends them. Whether the base station keeps them over a power cycle
  is unverified (check 9 below).
- GG's game EQ presets (above Custom) read as unknown.
- The headset card follows a gate change at the next headset state, not at once: see the `lib.rs` line below.
- No worker-level test (the worker's state holds a real `HidApi`); the gate-then-send path is tested through
  `controls::send_change` with `FakeBase`, the `80` schedule through `ux_due`. The IPC tests and the 50 ms gap test
  passed on their first run (written with their code); the device-crate tests were seen red first (17 failing on
  `todo!()`).
- ChatMix dial (H10), Sonar and the burn-in protection's behaviour are untouched.

## Changes needed in files I don't own

1. `crates/gglinux-ui/src/lib.rs`, in `show`, `Changed::Experimental` arm (b2 owns the file this wave), so the
   headset card greys and un-greys the moment the gate flips:
   ```rust
           Changed::Experimental => {
               experimental::show(ui, s);
               mouse::show_mouse(ui, s, &forms.mouse);
               // Panels whose controls are greyed while their gate is off.
               engine::show_headset(ui, s, &forms.headset);
               bindings::show(ui, s);
   ```
2. `docs/PROTOCOLS.md` §2 (lead):
   - Setters table: rename "Analog gain (hi-Z)" to "Output gain `06 27 <1 low|2 high>`" (GG: Audio tab "Gain";
     Arctis Sound Manager: "Audio Gain"); Line-out row: "`06 43 <1 speakers|2 stream>`; stream mix `06 47 <main> 00
     <aux> <mic>` as GG sends it (ASM sends main twice)". Add `06 b2 <0|1>` Bluetooth on at power-on, `06 b3
     <0 nothing|1 game −12 dB|2 mute game>`, `06 89 <0 detailed|1 simple>`, `06 83 <0 never|1|2|3|4|5|6 = 1, 5,
     10, 15, 30, 60 min>` (60 from arctis-5), `06 80` screen-settings query. "GG sends none of these with a save (`09`); gglinux doesn't either.
     GG keeps ~50 ms between commands (ASM `time_between_commands_ms`); gglinux keeps 50 ms."
   - Status map: "byte 4 Bluetooth power reads `00` on, `01` off (ASM `status_parse`); byte 5 `0` off, `1` phone
     connected, `2` on, nothing connected; byte 14 `1` not paired, `4` paired, headset off/away, `8` connected.
     arctis-1 (headset off): `06 b0 00 00 01 00 00 08 08 00 02 0a 05 00 04 01`; arctis-5 (on): `06 b0 00 00 01 00 01
     08 08 00 02 0a 05 00 08 08`. Events `07 b5 01 00 <04|08>` carry the same link codes; `07 b7 <headset> <spare>
     <power>` (arctis-5: `07 b7 00 08 01` off, `07 b7 01 08 08` on)."
   - New "`06 20` reply": "byte 3 volume, 4 gain (1/2), 5 surround, 6 EQ preset, 7–16 Custom bands (`0x14` = 0 dB),
     17 mic volume (1–10), 18 sidetone (0–3), 19 line out (1/2), 20–21 ChatMix, 22 stream main, 23 unused, 24 aux,
     25 mic (ASM `settings_readback`/`probe_queries`). arctis-5: `06 20 00 03 01 00 00 14×10 0a 00 01 64 64 64 00 64
     64`, the values GG pushed next."
   - New "`06 80` reply": "per ASM `probe_queries.ux_settings`: byte 2 dim timer (the `83` codes), 3 brightness,
     4 idle brightness, 5 home screen, 6 home-screen mic mute, 7 Sonar present. arctis-5: `06 80 01 0a 01 00`, the
     values GG pushed next (`83 01`, `85 0a`, `89 00`)."
   - The firmware string: `06 10` answers in ASCII (`0000.003.0820001.029.027…` in arctis-5); gglinux doesn't use it.
3. Optional, `crates/gglinuxd/src/main.rs`: nothing is required (the headset worker follows the gates on the event
   bus). If the lead prefers the mouse's pattern later, `headset::spawn` can take `gates.subscribe()` instead.

## For README / PLAN / CHANGELOG

- CHANGELOG: "Headset: the base station's own settings from GG — Bluetooth on at power-on, what a Bluetooth call does
  to the game sound, gain, line out (speakers or a stream mix of main, aux and mic), the home screen and how soon its
  screen dims — in the headset card and `ggctl headset`, behind the experimental switch *Headset: Bluetooth, gain and
  line out*. gglinux now shows what the base station reports for gain, line out, sidetone, mic volume, EQ, Bluetooth
  and the headset link instead of 'unknown until changed here'. Commands to the base station are kept 50 ms apart."
- PLAN: H01, H02, H04–H07, H09 built (gated, untried on hardware); H03 and H08 built as read-backs, checked against
  captured replies (the `06 80` one asked only with the gate on).
- README (headset section): "`ggctl headset` shows the headset and every base-station setting; `ggctl headset gain
  high` and similar change one (the base station's own settings need `ggctl experimental headset_extras on`)."

## Hands-on checks for the owner

Before: `ggctl experimental headset_extras on`, then `ggctl headset` and keep its output (the base station is on GG's
defaults since the captures: gain low, sidetone off, EQ Flat, line out speakers, home detailed, dim 1 min, Bluetooth
at power-on off, calls do nothing). The burn-in protection keeps the base-station screen dark, but each change here
lights it for one lit period (10 s): look at it right after each command. After each change wait 3 s, then
`ggctl headset`: the line for that setting should show the new value (that proves the read-back too; the first read
may still show the old one, so read twice).

1. **Gain (H04)**: turn the volume down first. `ggctl headset gain high` → sound in the headset gets louder;
   `gain: high`. Back: `ggctl headset gain low`.
2. **Bluetooth at power-on (H01)**: `ggctl headset bluetooth-at-power-on on` → `on at power-on: yes`. Switch the
   headset off and on: Bluetooth comes on by itself. Back: `ggctl headset bluetooth-at-power-on off`.
3. **Bluetooth call (H02)**: `ggctl headset bluetooth-call lower` → `during a call: game 12 dB lower`; with a phone
   on Bluetooth, start a call while a game plays: the game gets quieter. `ggctl headset bluetooth-call mute` → game
   silent during the call. Back: `ggctl headset bluetooth-call nothing`.
4. **Bluetooth and link status (H03, read only)**: `ggctl headset` with the headset on says `headset link:
   connected`, with it off `paired, headset off or out of reach`; `bluetooth: off`, or `on, a phone is connected`
   while a phone is paired. The app's card shows the same at the right of BLUETOOTH and POWER & RADIO.
5. **Line out (H05)**: `ggctl headset line-out stream` → speakers on the line out now get the stream mix; `line out:
   stream (main 100, aux 100, mic 100)`. Back: `ggctl headset line-out speakers`.
6. **Stream mix (H06)**: with line out on stream, `ggctl headset stream-mix 50,100,100` → the line-out's main sound
   drops by half; `stream (main 50, aux 100, mic 100)`. Back: `ggctl headset stream-mix 100,100,100`, then
   `ggctl headset line-out speakers`.
7. **Home screen (H07)**: `ggctl headset home-screen simple` → the base station's home screen (lit for 10 s) shows
   the simple layout. Back: `ggctl headset home-screen detailed`.
8. **Dim timeout (H09)**: turn off "Keep the base-station screen dark" in the app first (else gglinux blanks it).
   `ggctl headset screen-dim never` → the screen stays bright past a minute (`screen-dim 60` is accepted too). Then
   `ggctl headset screen-dim 5`, wait 15 s: `ggctl headset` says `screen dim: after 5 min`, by then read back from
   the base station (the first reply after a change is skipped, the next is taken about 10 s after it). The same
   holds for `home-screen` in check 7. Back: `ggctl headset screen-dim 1`, and the protection toggle on again.
9. **Read-backs (H08) and keeping settings**: `ggctl headset sidetone low` → `sidetone low` in `ggctl headset`;
   `systemctl --user restart gglinuxd`, wait 3 s: `ggctl headset` still says `sidetone low` (read from the base
   station, not remembered). Then `ggctl headset gain high`, unplug the base station's USB and plug it back, wait
   5 s: `gain: high` means the base station keeps the new settings without a save. Back: `ggctl headset sidetone
   off`, `ggctl headset gain low`.

Put everything back: `ggctl headset gain low`, `bluetooth-at-power-on off`, `bluetooth-call nothing`, `stream-mix
100,100,100`, `line-out speakers`, `home-screen detailed`, `screen-dim 1`, `sidetone off`, then compare with the
output kept before, and `ggctl experimental headset_extras off`.

## Design note

`docs/superpowers/specs/2026-11-13-b4-headset-extras-design.md`: what was built, the exact sources (captures
arctis-1…3; Arctis Sound Manager ea410e0 sections), ten decisions, a table of what each read-back rests on, and the
two-minute checks.
