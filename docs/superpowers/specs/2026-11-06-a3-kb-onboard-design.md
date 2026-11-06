# A3 kb-onboard: the keyboard's onboard profile slots and the checked flash write

Unit a3-kb-onboard of M11 (audit K06, K07). Built against fakes only; nothing here has been sent to a keyboard, and
all of it stays behind the `keyboard_onboard` experimental gate (off by default).

## What

- `gg-device-keyboard/src/onboard.rs`: the slot image (`Image::parse/serialize`, accessors, the few setters gglinux
  needs), the STM32 CRC, byte-exact builders for `02`/`03`/`83`/`B1`/`B2`, the chunked read, `Keyboard::load_slot`,
  and `Keyboard::write_slot`, the one checked transaction that can write flash. A test fake, `onboard::fake::FakeSlots`
  (five slots in memory behind `FakeTransport`, lagging on the first read after a change), is built with the
  `testing` feature.
- `gglinuxd/src/keyboard/onboard.rs`: the worker's `Read`/`Load`/`Write` commands, backups on disk, `busy` and
  `OnboardChanged`; `serve` answers the four onboard requests.
- The fake daemon runs the same transaction against `FakeSlots`; `ggctl onboard ...`; the Onboard panel at the end of
  the keyboard card.

## Protocol source

Apex Control, `docs/PROTOCOL.md` at `f7db8c5` (© 2026 Henrique Aron, CC BY 4.0), local copy
`gglinux-audit-2026-11-05/evidence/885690054d3f0e15.txt`:

- "Onboard profiles & the flash filesystem": files `0x80 + slot`; erase `02 01 <fs>` (output → input, up to 6 s);
  write chunk `03 01 <fs> <size u16> <offset u32> <data>` (feature, 500-byte chunks ~13 ms apart); validate
  `B1 <slot>` (output → input); read chunk `83 01 <fs> <size> <offset>` then the feature read `83 <err> <data>`.
- "Image layout (profile schema 3, device schema 8, fw ≥ 1.19.0)": the offsets table, used as published.
- "CRC": STM32 CRC-32, poly `04C11DB7`, init `FFFFFFFF`, no reflection, no final XOR, 32-bit little-endian words over
  the 12 320 body bytes.
- "Writing safely": read-modify-write; never write an image that didn't validate (size, schemas, CRC); pad the last
  chunk `0xFF`; full read-back compare; pause the lighting stream.
- "Queries & misc": `B2 <id>` loads a slot (output, no reply).

## Decisions

1. **The transaction lives in the device crate**, next to the builders, so the daemon and the fake daemon run the
   same code. Its order: read all 25 chunks → parse (size, schemas 3/8, CRC) → the caller's backup closure saves the
   file and returns what the file holds; it must equal the slot byte for byte, or nothing is erased → the change
   closure makes the new image, which is parsed again → an unchanged image is not written → erase, 25 chunks,
   `B1`, full read-back → on any failure from the erase on, the old image is written back the same way, and the
   error says both what failed and what the restore gave. `Restore::Untouched` (failed before the erase),
   `Restored` (the old image reads back the same) or `Failed` (both errors; "restore it from its backup file").
2. **Tests fail every send.** With `FakeTransport::fail_send(n)` for each of the 102 sends of a write (and two past
   it): sends 1–25 (reads) leave the slot untouched, never call the backup and never erase; every later one restores
   the old image byte for byte, and the step reported is the one that failed. Also: a backup closure that fails or
   returns other bytes stops before the erase; a chunk that lands wrong is caught by `B1`; a read-back that differs
   twice is caught and restored; an erase with no answer restores; a restore that fails too reports both.
3. **The erase answer is only waited for.** Its contents aren't documented; checking an error byte that turns out to
   mean something else would fail every write after its erase, and every restore the same way. The read-back is the
   real check. `B1` is read as `B1 00` good, anything else bad (the reference's general "error byte after the
   command" rule); a misreading there fails safe (the old image goes back).
4. **The read-back reads twice before it calls a difference real**, because the first read after a change may still
   serve the old state (the fakes lag like that). A difference on the second read is a failure.
5. **The last chunk is 324 bytes** (the open point, below).
6. **`Current` writes only what sits at a known place**: the screen picture (text or a picture; a clock or an
   animation would freeze in flash, so the slot keeps its own) and, if gglinux sets the switches, the Rapid Tap
   pairs and on/off. The per-key tables (actuation `h`/`l`, release mode, Rapid Trigger distance, both binding
   layers) are indexed by the firmware's key table (`deviceKeyIndexToHID`), which the reference names but doesn't
   list. Guessing it would write one key's settings into another's place, so those tables stay as the slot has
   them. `Image::set_switches(&KeyTable, …)` is built and tested for when the table is known; the daemon doesn't
   call it. Bindings aren't the worker's to read anyway (the bindings unit's state isn't passed to this module).
7. **Settings for `Current` come from disk** (`switches.json`, `screen.json` in the config dir, as the worker
   saves them on every change), because `on_command` doesn't get the worker's `SwitchesState`/`ScreenState`.
8. **Backups**: `<config>/onboard-backups/slot<N>-<UTC>.bin`, N from 1 as the user counts, `create_new` (a second
   one in the same second gets `-2`), folder made or tightened to 700, file 600, `fsync`ed, then read back. Only
   images that check out are saved; a slot that reads bad is marked `valid: false` and can't be written. A
   `Backup` source is parsed before anything is sent.
9. **The lighting pause is the worker's thread**: an operation runs inside `on_command`, so no frame goes out until
   it ends. `busy` and the two `OnboardChanged` events show it to clients; other requests to the keyboard worker
   wait meanwhile (a write takes about 2 s, at most ~15 s with a restore).
10. **`OnboardState.slots` is empty until something is known** (the contract's default, which the lead's routing
    tests pin); clients always show five slots.
11. **The daemon's route still answers "not built yet"** for read/load/write: `main.rs`'s routing test pins the
    stub's answer, and I can't edit it. `serve` is the real handler and is what the tests call; `handle` (what
    `main` routes to) forwards only `GetOnboard`. Switching is two lines for the lead (report). Same for `ggctl
    onboard list`.

## Open point: the last chunk

The reference says "25 × 500-byte chunks, ~13 ms apart" and "Pad the last chunk with `0xFF`". 25 × 500 = 12 500,
176 bytes past the 12 324-byte image, so the reference most likely sends the 25th chunk as **size 500 at offset
12000**, its last 176 bytes `0xFF`. `allow.rs` refuses any `03`/`83` with `offset + size > 12324`. gglinux sends
**size 324** at offset 12000, with the same 500 bytes of data after the header (324 image bytes, then 176 × `0xFF`),
so only the size field differs from the likely reference report. Unknown: whether the firmware records the file's
length from the writes, and whether `B1` then checks 12 324 or 12 500 bytes. If the M11b capture of GG shows size
500 for the last chunk, the lead should allow exactly that one shape (`03 01 <fs> F4 01 E0 2E 00 00`, data ending in
176 × `0xFF`) and change `CHUNKS`' last size; reads can stay at 324.

## Not verified

- Every opcode here on this keyboard (the reference's author confirmed them on one 1.19.7 keyboard).
- What the erase and `B1` answers hold; whether `B2` answers.
- Whether the keyboard reloads the slot it's running after a write (the reference doesn't say); gglinux doesn't
  load it by itself.
- The firmware key table, so per-key settings can't go into a slot yet.
- The image layout on firmware other than 1.19.x: a schema other than 3/8 is refused, so nothing is written there.
