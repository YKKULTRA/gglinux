# Base-station screen burn-in protection — design

**Status:** approved in conversation 2026-09-27, pending written-spec review.
**Milestone:** first slice of M2's daemon integration (headset worker, IPC, UI).

## Problem

The Arctis Nova Pro Wireless base station (GameDAC Gen 2, `1038:12e0`) has a 128×64 OLED that on
this unit **never turns off**: the same stock screen is lit for as long as the base has power. That
is the textbook recipe for OLED burn-in. The on-device menus can't help — the base station's knob
was damaged by a spill and is not usable — so software is the only way to manage the screen.

## Goal

The screen is **dark by default** and lit only for a few seconds when something real happens, the
way a phone's lock screen behaves. Success: over a normal day the screen spends almost all of its
time black, and still shows what matters (battery, headset state, volume changes) briefly when it
happens.

Explicitly **out of scope**: anything about the base-station knob (the owner does not use it —
leave it alone), a custom-drawn replacement screen or pixel shifting, reacting to system suspend,
persisting settings across daemon restarts (config file is M7).

## What the hardware does (spike, 2026-09-27, verified on this unit)

| Question | Finding |
|---|---|
| Does `06 85 00` turn the screen off? | **No.** Brightness is `1..=10`; `0` behaves like the dimmest level (ggoled also rejects `0`). Brightness alone cannot stop burn-in. |
| Does a black frame blank it? | **Yes.** One all-black `06 93` frame (two 1024-byte *feature* reports, strips at x=0 and x=64) blanks the panel and holds for 3+ minutes, including through headset power-on. Black OLED pixels emit no light, so they do not wear. |
| Can we refresh the black frame periodically? | **Not often.** Re-sending every 2 s made the stock screen visibly flash through now and then. Send it once; re-send rarely. |
| What brings the stock screen back? | A **volume change from the headset wheel** — the firmware shows its popup and then stays on the stock screen. That change is observable: `07 25 <atten>` interrupts plus byte 3 of the `06 20` reply. |
| `06 95` | Returns to the stock screen immediately. |
| Unknown | Whether ANC / transparency / mic-mute button presses appear in `06 b0`, whether PC-side volume changes pop the stock screen, and whether a `06 b0`/`06 20` poll while dark relights the screen. The last one is checked first during implementation (see Testing). |

## Behaviour

Timings: **lit period** `T` = 10 s by default (setting, 3–600 s); **status poll** every 1 s; **safety
re-blank** every 10 min while dark.

1. **Blank** = draw one black frame. **Show** = send `06 95` (stock screen).
2. **Our events** — changes only gglinux can see, derived by diffing consecutive `06 b0` status
   reads, plus screen-setting changes made from gglinux: **Show**, then blank after `T`.
   - headset power status changes (offline / charging / online — this covers charging start/stop);
   - headset battery drops a step (eighths) while at or below 25 %;
   - spare pack inserted or removed;
   - ANC / transparency / mic status change (if `06 b0` reflects them — to be confirmed);
   - any unsolicited `07 ..` report **other than** `07 25` (e.g. `07 bd` ANC), since nothing
     confirms the firmware shows those on its own — Show is harmless if it already does;
   - any screen setting changed through gglinux (so a brightness change is visible).
3. **Firmware events** — the one change the base station is verified to show on its own: volume,
   seen as a `07 25` report or a change in the `06 20` volume byte. gglinux sends nothing (the
   popup is already up) and blanks `T` after the *last* such event.
4. **Safety net:** while dark, re-blank every 10 min, covering stock-screen returns gglinux cannot
   observe. Worst case: one brief flash per 10 min, or a lit screen for up to 10 min.
5. **Device opened** (daemon start, replug): Show, then blank after `T`.
6. **Daemon shutdown** (SIGTERM/SIGINT — logout, shutdown): Blank as the last device action, so an
   unattended, still-powered base station stays dark.
7. **Protection disabled** from the UI: Show and stop blanking. Re-enabled: behave as (5).
8. **Brightness** (`1..=10`) is sent only when the user sets it; gglinux never pushes a default,
   since the current level cannot be read back.

## Architecture

```
gg-device-headset      protocol: OLED packets, audio-settings read, event-preserving poll
        │
gglinuxd/src/headset.rs         worker thread: owns the HID handle, polls, reads events,
        │                        runs the policy, executes its actions, serves IPC commands
        └── headset/screen.rs   screen policy: pure state machine, no I/O, time passed in
gg-ipc                 GetHeadsetState / SetHeadsetScreen / Event::HeadsetChanged
ui/src/lib/HeadsetPanel.svelte   battery + "Screen burn-in protection" card
```

### `gg-device-headset` additions

- `proto`: `oled_blank_packets() -> [Vec<u8>; 2]` (1024-byte feature reports: `06 93 <x> 00 40 40`
  + zero body, x = `0x00`, `0x40`), `oled_return_to_ui_packet()` (`95`), `oled_brightness_packet(lvl)`
  (`85 <lvl>`, rejects outside `1..=10`), `audio_settings_packet()` (`20`) and a decoder for the
  volume at byte 3 of the `06 20` reply (`0..=56` attenuation).
- `Headset`: `oled_blank()` (two `send_feature_report` calls), `oled_show_stock()`,
  `set_oled_brightness(lvl)`, and **`poll() -> Poll { status, volume, events }`**, which asks for
  `06 b0` and `06 20` and **returns every unrelated report it read** (drained or skipped) in
  `events`, instead of discarding them as `status()` does today. `status()` keeps its current
  behaviour for the examples. Also `read_event_timeout(ms)` so the worker controls its own latency.

### `gglinuxd/src/headset/screen.rs` — the policy

A pure state machine. States: `Lit { until }`, `Dark { last_blank }`, `Disabled`.

```rust
pub enum Input { DeviceOpened, OurEvent, FirmwareEvent, Tick, Settings(ScreenSettings), Shutdown }
pub enum Action { Blank, ShowStock }
impl Screen {
    pub fn new(settings: ScreenSettings, now: Instant) -> Self;
    pub fn on(&mut self, input: Input, now: Instant) -> Option<Action>;
    pub fn is_dark(&self) -> bool;
}
```

Transitions follow the Behaviour list: `OurEvent` → `ShowStock` if dark, `Lit{now+T}` either way;
`FirmwareEvent` → `Lit{now+T}` with no action; `Tick` → `Blank` when a lit period expires or the
safety interval passes; `Shutdown` → `Blank` unless disabled.

The rule that turns two status reads into "our event" lives next to it as
`fn wake_reason(prev: &Status, next: &Status) -> Option<WakeReason>` — also pure, so the battery
thresholds are unit-tested directly.

### `gglinuxd/src/headset.rs` — the worker

Same shape as the mouse worker (dedicated OS thread, `Command` enum with oneshot replies, cloneable
`Handle`) but, unlike the mouse, it **keeps the device open**, because it must hear interrupts.
Loop: handle pending commands (non-blocking) → `read_event_timeout(100 ms)` → every 1 s `poll()` →
feed the policy (`FirmwareEvent` for `07 25` or a `06 20` volume change; `OurEvent` for any other
`07` report or a `wake_reason` hit; `Tick`) → execute the returned action. While disconnected, retry opening every 2 s; any HID error
drops the handle and the next successful open feeds `DeviceOpened`. Commands: `GetState`,
`SetScreen(ScreenSettings)`, `Shutdown` (blank, then reply). State changes are broadcast as
`Event::HeadsetChanged`.

`main.rs` spawns the worker next to the mouse's and, on SIGTERM/SIGINT, sends `Shutdown` and waits
up to 1 s for the reply before exiting.

### IPC (`gg-ipc`)

```rust
pub struct ScreenSettings { pub protect: bool, pub lit_seconds: u16, pub brightness: Option<u8> }
pub struct HeadsetState {
    pub connected: bool,
    pub power: HeadsetPower,              // offline | charging | online | unknown
    pub headset_battery: Option<u8>,      // % — None while offline
    pub spare_battery: Option<u8>,        // % — None when no spare seated
    pub screen: ScreenSettings,
    pub screen_dark: bool,
}
Request::GetHeadsetState                  -> ResponseData::Headset { state }
Request::SetHeadsetScreen { settings }    -> Success | Error   (validates 3..=600 s, 1..=10)
Event::HeadsetChanged { state }
```

Defaults at daemon start: `protect: true, lit_seconds: 10, brightness: None`.

### UI

`HeadsetPanel.svelte`, mounted for `kind === "headset"` the way `MousePanel` is for the mouse:
headset and spare battery, power state, and a **Screen burn-in protection** card — on/off toggle,
"stay lit for N s" slider, brightness slider (1–10; "unchanged" until first moved) — plus a line
showing whether the screen is currently dark.

### Docs

`docs/PROTOCOLS.md`: `07 25` is the volume wheel position (not a level meter — correct the
2026-08-12 note); OLED findings from the table above; `06 20` byte 3 = volume. `PLAN.md`: M2
progress note.

## Error handling

- Base station absent: `HeadsetState.connected = false`, battery fields `None`; the worker keeps
  retrying quietly (debug-level logging only, no warning spam every 2 s).
- A failed blank/show write: log a warning, drop the handle, reopen; the policy re-issues on the
  next `DeviceOpened`, so the screen converges to the right state.
- `SetHeadsetScreen` with out-of-range values: `Error` with a message, settings unchanged.
- Shutdown with no device: reply immediately; never delay exit by more than 1 s.

## Testing

**Unit (no hardware):**
- packet builders: exact bytes and lengths (two 1024-byte strips with the right headers; brightness
  range; `06 20` volume decode from the captured reply `06 20 00 06 01 …` → 6);
- `Headset::poll` event preservation: the reply-vs-event split is a pure function
  (`classify(report, awaited_opcode) -> Reply | Event`) and is unit-tested on captured reports,
  including a `07 25` arriving before the `06 b0` reply;
- policy with an injected clock: open → show → blank after `T`; our event while dark → show +
  relight; firmware event while dark → relight with no action, blank `T` after the last one;
  repeated firmware events extend the lit period; safety re-blank at 10 min; disable → show and
  never blank; shutdown → blank (and nothing when disabled);
- `wake_reason`: power change, battery steps above/below 25 %, charging flip, spare in/out, no
  reason on identical reads;
- IPC JSON shapes for the new request/response/event.

**Hardware (someone watching the screen):**
1. *First, before building on it:* with the screen black, poll `06 b0` + `06 20` every second for
   60 s — the screen must stay black. If it relights, detection falls back to interrupts only.
2. Daemon start → stock screen → black after 10 s.
3. Headset wheel → volume popup → black 10 s after the last notch.
4. ANC and mic-mute presses → do they wake the screen? (Resolves the open question.)
5. Change brightness and lit period from the UI → screen shows at the new brightness.
6. `systemctl --user stop gglinuxd` → screen stays black.
