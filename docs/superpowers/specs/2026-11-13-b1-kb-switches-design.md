# b1-kb-switches: switch tables as GG sends them, paced reports, verified binding layers

Unit b1 of M11 wave B. Apex Pro TKL Gen 3, firmware 1.19.7. No new opcode: everything here was already on the
allowlist.

## What

1. **Pacing.** Every report to the keyboard (feature and output, all through `Keyboard::send`) keeps at least
   `REPORT_GAP` = 5 ms from the one before on that open handle. The gap counts from the end of the last send, failed
   sends too. A report the allowlist refuses neither waits nor counts. Reads are not paced.
2. **Verified layer writes.** `Keyboard::write_bindings` writes the layer (`36 <layer>`), waits `READ_SETTLE`
   (13 ms), reads it back (`B6 <layer>`) and compares. On a mismatch it writes again, `WRITE_TRIES` = 3 writes in
   all, then fails with "the keyboard didn't keep the <normal|Fn|deep-press> bindings after 3 tries …". A kept write
   costs one write and one read. The daemon records a layer as written only after a write that read back right, so
   a layer the keyboard never kept goes again with the next change. This covers all three layers, the writes on
   connect, and the deep-layer rewrite after a points table (`rewrite_deep`).
3. **Only the table that changed.** `SwitchReports` holds the five switch reports, built: actuation `38 61 44 00`,
   release modes `38 62`, sensitivity `38 65`, Rapid Tap pairs `38 67`, Rapid Tap switch `38 66`.
   `Keyboard::send_switches(&reports, &mut SentSwitches)` sends only those whose bytes differ from what this
   connection last sent, and remembers each one as it goes. After a connect or a slot load (`forget()`) the memory
   is empty, so all five go, as before. A failed send is forgotten, so that report goes again with the next change.
   The deep-press points table keeps its own rules (`second_actuation_table` is unchanged).
4. **Keys the board doesn't have.** `proto::actuation_table_for` / `release_mode_table_for` take the board's keys.
   An analog key the board lacks gets `ABSENT_POINT` = `2f 2a` and release mode `ABSENT_RELEASE` = `00`, whatever
   the settings. `SwitchesState::sync_board(device, board)` takes the board from the worker; `sync(device)` is the
   US board, which is also what an unknown region gets.

## Sources

- Pacing: `hwtest/MANUAL-ACCEPTANCE.md` ("Fix build and the second stuck key"): a deep-layer write 0 ms after the
  points table was dropped 2 of 3 times, 2 ms or more after it kept 9 of 9. GG leaves about 15 ms between its
  reports (`captures/GG-CAPTURES-2026-11-13.md`, the slot write at ~15 ms a chunk).
- Dropped layer writes: the same log: 4 of 16 deep-layer writes through the daemon did not stick, 1 of 32 with a
  bare probe.
- Only the changed table, and `2f 2a` / `00`: our capture `apex-10.pcap` (GG 123): 19:20:44 all keys 1.5 mm sends
  only `38 61 44 00` with `1e 1b` × 61 and `2f 2a` × 7 (`32 64 87 88 89 8a 8b`); 19:20:52 Rapid Trigger on sends only
  `38 62` with `02` × 61 and `00` × 7; 19:21:00 off and 19:21:09 1.8 mm (`28 25`) the same way. The tests compare
  gglinux's tables with these bytes, all 68 blocks.

## Decisions

- **5 ms, not 15 ms.** More than the 2 ms that kept every write, and less than the 13 ms a read already waits, so
  reads and the slot transaction (13 ms between chunks) get no slower on the real keyboard. The lighting frame every
  33 ms is never delayed by more than 5 ms.
- **The gap is per `Keyboard`**, not process-wide like the mouse's: the worker holds one open handle for the whole
  connection.
- **Read back once, a settle after the write.** The spec asked for exactly one write and one read for a kept write.
  A keyboard that still showed the old layer on that read would cost one extra write (tested with a lagging fake),
  never a wrong result.
- **Compare blocks as a set.** Both lists hold each key of the layer once, so order doesn't matter.
- **A failed layer write leaves `written` as it was** (the last layer read back right, or the keyboard's own read).
  That is what the keyboard still shows, so a later change back to it rightly sends nothing, and any other change
  writes again.
- **Sensitivity (`38 65`) keeps the user's value for absent keys.** GG never sent that table in our captures, so
  there is no GG value to copy. Rapid Tap pairs are unchanged too.
- **ISO and JIS boards** get the same rule for the analog keys they lack (ISO: `31 87 88 89 8a 8b`; JIS: `31 64`).
  Only the US board's tables were captured; on the others the rule is applied, not seen from GG.
- **The fake keyboard** can now hold the three layers (`hold_layer`): a `36` write shows to reads asked 13 ms or more
  after it; layer writes can be dropped (the next N, or each that comes less than a given gap after the report
  before); a slot load can replace a layer from the second read on. It also keeps when each report came
  (`sent_at`).
- **Test time.** The slot-transaction tests in `onboard.rs` loop over 160 full slot writes with zero waits. With
  the pace on, that binary takes ~64 s instead of ~2 s. `Keyboard::set_report_gap` (tests and the `testing` feature
  only) lets those fakes turn it off; the call goes in `onboard.rs`, which this unit doesn't own.

## Not checked on hardware

1. **Only the changed table.** `ggctl switches on`, then `ggctl switches actuation 1.8`: the daemon log should say
   `keyboard switch settings sent reports=1` (the first time on a connection it's 5). Keys feel the new point.
2. **Absent keys.** Nothing to feel on a US board; the bytes match GG's capture.
3. **Verified writes.** `ggctl experimental keyboard_bindings on`, `ggctl bind on`, `ggctl bind capslock b`: Caps
   types `b`, and the log has no "didn't keep a bindings write" warning (if it has one, a dropped write was caught and
   written again). Ten toggles between `ggctl bind capslock b` and `ggctl bind capslock default` should all hold.
4. **Pacing.** Not visible; it shows as the writes above holding.

Put back: `ggctl bind capslock default`, `ggctl bind off`, `ggctl experimental keyboard_bindings off`,
`ggctl switches off`, then unplug the keyboard and plug it back in (or `ggctl onboard load 1` with
`keyboard_onboard` on) to bring back the keyboard's own switches.
