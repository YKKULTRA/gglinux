# A3 kb-onboard: the keyboard's onboard profile slots and the checked flash write

Unit a3-kb-onboard of M11 (audit K06, K07), fixed by unit a3f-onboard-fixes after review R1-onboard (see "Fixes
after review R1-onboard" below; where a decision here changed, it says so). Built against fakes only; nothing here
has been sent to a keyboard, and all of it stays behind the `keyboard_onboard` experimental gate (off by default).

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

Where the document leaves a shape or a timing open, gglinux follows Apex Control's own code at the same commit
(MIT), pinned in `gglinux-audit-2026-11-05/evidence/apexkit-f7db8c5/` (sha256s in `SHA256SUMS`), cited
`AK:<file>:<line>` as review R1-onboard cites it.

## Decisions

1. **The transaction lives in the device crate**, next to the builders, so the daemon and the fake daemon run the
   same code. Its order: read all 25 chunks → parse (size, schemas 3/8, CRC) → the caller's backup closure saves the
   file and returns what the file holds; it must equal the slot byte for byte, or nothing is erased → the change
   closure makes the new image, which is parsed again → an unchanged image is not written → erase, 25 chunks,
   `B1`, full read-back → on any failure from the erase on, the old image is written back, and the error says both
   what failed and what the restore gave. `Restore::Untouched` (failed before the erase), `Restored` (the old image
   reads back the same), `Failed` (both errors, and the `--damaged` command that puts the backup back) or
   `NotRecovered` (a `--damaged` recovery that didn't get through). *Changed by a3f:* the restore no longer stops at
   its first problem (F2, below).
2. **Tests fail every send.** With `FakeTransport::fail_send(n)` for each of the 102 sends of a write (and two past
   it): a failed chunk read is tried again, so sends 1–25 and the read-backs' sends still end in a written slot;
   a failed erase, chunk or `B1` send restores the old image byte for byte, and the step reported is the one that
   failed. *Changed by a3f:* reads are retried (F5), and sweeps now cover the restore and failed reads too (F15). Also: a backup closure that fails or
   returns other bytes stops before the erase; a chunk that lands wrong is caught by `B1`; a read-back that differs
   twice is caught and restored; an erase with no answer restores; a restore that fails too reports both.
3. **The erase answer is only waited for.** Its contents aren't documented; checking an error byte that turns out to
   mean something else would fail every write after its erase, and every restore the same way. The read-back is the
   real check. *Changed by a3f:* `B1`'s answer decides nothing either (F6); it's logged and kept as a note.
4. **The read-back reads twice before it calls a difference real**, because the first read after a change may still
   serve the old state (the fakes lag like that). A difference on the second read is a failure.
5. ~~**The last chunk is 324 bytes**~~ *Changed by a3f (F4, F12):* every chunk is 500 bytes on the wire, written and
   read, exactly as Apex Control's code sends it; the last carries 324 image bytes and 176 × `0xFF`.
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
   images that check out are saved as backups; a slot that reads bad is marked `valid: false` and can't be written
   the usual way. A `Backup` source is parsed before anything is sent. *Changed by a3f (F16, F3):* the folder is
   synced too, a slot that reads bad is saved raw as `slot<N>-<UTC>-damaged.bin` (never taken for a backup), and
   `Recover` puts a backup into such a slot.
9. **The lighting pause is the worker's thread**: an operation runs inside `on_command`, so no frame goes out until
   it ends. `busy` and the two `OnboardChanged` events show it to clients; other requests to the keyboard worker
   wait meanwhile (a write takes about 2 s; with every wait run out and three restores, about 37 s). *a3f (F7):*
   stopping or restarting the daemon waits for a write asked for or running (`keyboard::onboard::Writes`), at most
   that long, and says so in the log.
10. **`OnboardState.slots` is empty until something is known** (the contract's default, which the lead's routing
    tests pin); clients always show five slots.
11. **The daemon's route still answers "not built yet"** for read/load/write: `main.rs`'s routing test pins the
    stub's answer, and I can't edit it. `serve` is the real handler and is what the tests call; `handle` (what
    `main` routes to) forwards only `GetOnboard`. Switching is two lines for the lead (report). Same for `ggctl
    onboard list`.

## Fixes after review R1-onboard (unit a3f-onboard-fixes)

The review (`gg-m11/reviews/R1-onboard.md`) found the bytes, CRC, layout, order and gate right, and four ways a slot
could be left erased or corrupt with no way back inside gglinux. All fixed test-first:

- **F1, old input reports.** `Keyboard::drain_input` throws away the input reports nobody read before the erase and
  before `B1`; `Keyboard::await_input` skips reports that don't start with the awaited opcode until the deadline. The
  fake keeps an input queue (`FakeTransport::queue_input`, `queue_input_after`), and a write and a restore go
  through with old and late reports queued.
- **F2, the restore never stops once its erase is out** (`Pass::Back`): each send tried 3 times, a missing erase
  answer waited out for its whole time, `B1` advisory, the read-back always run, and the whole restore done up to 3
  times (`RESTORES`). Sweeps fail each restore send once, and 4 reads in a row anywhere; a test leaves the slot
  half written (the keyboard takes nothing for the whole restore) and asserts what it holds.
- **F3, a damaged slot.** `Keyboard::recover_slot`, the daemon's `OnboardSource::Recover` and `ggctl onboard restore N
  <file> --damaged` (with its own yes/no question): read the slot, refuse if it checks out or holds a whole image of
  other schemas, save it raw as `-damaged.bin`, check the backup file, then erase, 25 chunks, `B1`, read-back as a
  restore does. The app shows the command; it doesn't run it. Error texts now name the command.
- **F4/F10/F12, chunk shapes.** Every `03`/`83` is size 500 at a multiple of 500 from 0 to 12000; `03`'s data is the
  image's bytes, `0xFF` up to 500, then zeros. The allowlist takes exactly that and nothing else. Byte test:
  `03 01 80 F4 01 E0 2E 00 00`.
- **F5** 13 ms between `83` and its read, up to 4 tries per chunk; a fake whose answers come 4 ms late shows both.
- **F6** `B1`: up to 2 s, logged, decides nothing; `B1 <≠00>` or no answer is the state's `note`.
- **F7** stop/restart/parts change wait for a slot write (asked for or running), at most `longest_write()`; **F8**
  ggctl waits 60 s for onboard reads and writes, and says the daemon may still be writing.
- **F11** 13 ms after the last chunk before `B1`; **F13** `check_slot` can't overflow; **F14** offsets pinned as numbers,
  ApexKit's CRC vectors, a literal-byte reply; **F15** sweeps over the restore and failed reads; **F16** the backup
  folder is synced; **F17** the `Current` doc says what it writes.
- Not done here: F9 (switches sent again after a Load) is unit a2f's; F18 is the plan's wording (the lead's).

## Not verified

- Every opcode here on this keyboard (the reference's author confirmed them on one 1.19.7 keyboard).
- What the erase and `B1` answers hold; whether `B2` answers. Whether `B1` must be sent for the firmware to take a
  slot (gglinux always sends it, and doesn't call a new image written if it couldn't be sent).
- Whether 13 ms is enough for every chunk read on this keyboard (Apex Control's figure); M11d's two reads compared
  byte for byte show it.
- How the firmware takes a second erase while a first is still running (only after an erase that never answered,
  and only after its whole 6 s).
- Whether the keyboard reloads the slot it's running after a write (the reference doesn't say); gglinux doesn't
  load it by itself.
- The firmware key table, so per-key settings can't go into a slot yet.
- The image layout on firmware other than 1.19.x: a schema other than 3/8 is refused, so nothing is written there.
