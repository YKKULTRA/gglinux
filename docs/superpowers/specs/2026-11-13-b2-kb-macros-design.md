# B2 kb-macros: keyboard macros, written into an onboard slot as GG 123 does

Unit b2-kb-macros of M11 wave B. Built against fakes only: nothing here has been sent to a keyboard. Writing needs two
experimental gates, `keyboard_macros` and `keyboard_onboard` (both off by default); changing the list needs
`keyboard_macros`.

## What

- `gg-device-keyboard/src/macros.rs`: the codec. `read(&Image)` gives the macros a slot holds, by key;
  `write(&Image, &[SlotMacro])` gives the image with new ones. `place(code)` is a key's place in the firmware's key
  order. `factory_like(name)` (with the `testing` feature) is a factory-like image for tests and the fake daemon.
  New `Image` accessors in `onboard.rs`: `normal_bindings`, `fn_bindings`, `second_bindings`, `set_macros`.
- `gg-ipc/src/macros.rs`: the contract's types, refined: `MacroLibrary::validate` (messages meant for the user, the
  limits below), the steps as words (`Macro::parse_steps`, `Macro::steps_text`), key names as `ggctl bind` takes them,
  and `MacrosState::{backup, note}` (both `#[serde(default)]`, so older states still read).
- `gglinuxd/src/keyboard/macros.rs`: `macros.json` (the library and the slot last written), `SetMacros` (check, save,
  send nothing), `WriteMacros` (the onboard slots' checked transaction with a change that touches only the macro area,
  the bound keys' places and the CRC), `busy` and `MacrosChanged`.
- `ggctl macro …`, the fake daemon's `keyboard_macros.rs`, and a MACROS panel at the end of the keyboard card.

## Source

Our own GG 123 captures (CONTRIBUTING: "a published source or our own GG capture"), notes in
`~/projects/gg-m11/captures/GG-CAPTURES-2026-11-13.md` → "Keyboard macros (slot image, three SAVEs of Config 5)",
summed up in `docs/PROTOCOLS.md` §3f. The three saved images `apex-20.slot5-gg-macro.bin`,
`apex-21.slot5-gg-2macros.bin`, `apex-22.slot5-gg-repeat.bin`, diffed against the factory image of the same slot
(`~/backups/gglinux/onboard-2026-11-13/slot5-20261113T103122Z.bin`):

- macro area at image offset 2704 (Apex Control's opaque 9 600 bytes, `at::MACROS`), macros back to back, the rest
  zero in all three;
- header `77 <mode> <count u16 LE> 01 01 0f 00`; mode `01` Play Once, `03` Repeat While Pressed;
- steps: key `02 <usage> 01 <01|00>`, wait `04 00 <ms u16 LE>`, mouse button `00 <button> 00 <01|00>` (button 1 only);
- binding on the normal layer at `80 + 5 × place`: `71 00 00 <start u16 LE, in 4-byte words>`; O at place 23, P at
  place 24, which is Apex Control's `deviceKeyIndexToHID` (`evidence/apexkit-f7db8c5/Key.swift`);
- a second macro stored first pushed the other to word 20 (`00 14` = 80 bytes = 8 + 18 × 4).

The macro areas and bound places gglinux makes from the equivalent library are byte for byte GG's (tests
`one_macro_on_p_is_ggs_first_save_byte_for_byte`, `two_macros_on_o_and_p_…`, `repeat_while_held_on_p_…`, with GG's
bytes as hex constants), and decoding GG's bytes gives that library back. Checked by hand once more, outside the
tests: putting GG's area and places into the factory image and working the CRC out again gives GG's image except at
the unrelated fields GG also rewrote (107 bytes: blank places 68/69/93 and the Fn layer's `51` → `00`, zone bytes at
1152–1233, 1584–1585) and the CRC; the CRC function reproduces GG's stored CRCs.

## Decisions

1. **Only what GG's saves show is offered.** Steps: keys (keyboard usages `04`–`E7`), waits of 1–65 535 ms, and the
   left mouse button only: whether the button byte counts buttons or is a bit mask isn't known, so `click:2` is
   refused ("only the left mouse button (click:1) is known…"). Modes: once, while held. Keys: the normal layer only.
   At most two keys with a macro (GG's saves show two). Anything else is refused with a plain message.
2. **One copy per key.** The play mode is in the macro's header, so each bound key gets its own copy in the area.
   Unbound macros live only in `macros.json`; they take no room in the slot.
3. **Copies go in by the keys' places** (O's before P's), which is GG's order in apex-21/22, so the same library
   always gives the same bytes and a second write of it changes nothing (no erase).
4. **A slot gglinux can't read is never rewritten.** Before the change, the slot's own macros are decoded; any other
   mode, kind of step, header bytes, a binding pointing where no macro starts, other bytes before the start, bytes
   after the last macro, or a `71` on the Fn or deep-press layer refuses the write with "this slot holds macros
   gglinux can't read yet (…), so gglinux leaves it as it is" before the erase. A slot with GG-made macros gglinux
   *can* read is rewritten with the library (the panel's sentence says the write rewrites that slot's macros, and a
   backup is kept).
5. **Keys whose macro is gone type themselves again** (`51 <key> 00 00 00`, the factory binding). A key that had a
   remap before it got a macro doesn't get the remap back.
6. **Only the macro area, the bound keys' places and the CRC change.** GG's unrelated rewrites (blank places, zone
   bytes) are not copied. The rest of the area is zeroed, as GG leaves it.
7. **The write is the onboard slots' transaction** (`Keyboard::write_slot`: read, backup saved and read back, change,
   erase, 25 chunks, `B1`, read-back, put back on any failure). The backup goes to `onboard-backups/` with the onboard
   module's own writer (`save_backup`, now `pub(super)`).
8. **The slot the keyboard runs is loaded again** (`B2`) after a write that changed it, as the onboard module knows
   it (`OnboardState::active`, set by `ggctl onboard load`); then the bindings and switches modules hear what a load
   tells them (`bindings::on_slot_loaded`, `switches.forget()`). When the running slot isn't known (after a connect),
   nothing is loaded and the note says `ggctl onboard load N`.
9. **Steps as words** are shared by ggctl and the app (`Macro::parse_steps`): `a`, `ctrl+c` (down in order, up in
   reverse), `down:shift`, `up:shift`, `wait:100`, `click:1`, `down:mouse1`/`up:mouse1`, `text:"Hello"` (US layout;
   `\"`, `\\`, `\n` Enter, `\t` Tab; anything the US layout can't type is refused). Text is typed as plain key taps,
   no waits: GG's own Text Macro format isn't decoded. A macro must let go of every key and button it holds by its
   end (no stuck keys).
10. **Key names** are `ggctl bind`'s. gg-ipc holds a copy of that table (bind.rs isn't this unit's); a ggctl test
    compares both tables for every usage and alias.

## Not verified on hardware

Everything: no macro has been written to a real keyboard by gglinux. Open questions the owner's first try answers:
whether the keyboard plays a macro gglinux wrote (it's GG's bytes, so it should), whether `B2` right after the write
makes it take effect without a power cycle, and how fast plain taps of a text step come out (GG's recorded macros
have waits between keys; a `text:` step has none).

**Two-minute check by hand** (slot 5 is the spare one with the factory image):

```
ggctl experimental keyboard_macros on
ggctl experimental keyboard_onboard on
ggctl macro set test 'text:"gg"'
ggctl macro bind p test
ggctl macro write 5          # backs slot 5 up first, then writes
ggctl onboard load 5
# press P in a text editor: "gg" should appear (instead of "p")
ggctl onboard restore 5 ~/backups/gglinux/onboard-2026-11-13/slot5-20261113T103122Z.bin
ggctl onboard load 1
```
