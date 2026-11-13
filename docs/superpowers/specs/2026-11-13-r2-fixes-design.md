# r2-fixes: review R2's majors S1, S3 and H1

Unit r2-fixes of M11 wave C. Fixes three findings of the read-only review `reviews/R2-wave-b.md` before the owner's
hands-on session. No new opcode, no new request, no new gate: everything sends reports that were already on the
allowlist, behind the same gates as before.

## What

1. **S1: switch tables gglinux thinks were sent.** Since b1, a change sends only the switch reports whose bytes
   differ from what this connection last sent. That memory can go stale without gglinux seeing it (a config picked
   on the keyboard's own menu, GG, a report the keyboard dropped without a word; switch tables have no read-back).
   Now `SwitchesState` also remembers when all five last went (`full_at`), and sends all five again:
   - when a `SetSwitches` turns them on (they were off), even with nothing else changed;
   - with the first change `FULL_SEND_AGE` = 5 minutes or more after the last time all five went.
   A whole send that fails part-way goes whole again next time. A connect or a slot load clears the memory as
   before.
   - `ggctl switches resend` reads the switch settings and sends them back twice, unchanged: off, then on. The
     second one turns them on, so the daemon sends all five. With the settings off it refuses ("nothing to send
     again"). If the second request fails it says so and names `ggctl switches on`.
2. **S3: a layer write that fails part-way.** `Keyboard::write_bindings(layer, blocks, kept)` now takes `kept`, the
   layer the keyboard is known to hold (what gglinux last wrote and read back, else the keyboard's own read).
   - Every try reads the layer back, also after a failed send (it may have reached the keyboard whole or in part).
     A layer that reads back as written counts as kept, even after a send error.
   - A failed send ends the tries; a layer that reads back different, or can't be read back, is written again, 3
     writes in all, as before.
   - If it still isn't kept, `kept` is written once and read back, and the error is a `NotKept { layer, why,
     put_back }`. Its text says which state the layer is in: "gglinux wrote back the normal bindings the keyboard
     had before, and it reads back with them again", or "… didn't check out either, so its normal layer may be a mix
     of old and new bindings: unplug the keyboard and plug it back in".
   - Both block lists and the allowlist are checked before anything goes out, so a refusal sends nothing, as before.
   - The daemon (`Store::write_layer`) records a layer as written only after a kept write. A layer whose write-back
     didn't check out is marked unsure: the next change writes it again even if it's what gglinux last knew was
     there (otherwise "back to the keyboard's own" would send nothing and leave the mix). A reconnect or slot load
     clears the mark.
   - `apply` still stops at the first layer that fails, and its error adds what the other layers hold: "The normal
     layer has the new bindings." / "The deep-press layer wasn't written: it keeps the bindings it had."
3. **H1: the app's stream-mix sliders.** A slider moved sends `LineOutMix` with the moved level and the other two
   as the daemon last took them from this app (`HeadsetForm::mix_written`, set only on a success answer). A level
   never written in this run of the app goes out at 100. The sliders still show the read-back; it's for display
   only.
   - The memory lives in the headset form and survives the daemon going away and coming back (`follow(None)`,
     `Forms::reset`): the base station keeps its levels while the daemon restarts. It's gone when the app quits.

## Sources

- S1, S3, H1: `reviews/R2-wave-b.md` rows S1, S2, S3, H1 and checklist steps K-S, K-B and H-2.
- Only the changed switch table goes (kept for changes within 5 minutes): capture apex-10 (GG 123), as in b1.
- 100 for an unwritten stream level: GG's own connect push `47 64 00 64 64` (capture arctis-1). `0x64` = 100.
- Why the read-back can't be trusted for writing: the only captured `06 20` reply (arctis-5) has `64` at bytes
  0x14–0x16 and 0x18–0x19, so the main/aux/mic places (0x16, 0x18, 0x19) rest on Arctis Sound Manager alone.

## Decisions

- **No resend on a timer.** "Older than 5 minutes" makes the *next change* send all five; nothing goes out on its
  own. A timer would send switch tables while the owner types or plays, with nobody having asked, on the path that
  has been suspended since the phantom keys (MANUAL-ACCEPTANCE). The heal for a keyboard changed behind gglinux's
  back without any change in gglinux is `ggctl switches resend` (or `switches on` after `off`).
- **Turning on, not "the same settings again", triggers the full send.** The app sends `SetSwitches` on every
  control change; a rule like "identical settings mean resend" would send all five on an unchanged click. Turning
  on is a clear user act.
- **`resend` as off + on in ggctl, not a request.** gg-ipc isn't this unit's. Between the two requests the
  settings are off for a moment; that sends nothing to the keyboard (off only stops gglinux sending). A proper
  `ResendSwitches` request would avoid the two saves and the off/on events the app sees; snippet in the report.
- **The deep-press table follows the full send** as it follows every switch send today (`second = None` in
  `send`). S4 (send it only when the points change) is not done: nothing shows yet whether the keyboard keeps the
  second points when the primary table is written, so it stays as b1 left it.
- **Always write `kept` back after a failure**, even when the last read-back already showed it: a write that lands
  late (the keyboard answering the first read with the state from before) would otherwise stay. It costs one write
  and one read, only on the failure path.
- **A failed send ends the tries.** A transport that errors is often a keyboard going away; writing twice more
  would only add errors. The read-back and the one write-back still go, so a keyboard that is still there is left
  in a known state.
- **A read-back error is a failed check** (written again, up to 3), not an instant failure as before: a stale read
  slot is the usual cause on the owner's keyboard, and writing a layer again is harmless (RAM only).
- **H1: one memory for the whole mix, not per level.** Every `47` report carries all three levels, so after the
  first write all three are "written by gglinux". Only a success answer updates it: a refused change (gate off,
  no base station) wrote nothing.
- **H1: what `ggctl headset stream-mix` writes isn't known to the app.** The app can only remember its own writes;
  a level written by ggctl goes out as the app last wrote it (or 100) when a slider moves. Once a capture with
  three different levels proves the reply's places (checklist H-2), the read-back can be used again and this memory
  dropped.

## Tests (each seen failing first)

- `switches.rs`: `turning_them_on_sends_all_five_even_with_nothing_changed`,
  `a_change_five_minutes_or_more_after_the_last_full_send_sends_all_five` (fake clock: `sync_at(device, now)`),
  `switches_turned_off_and_on_again_go_to_the_keyboard_whole` (through the worker, what `resend` sends).
- `ggctl/keyboard.rs`: `resend_turns_the_switch_settings_off_and_on_again_unchanged`.
- `gg-device-keyboard/bindings.rs`: a test transport `PartWay` on the fake keyboard keeps only half of chosen layer
  writes, fails a send after half or after all of it, or drops one:
  `a_layer_the_keyboard_drops_three_times_is_written_back_as_it_was_and_named`,
  `a_layer_kept_only_in_part_every_time_is_written_back_whole`,
  `a_layer_left_in_part_that_cant_be_written_back_is_called_a_mix`,
  `a_send_that_fails_part_way_is_read_back_and_the_layer_written_back_once`,
  `a_send_error_after_the_keyboard_took_the_layer_counts_as_kept`.
- `gglinuxd/keyboard/bindings.rs`: the dropped-three-times, half-way and deep-layer tests now expect the write-back;
  `a_layer_that_may_be_a_mix_is_written_again_with_the_next_change`,
  `a_failed_layer_write_says_what_each_layer_holds_now`.
- `gglinux-ui/engine.rs`: `a_stream_level_goes_out_with_the_levels_gglinux_last_wrote_never_the_read_back`.
