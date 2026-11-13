# B1-KB-SWITCHES report (M11 wave B, unit b1)

Branch `m11-b1-kb-switches` in `~/projects/gglinux-wt/b1-kb-switches`, on top of `aeb0193`.

## Result

`~/projects/gg-m11/check.sh b1-kb-switches` (last run, after the last source change):

```
tests: 852 passed, 0 failed
CHECK OK
```

The fakemouse example's own tests are not built by check.sh; run separately:
`cargo test -p gglinuxd --example fakemouse` → `test result: ok. 4 passed; 0 failed`.

## What changed, for the user

1. **Only the switch table that changed goes to the keyboard**, as GG 123 does (capture `apex-10`). Changing the
   actuation of all keys sends one report (`38 61 44 00`); turning Rapid Trigger on sends only `38 62`; saving the
   same settings again sends nothing. After a connect or a slot load all five go, as before.
2. **Keys the board doesn't have get GG's values**: `2f 2a` in the actuation table and release mode `00`, whatever the
   settings. On a US board these are `32 64 87 88 89 8a 8b`; gglinux's tables for "all keys 1.5 mm" and "Rapid
   Trigger on" now match GG's captured bytes block for block.
3. **Every report to the keyboard keeps 5 ms from the one before** on the open handle (lighting frames, screen,
   switches, bindings, slot transaction).
4. **Every binding-layer write is read back.** If the keyboard didn't keep it, it's written again (3 writes in
   all), then the user gets "the keyboard didn't keep the deep-press bindings after 3 tries: each time the layer read
   back different from what was sent. gglinux tries again with the next change; if this keeps happening, unplug the
   keyboard and plug it back in" (normal / Fn / deep-press). A layer that never stuck isn't taken as written.

## Files

Changed (all owned):
- `crates/gg-device-keyboard/src/lib.rs`: `REPORT_GAP`, `pace()`, pacing in `send`; `SwitchReports`,
  `SentSwitches`, `SWITCH_REPORTS`, `Keyboard::send_switches` (replaces `Keyboard::set_switches`);
  `Keyboard::set_report_gap` (tests / `testing` feature only). The `pub mod` list is untouched (b2 adds
  `pub mod macros;` there).
- `crates/gg-device-keyboard/src/proto.rs`: `ABSENT_POINT`, `ABSENT_RELEASE`, `US_ABSENT`, `actuation_table_for`,
  `release_mode_table_for` (the old `actuation_table` / `release_mode_table` are these with every key on the board);
  byte tests against `apex-10` 19:20:44 / 19:20:52 / 19:21:00 / 19:21:09.
- `crates/gg-device-keyboard/src/bindings.rs`: `write_bindings` now writes, waits `READ_SETTLE`, reads back,
  rewrites up to `WRITE_TRIES` = 3; `READ_SETTLE` is now `pub`.
- `crates/gg-device-keyboard/src/testing.rs`: `FakeTransport::sent_at`, `hold_layer`, `held_layer`,
  `drop_layer_writes`, `drop_layer_writes_closer_than`, `layer_settle`, `load_layer_after`.
- `crates/gglinuxd/src/keyboard/switches.rs`: remembers the five reports per connection, sends only changed ones;
  `sync_board(device, board)` (`sync(device)` = US board); `forget()` clears the memory.
- `crates/gglinuxd/src/keyboard/bindings.rs`: docs; tests moved to a fake that holds the layers, plus new tests for
  drops (normal layer and the deep-layer rewrite after a points table).
- `crates/gglinuxd/examples/fakemouse/keyboard_bindings.rs`: a read right after a change shows it (the daemon reads
  each write back now, so the keyboard's lagging first read is that one).

Added: `docs/superpowers/specs/2026-11-13-b1-kb-switches-design.md` (design note), this report.

## Public API

- `gg_device_keyboard::REPORT_GAP: Duration` (5 ms).
- `gg_device_keyboard::SwitchReports::new(&Switches, has_key: &dyn Fn(u8) -> bool) -> Result<SwitchReports>`,
  `.reports() -> &[Vec<u8>; 5]`; `SentSwitches = [Option<Vec<u8>>; 5]`; `SWITCH_REPORTS = 5`.
- `Keyboard::send_switches(&SwitchReports, &mut SentSwitches) -> Result<usize>` (how many went).
  **Removed:** `Keyboard::set_switches` (its only caller was `switches.rs`).
- `Keyboard::set_report_gap(Duration)`: only with `cfg(test)` or feature `testing`.
- `proto::{ABSENT_POINT, ABSENT_RELEASE, US_ABSENT, actuation_table_for, release_mode_table_for}`.
- `bindings::{READ_SETTLE (now pub), WRITE_TRIES}`; `Keyboard::write_bindings` keeps its signature, now verified.
- Daemon: `SwitchesState::sync_board(Option<&Keyboard>, &[layout::Key])`.
- No IPC change, no new request, no new opcode, no allowlist change.

## Tests added or changed

TDD: the device-crate tests were written first and run red against stubs (10 failed for the right reasons), then
made green; the daemon switches wiring tests were run red against a stub `send` (4 failed), then green.
- lib.rs: the wait arithmetic (`pace`, incl. that a 13 ms read settle and the 13 ms chunk gap add no wait); real
  reports at least 5 ms apart (`sent_at`, a lower bound only); only the changed reports go (1.5 → 1.8 mm, Rapid
  Trigger, Rapid Tap switch, a pair, fresh memory = all five); a failed report goes again next time; a bad pair list
  builds nothing.
- proto.rs: US-board tables equal GG's captured bytes (61 × `1e 1b` + 7 × `2f 2a`; 61 × `02` + 7 × `00`), 1.8 mm,
  Rapid Trigger off.
- bindings.rs (device): kept write = one write + one read, the read ≥ 13 ms after the write, for all three layers;
  one drop → written again and kept; three drops → error naming the layer, layer unchanged; a write straight after a
  lighting frame is kept on a fake that drops writes < 2 ms after another report.
- testing.rs: held layers, settle, drops, loads.
- switches.rs (daemon): only changed reports; refused report goes with the next change; absent keys on US and UK
  boards; updated the three tests that pinned "five reports on every change".
- bindings.rs (daemon): every write sequence now ends with its read back; lagging keyboard costs one more write;
  one drop kept; three drops → error, not taken as written, next change writes again; deep-layer rewrite after a
  points table verified (one drop kept; three drops reported).

## Pictures

None: this unit has no UI.

## Not done / uncertain

- **The worker still calls `sync(device)`, so the board is always US** until the one-line change below lands. On a
  US board that is already right.
- **Test time:** the device crate's test binary takes ~64 s instead of ~2 s, all in `onboard.rs`'s
  failure-injection loops (160 full slot writes with zero waits, now paced). The fix is one line in `onboard.rs`
  (below). Real timing isn't slower: reads settle 13 ms and chunks are 13 ms apart, more than the 5 ms gap.
- **ISO/JIS boards:** the "absent key → `2f 2a` / `00`" rule is applied to their missing keys (ISO `31 87–8b`, JIS
  `31 64`), but only the US board's tables were captured from GG.
- **Sensitivity (`38 65`) and Rapid Tap pairs** keep the user's values for absent keys: GG never sent those reports
  in our captures, so there's no GG value to copy.
- **Read-back compares blocks byte for byte.** If the real keyboard normalises some block (for example a consumer
  code packed another way) a write would read back "different" every time and end in the 3-tries error. Not seen in
  the hardware log (every read-back matched), but not tried with every action type.
- The phantom key presses after switch tables are not explained; sending less, as GG does, is only the safer
  direction.
- Not run on hardware (by rule).

## Changes needed in files I don't own

1. `crates/gglinuxd/src/keyboard.rs`, in `run()` (line 450), so the switch tables follow the keyboard's region:

   ```rust
   // was: state.switches.sync(state.device.as_ref());
   state.switches.sync_board(state.device.as_ref(), board(state.region));
   ```

   After that, `SwitchesState::sync` is unused outside tests; it can stay (tests use it as "the US board") or the
   lead can drop it.

2. `crates/gg-device-keyboard/src/onboard.rs`, `pub mod fake`, `FakeSlots::keyboard` (line ~1260), to win back ~60 s
   of test time (`Duration` is already imported there):

   ```rust
   /// A keyboard on these slots, every gated family off, with no pace between reports:
   /// the slot tests send thousands, and this fake drops none.
   pub fn keyboard(&self) -> Keyboard {
       let kb = Keyboard::with_transport(Box::new(self.clone()));
       kb.set_report_gap(Duration::ZERO);
       kb
   }
   ```

3. Optional, `crates/gg-device-keyboard/src/onboard.rs` `Timing::longest_write`: it counts 2 ms per report; with the
   pace a report can take up to 5 ms when nothing else waits. With `Timing::KEYBOARD` every report already waits
   13 ms, so the estimate is still right; change only if you want the comment exact.

4. `docs/PROTOCOLS.md` §3f, after "GG sends only the table that changed." bullet:

   > gglinux does the same since M11 (b1): it remembers the five switch reports sent on this connection and sends
   > only those that changed (all five after a connect or a slot load), with `2f 2a` / mode `00` for the analog keys
   > the board doesn't have. Every report keeps 5 ms from the one before, and every binding-layer write (`36`) is
   > read back (`B6`, 13 ms later) and written again, up to 3 times.

## For README / PLAN / CHANGELOG

> **Keyboard:** switch settings now go to the keyboard the way SteelSeries GG sends them: only the table that
> changed, with GG's values for the keys your board doesn't have. Every report to the keyboard keeps 5 ms from the
> one before, and key-binding changes are read back from the keyboard and sent again if it didn't keep them (up to
> three times, then gglinux says so).

## Hands-on checks for the owner

All on his Apex Pro TKL Gen 3, with the lead. Watch the log with `journalctl --user -u gglinuxd -f`.

1. **Only the changed table** (switch tables are suspended on his keyboard since the phantom keys; run only with his
   go and the evdev-grab safety net):
   - `ggctl switches actuation 1.5` then `ggctl switches on`: log `keyboard switch settings sent reports=5`.
   - `ggctl switches actuation 1.8`: log `reports=1`. A light press needs a little more travel.
   - `ggctl switches rapid-trigger on`: `reports=1`. `ggctl switches rapid-trigger off`: `reports=1`.
   - Same command twice: the second logs nothing at info level ("unchanged: nothing sent" at debug).
   - Watch for keys typing by themselves for a minute after each.
2. **Verified binding writes:**
   - `ggctl experimental keyboard_bindings on`, `ggctl bind on`, `ggctl bind capslock b`: Caps Lock types `b`.
   - Toggle ten times between `ggctl bind capslock b` and `ggctl bind capslock default`: every `ggctl` answer is
     "ok" and Caps Lock follows. A log line "the keyboard didn't keep a bindings write; writing it again" means a drop
     was caught and fixed; an error answer naming the layer means three drops in a row (please note how often).
3. **Pacing:** nothing to see by itself; it shows in step 2 holding while the lighting runs.

Put back: `ggctl bind capslock default`, `ggctl bind off`, `ggctl experimental keyboard_bindings off`,
`ggctl switches off`, then unplug and replug the keyboard (or `ggctl onboard load 1` with `keyboard_onboard` on) so
it runs its own switch settings again.
