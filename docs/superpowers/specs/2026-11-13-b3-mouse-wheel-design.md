# b3-mouse-wheel: scroll-wheel actions on the Rival, and honest answers from a sleeping mouse

Unit b3 of M11 wave B. Rival 3 Wireless Gen 2 (`1038:1872`) only.

## What

**A. Wheel actions.** GG 123 lists eight actions for this mouse: buttons 1–6, Scroll Up and Scroll
Down. The wheel pair is its own command.

- `SetMouseButtons.actions` takes 6 names (as before) or 8: buttons 1–6, then the wheel turned up,
  then turned down. With 6 the wheel is left alone (no `1A`). With 8, `19` goes as before, `1A`
  after it (paced by `COMMAND_PACE`, 35 ms), then `09` if `save`.
- The wheel takes only three actions, the ones a capture shows: `scrollup` (`31`), `scrolldown`
  (`32`), `button4` (`04`). Everything else in the buttons table (other buttons, keys, media keys,
  DPI, disabled) is refused with "“f5” is not known for the wheel yet: it takes scrollup,
  scrolldown, button4". The table is `gg_device_mouse::proto::WHEEL_ACTIONS`, one row per action
  with the capture that proves it; the app's and ggctl's names are `gg_ipc::mouse_keys::WHEEL`, and
  a gglinuxd test checks the two agree.
- New gate `Feature::MouseWheel` (`mouse_wheel`, "Mouse: scroll wheel actions"), off by default.
  While it's off, 8 names ending in the factory pair (`scrollup`, `scrolldown`) send exactly what 6
  names send (byte-identity test: no `1A`); any other wheel pair is refused with the contract's
  sentence. While it's on, 8 names always send `1A`, the factory pair included: that is how the wheel
  is put back.
- `MouseSettings.buttons` records what went out: 8 names after a write with the wheel, 6 after a
  write without it, keeping the wheel's two from the record before if there were any.
- ggctl: `ggctl mouse buttons b1,b2,b3,b4,b5,dpi[,up,down]` and the short form
  `ggctl mouse wheel <up> <down> [--save]`, which keeps the six buttons as the daemon applied them
  (the factory six if it applied none). `ggctl mouse` prints a separate `wheel:` line.
- App: two more rows under BUTTONS, "Scroll up" and "Scroll down", each a menu of the three proven
  actions, greyed with the refusal sentence while the gate is off. With the gate off the app sends
  the six buttons only (today's request). "Reset to factory" resets the wheel too.

**B. A sleeping mouse** (found on hardware 2026-11-13: `ggctl mouse sleep 20` sent to an asleep
mouse answered "ok", the setting never arrived, and `ggctl mouse` kept showing the old report).

- Every write (`SetMouseDpi`, `SetMousePolling`, `SetMouseButtons`, `SaveMouse`, `SetMouseSleep`,
  `SetMouseRadio`) starts with a status read made for that request: the battery query `AA 01`. No
  battery reading (no answer, or the dongle's all-zero answer for a sleeping mouse) means asleep:
  nothing more is sent, and the answer is `gg_ipc::MOUSE_ASLEEP`, "the mouse is asleep: move it,
  then try again". ggctl exits non-zero; the app shows it in the card's error line.
- While asleep, the mouse's own readings (`sleep_secs`, `radio`, `reported_dpi`,
  `reported_polling_hz`) are **cleared**, by the poll and by a write that finds it asleep. The next
  poll after it wakes reads them again. `ggctl mouse` then prints "settings: not known while it's
  asleep; it reports them once it wakes"; the app's note says "The mouse is asleep, so its settings
  aren't known until it wakes. Move it to wake it."
- After a write, a setting the mouse can report is read back: DPI (`AC`), polling rate (`97`),
  sleep timer (`AB`), wireless mode (`AE`; `97` and `AE` only believed after an `AC` that answered,
  as zeros would read as 1000 Hz and High-Efficiency Mode). The read waits for the pace after the
  write, as every command does (3 s after `17` or `2E`). If the answer is still something else, it
  is asked once more after 300 ms (`READ_AGAIN`), since the mouse can answer late; still wrong, the
  answer is "the mouse didn't take the polling rate; try again" (or DPI stages, sleep timer, wireless
  mode) and the setting isn't recorded as applied. No answer at all: "the mouse didn't answer after
  the change, so it isn't known whether it took the …: move it, then look again". Buttons and wheel
  have no read: they can't be checked (said in the code and the request's docs).

## Sources

- Wheel: our own GG 123 capture `~/projects/gg-m11/captures/rival-5.pcap` (2026-11-13), decoded with
  `lab/usbpcap_hid.py --dev 2 --bytes 64`: on connect `2c …`, `19 01 00 00 00 00 02 …`,
  `1a 31 00 00 00 00 32` + zeros; Save with Scroll Up = Button 4: `1a 04 00 00 00 00 32` + zeros, then
  `09` 31 ms later; back: `1a 31 00 00 00 00 32`, `09`. The whole 64-byte report is the eleven bytes
  and zeros. Written up in `GG-CAPTURES-2026-11-13.md` ("Rival 3 Wireless Gen 2") and
  `docs/PROTOCOLS.md` §1 "Rival: more from GG 123".
- Asleep: `hwtest/HW-TEST-LOG.md` (2026-11-13, 14:39–15:45): an asleep mouse answers no query; a
  write to it is lost. The read-back commands are §1a of `docs/PROTOCOLS.md` (built in M11 a1, read
  back on hardware: "Three reads identical", "every setting read back from the mouse").

## Decisions

- **Cleared, not marked.** The unit allowed either clearing the readings or a new `MouseState`
  field "as last reported before it fell asleep". Clearing is what `MouseState`'s docs already
  promised ("`None` … or the mouse is asleep"), needs no wire change, and works for every client as
  it is: an older ggctl or app that doesn't know a new field would have kept showing the old values
  as current, which is the bug. What's lost is a hint of what the mouse had; the settings in its
  memory do survive sleep (HW log, 15:20), and they're read again within one poll of it waking.
- **The app hides an asleep mouse's readings too.** The battery-only notification (`MouseBattery`)
  can't clear the app's copy of the readings, so the app treats "connected, no battery" as asleep
  and neither shows nor seeds from them.
- **Read-back only where reads are allowed.** The settings reads are gated by `mouse_power` (as
  since M11 a1). With it off, DPI and polling writes go out as before and aren't read back; only the
  battery query is new before them. The sleep timer and radio need the gate anyway.
- **One retry for a late answer.** Real devices answer late (HW log), and the fakes lag on purpose
  (brief: the first read after a change gets the old state). One second read 300 ms later separates
  "late" from "dropped" without slowing a normal write (one read when the first answer is right).
- **The six-entries request stays the default.** The app adds the wheel's two only while
  `mouse_wheel` is on, so with it off every buttons request is byte for byte what it was.
- **Wheel entries in either slot.** The unit offers the three proven entries on both rows. Literally
  seen: `31` and `04` in the up slot, `32` in the down slot. Button 4 on Scroll Down and swapped
  scroll directions are seen entries in the other slot, not seen combinations (see "Unverified").
- **Testing seam.** `gg_device_mouse::Transport` (implemented for `HidDevice`) and
  `testing::FakeMouse`: a fake that runs settings, answers queries with 64-byte reports, lags (first
  answer after a change is the old one), drops a chosen command, and sleeps (takes nothing, answers
  zeros). The daemon reaches the mouse through a `Port` (hidapi, or the fake in tests), so no test
  can open the real dongle. The fake records each pace instead of sleeping, so tests stay fast.
  `testing` is compiled in normal builds: the crate's `Cargo.toml` isn't this unit's (see report).

## Unverified on hardware, and a two-minute check

Not tried on the owner's mouse: the `1A` write from gglinux, the asleep refusal, and the read-back
errors. To check by hand (gate on: `ggctl experimental mouse_wheel on`):

1. `ggctl mouse wheel button4 scrolldown` → "ok" and "note: not saved". Turning the wheel up now goes
   back in a browser (Button 4); down still scrolls. `ggctl mouse` shows `wheel: up button4, down
   scrolldown`.
2. Put it back: `ggctl mouse wheel scrollup scrolldown` → the wheel scrolls up again.
3. Let the mouse fall asleep (or `ggctl experimental mouse_power on` and `ggctl mouse sleep 1`, then
   leave it still a minute and a half), then `ggctl mouse polling 1000` → "Error: the mouse is
   asleep: move it, then try again", exit status 1; `ggctl mouse` shows "settings: not known while
   it's asleep". Move it, wait a few seconds, `ggctl mouse` shows the settings again. Put the timer
   back with `ggctl mouse sleep 20`.
