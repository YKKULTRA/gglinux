# B2-KB-MACROS report: keyboard macros, written into an onboard slot as GG 123 does

Branch `m11-b2-kb-macros`, on top of `aeb0193`. Commits: `d651f27` (codec, daemon, ggctl, fake), `0c0bfde` (app panel,
connect read, design note), `342f24a` (ggctl reads twice before a change, the panel's note as a sentence).

## Result

`~/projects/gg-m11/check.sh b2-kb-macros` on `342f24a`, which I ran and read to the end:

```
tests: 870 passed, 0 failed
CHECK OK
```

That's 39 new tests on top of the 831 at `aeb0193`.

## Files

Added:
- `crates/gg-device-keyboard/src/macros.rs`: the codec (11 tests)
- `docs/superpowers/specs/2026-11-13-b2-kb-macros-design.md`: the design note
- `crates/gglinux-ui/ui/macros.slint`: the MACROS panel

Changed (all in my unit file's list):
- `crates/gg-device-keyboard/src/lib.rs`: `pub mod macros;`
- `crates/gg-device-keyboard/src/onboard.rs`: new `Image` accessors only (`normal_bindings`, `fn_bindings`,
  `second_bindings`, `set_macros`). The transaction is not changed.
- `crates/gg-ipc/src/macros.rs`: validation, steps as words, key names, `MacrosState::{backup, note}` (7 tests)
- `crates/gglinuxd/src/keyboard/macros.rs`: the daemon module (9 tests)
- `crates/gglinuxd/src/keyboard.rs`: `Command::Macros`, its Debug arm, the `macros` store field and its load, the
  `handle` arm
- `crates/gglinuxd/src/keyboard/onboard.rs`: `save_backup` made `pub(super)`, nothing else
- `crates/gglinuxd/src/bin/ggctl/macros.rs` (5 tests)
- `crates/gglinuxd/src/bin/ggctl/main.rs`: USAGE lines, `answer_wait` for `WriteMacros`, the `Macros` print arm
- `crates/gglinuxd/examples/fakemouse/keyboard_macros.rs`, and in `fakemouse/main.rs` the `macros` field plus its
  one initializer line
- `crates/gglinux-ui/src/macros.rs` (6 tests)
- `crates/gglinux-ui/src/lib.rs`: module, form, reset, wire, show arms, poke
- `crates/gglinux-ui/src/client.rs`: `GetMacros` read at connect
- `crates/gglinux-ui/src/client/tests.rs`: 1 test
- `crates/gglinux-ui/ui/app.slint`: import/export and the panel's place in the keyboard card

## Public API

- **Device crate** (`gg_device_keyboard::macros`):
  - `read(&Image) -> Result<Vec<SlotMacro>>` and `write(&Image, &[SlotMacro]) -> Result<Image>`
  - `place(code) -> Option<usize>`
  - `SlotMacro { key, mode: Mode::{Once, WhileHeld}, events: Vec<Event::{Key, Button, Delay}> }`
  - `FIRMWARE_KEYS: [u8; 100]` (Apex Control's `deviceKeyIndexToHID`), `MAX_MACROS = 2`, `CANT_READ` (the start of
    the refusal sentence)
  - `factory_like(name)` (with the `testing` feature)
  - New `Image` accessors: `normal_bindings()`, `fn_bindings()`, `second_bindings()`, `set_macros(area, places)`.
- **gg-ipc**: everything hangs off the types `lib.rs` already re-exports, because `lib.rs` isn't mine:
  - `MacroLibrary::{validate, find, slot_bytes}` and the constants `MAX_NAME` (32), `MAX_BOUND` (2),
    `AREA_BYTES` (9600), `HEADER_BYTES`, `EVENT_BYTES`, `LEFT_BUTTON`
  - `Macro::{parse_steps, steps_text, check_steps}`
  - `MacroEvent::{usage_by_name, usage_name}` and `MacroBinding::{key_by_name, key_name}`
  - `MacrosState` gains `backup` and `note`, both `#[serde(default)]`.
- **Requests**: the contract's, unchanged.
  - `GetMacros` → `Macros { state }`, no gate needed.
  - `SetMacros { library }` → `Macros { state }`. Needs `keyboard_macros`; checks the library and saves it to
    `macros.json`; sends the keyboard nothing.
  - `WriteMacros { slot }` → `Macros { state }`. Needs `keyboard_macros` and `keyboard_onboard`, and refuses with the
    contract's sentence for whichever is off. Runs the checked slot transaction, counts as a slot write, says
    `busy` while it runs and sends `MacrosChanged` at the start and the end. If the slot it wrote is the one the
    keyboard runs, it loads it again (`B2`) and then calls `bindings::on_slot_loaded` and `switches.forget()`.
- **ggctl**:
  - `macro`
  - `macro set NAME <steps>`
  - `macro rm NAME`
  - `macro bind KEY NAME [hold]`
  - `macro unbind KEY`
  - `macro write N` (waits 60 s, like `onboard write`)
  - The steps are `a`, `ctrl+c`, `down:shift`, `up:shift`, `wait:100`, `click:1`, `down:mouse1`/`up:mouse1` and
    `text:"…"` (US layout). In a shell, quote the text: `'text:"hello world"'`.
- **App**: `MacrosUi`, `MacroRow` and `MacrosPanel`. Steps for the `shot` example: `macros.pick:N` (N = -1 opens a
  new macro, -2 closes the editor), `macros.key:N`, `macros.save`, `macros.slot:N`, `macros.write`.

## What was checked

- **GG's three saves, byte for byte.** Tests rebuild the macro area and the bound keys' places of each save from the
  same macros:
  - apex-20: A↓ 15 ms A↑ 313 ms B↓ B↑ 3187 ms Btn1↓ Btn1↑ on P;
  - apex-21: two macros, on O and P, with P's moved to word 20;
  - apex-22: the same, with P repeating while held.

  GG's bytes are hex constants in the tests. Decoding GG's bytes gives those macros back.
- **By hand with Python, outside the tests:** I put GG's area and places into the factory slot-5 image and worked
  out the CRC again. The result matches GG's image except at the 107 bytes GG also rewrote (blank places, the Fn
  layer's `51`→`00`, zone bytes) and the CRC. The STM32 CRC reproduces GG's stored CRCs.
- **Refusals:**
  - A slot with anything not known is refused before the erase with "this slot holds macros gglinux can't read yet
    (…)". That covers another play mode, another kind of step, other header bytes, a binding pointing where no macro
    starts, other bytes in a binding, bytes after the last macro, a count that runs past the area, and a macro on
    the Fn layer. The daemon test also shows nothing is erased and the slot stays as it was.
  - With the onboard family off, the allowlist lets nothing of a macros write out, the first read included.
  - Each gate refuses with its own sentence, and nothing is sent.
- **Daemon behaviour (tests):**
  - A write backs up first; the backup goes to `onboard-backups/` and holds the old slot.
  - A write is one erase, 25 chunks and `B1`. Writing the same macros again sends no erase.
  - Writing the running slot ends with `B2`.
  - A slot number past 5 is refused before the worker. With the keyboard away, a write is refused.
  - A write counts as a slot write from its request to its end.
- **Against the fake daemon with the real `ggctl`** (run on the PC, private runtime dir):
  - both gate refusals;
  - the two-key limit;
  - a write (1.3 s);
  - an edit straight after another edit. Before `342f24a` this lost the first change, because the lagging fake
    answers the first read with the old state. ggctl now reads twice.

## Pictures

I made these with `shot.sh` and the fake (`FAKE_ARGS="--no-mouse --no-headset"`, `SHOT_SCROLL=end:1`) and looked at
each one. They're in `/home/agent/.hermes/cache/scratch/b2shots/`, which is pruned after a day; the commands below
make them again.

- **`off.png`**, both gates off (the default):
  - the panel at the end of the keyboard card;
  - the refusal sentence in orange;
  - the fake's two example macros ("greeting" on P, "copy and paste" on no key);
  - New macro, the slot list and "Write to slot 5" greyed;
  - the sentence that the write rewrites slot 5's macros and keeps a backup.
- **`on-picked.png`**, both gates on, "greeting" open (`SHOT_DO=experimental.on:3,experimental.on:1,macros.pick:0`):
  - the name box and the steps box ("shift+h e l l o shift+1 enter"), the steps hint, and Save / Remove / Close;
  - the keyboard picture with P outlined and labelled "greeti…", and Fn greyed;
  - the hold toggle, the slot list and the Write button enabled.
  - One problem showed here: the media button wasn't greyed. I fixed it (`can_have`, tested), and
    **`on-picked2.png`**, drawn again on the final code, shows Media and Fn both greyed.
- **`macros-only.png`**, only `keyboard_macros` on: the list and New macro work, the onboard refusal shows under
  "Write into a slot", and the slot list and Write button are greyed.
- **`written.png`**, right after "Write to slot 5": the busy state, "Writing the keyboard's memory…", with
  everything greyed.
- **`written3.png`**, after the write (padded with `macros.slot:4` steps so the 1.3 s write has ended): "Last
  written into slot 5." and the note about loading the slot.
  - Two fixes came after this picture, without a new one: the note now starts with a capital, and it no longer
    mentions a slot button on the keyboard. That button isn't proven, so the note now points to
    `ggctl onboard load 5` or the Load button.
- **`new-refused.png`**, New macro then Save with no steps: the "New macro" editor without Remove, and the error
  "a macro needs at least one step".
  - The name box looked filled because its placeholder ("copy and paste") read like a real name. I changed it to
    "Its name". **`new2.png`**, drawn again on the final code, shows it, with the Write button and slot list
    enabled when both gates are on. The headless renderer draws the placeholder only a little dimmer than typed
    text.

## Not done, or uncertain

- **Nothing has been on hardware.** The format is GG's own bytes, but no keyboard has played a macro gglinux wrote.
  How fast a `text:` step types on the real keyboard is unknown: its taps have no waits between them, while GG's
  recorded macros have waits.
- **Refused because not proven:**
  - GG's other three play modes (Play N Times, Toggle Auto-Repeat, Toggle Hold);
  - other kinds of step, and mouse buttons other than 1 (the button byte may be a number or a bit mask);
  - macros on the Fn or deep-press layer;
  - more than two keys with a macro;
  - GG's Text Macro format (gglinux types text as plain key taps instead).
- **Which slot runs is only known after a load.** It comes from the onboard module's `active`, which is set by a
  load and forgotten on connect. Right after a connect, a write into the running slot doesn't load it; the note
  says to run `ggctl onboard load N`.
- **The slots list doesn't hear about a macros write.** The onboard panel shows the older backup and name until
  that slot is read again or the daemon restarts. See snippet 1 below.
- **A key that had a remap gets the plain key back.** When its macro goes away, it types itself again
  (`51 <key> 00 00 00`), not its old remap.
- **A slot with GG-made macros gglinux can read is rewritten.** Its macros are replaced by the library. The panel
  sentence says this, and a backup is kept.
- **Live key bindings can override a macro key.** With gglinux's key bindings on (`ggctl bind on`) and a binding of
  its own on a macro key, the live binding is sent after a slot load and wins on that key until the keyboard is
  unplugged.
- **The app's keyboard picture greys only the Fn key and the media button.** That covers every key on the three
  boards that has no firmware place. The daemon checks each key against the firmware key order.
- **The strict red step (see the test fail first) was run for the codec and the daemon module.** The gg-ipc, ggctl,
  fake and UI tests were written together with their code.

## Changes needed in files I don't own

1. `crates/gglinuxd/src/keyboard/onboard.rs`, so the slots list learns about a macros write (the newest backup, the
   slot checks out) and the backup folder has one source:
   ```rust
   impl Store {
       /// Another module (the macros) wrote `slot` through the slot transaction.
       pub(super) fn written_elsewhere(&mut self, slot: u8, backup: Option<String>, name: String) {
           let s = self.slot(slot);
           if backup.is_some() {
               s.backup = backup;
           }
           s.valid = Some(true);
           s.name = Some(name);
       }
   }
   /// Where the slots' backups go.
   pub(super) fn backups(config_dir: &Path) -> PathBuf { config_dir.join("onboard-backups") }
   ```
   Then `keyboard/macros.rs` would drop its `BACKUPS` constant, and `on_command` would also return the backup and
   the name so the `handle` arm can call `onboard::Store::written_elsewhere`.
2. `crates/gglinuxd/src/bin/ggctl/bind.rs`, `describe`. A key with a gglinux macro reads back as `macro 0`, because
   the first code byte is always `00` and the start comes after it:
   ```rust
   KeyAction::Macro { .. } => "a macro stored in the slot (see `ggctl macro`)".into(),
   ```
3. `crates/gg-ipc/src/bindings.rs` and `crates/gg-device-keyboard/src/bindings.rs`: the docs of `KeyAction::Macro`
   and `Action::Macro` say the format isn't known. It is now (PROTOCOLS §3f): `71 00 00 <start u16 LE>`. The field
   called `slot` is the first code byte, always `00` in GG's saves.
4. `crates/gglinuxd/src/bin/ggctl/main.rs`, test `the_usage_lists_every_command_group`: add `"macro"` (and
   `"headset"` for b4).
5. With b1: `gg_device_keyboard::macros::FIRMWARE_KEYS` is the firmware key order. If b1 adds the same table for
   `onboard::KeyTable`, keep one copy, e.g. build `KeyTable` from `FIRMWARE_KEYS[..KEY_PLACES]`.
6. With bind.rs: gg-ipc now holds a copy of `ggctl bind`'s key names, because `bind.rs` isn't mine. The ggctl test
   `key_names_are_the_same_as_ggctl_binds` compares both tables, every usage and every alias. Later, `bind.rs` could
   use `MacroEvent::usage_by_name` and `MacroBinding::key_by_name` and drop its copy.
7. `docs/PROTOCOLS.md` §3f, one line after "Keyboard macros live in the slot image":
   > gglinux writes exactly these shapes (`ggctl macro write`, gated by `keyboard_macros` and `keyboard_onboard`):
   > only the macro area, the bound keys' normal-layer places and the CRC change, and a slot whose macros hold
   > anything else is never rewritten.

## For README / PLAN / CHANGELOG

> **Keyboard macros (experimental).** The Apex Pro TKL Gen 3 plays macros itself, from the onboard slot it runs.
> gglinux keeps a list of macros (steps as words: `a`, `ctrl+c`, `down:shift`, `wait:100`, `click:1`,
> `text:"hello"`), lets up to two keys play them once per press or while held, and writes them into a slot exactly
> as SteelSeries GG stores them (decoded from our own GG captures). Only the macro area and those keys change, after
> a backup of the slot, and the slot is read back and put back on any mismatch. A slot holding macros gglinux can't
> read is left alone. `ggctl macro …` and a MACROS panel in the keyboard card. Off until both "Keyboard: macros" and
> "Keyboard: onboard profile slots" are on in Settings → Experimental. Not tried on a keyboard yet.

## Design note

`docs/superpowers/specs/2026-11-13-b2-kb-macros-design.md` covers what was built, the source (capture names and
offsets), ten decisions, what isn't verified on hardware, and a two-minute check by hand.

## Hands-on checks for the owner

Use the spare slot 5 (Config 5, factory image). Its backup is
`~/backups/gglinux/onboard-2026-11-13/slot5-20261113T103122Z.bin`. Don't write macros into slot 1: if it holds
GG-made macros gglinux can read, a write replaces them (a backup is kept, but don't).

1. **Turn it on and make a macro.** These change only gglinux's own settings; nothing is sent yet.
   ```
   ggctl experimental keyboard_macros on
   ggctl experimental keyboard_onboard on
   ggctl macro set gg 'text:"gg"'
   ggctl macro bind p gg
   ggctl macro                  # shows: gg: g g / p → gg (once per press) / not written into a slot yet
   ```
2. **Write it into slot 5.** This takes about 2 s; leave the keyboard plugged in.
   ```
   ggctl macro write 5
   ```
   You should see "last written into slot 5", the path of the backup it made, and a note to load the slot.
3. **Run slot 5, then press P** in a text editor:
   ```
   ggctl onboard load 5
   ```
   You should see `gg` instead of `p`, once per press. If gglinux's switch settings are on, they're sent again
   after the load.
4. **Optional: while held.**
   ```
   ggctl macro bind p gg hold
   ggctl macro write 5
   ggctl onboard load 5
   ```
   Holding P should type `gg` again and again.
5. **Optional: GG's own example**, the exact macro from capture apex-20:
   ```
   ggctl macro set gg1 down:a wait:15 up:a wait:313 b wait:3187 click:1
   ggctl macro bind o gg1
   ggctl macro write 5
   ggctl onboard load 5
   ```
   O should type `ab`, then about 3 s later do a left click.
6. **In the app:** Engine → keyboard card → MACROS at the bottom.
   - Click a macro to open it. Click P on the keyboard picture to bind or unbind it.
   - Pick "Slot 5" and press "Write to slot 5". The panel says "Writing the keyboard's memory…", then
     "Last written into slot 5."
   - With either gate off, everything is greyed and the reason is shown.
7. **Put things back:**
   ```
   ggctl onboard restore 5 ~/backups/gglinux/onboard-2026-11-13/slot5-20261113T103122Z.bin
   ggctl onboard load 1
   ggctl macro unbind p; ggctl macro unbind o    # gglinux's list only
   ggctl experimental keyboard_macros off
   ggctl experimental keyboard_onboard off
   ```
   If gglinux's switch settings had replaced Config 1's in the keyboard's RAM, run `ggctl switches off` before
   `ggctl onboard load 1`.
