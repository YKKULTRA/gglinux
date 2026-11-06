# Key bindings, the Fn layer and the deep-press point (M11 unit a2)

Audit items K02 (remapping), K03 (Fn layer), K04 (second actuation point), K08 (reading bindings back), for the
Apex Pro TKL Gen 3 (`1038:1642`). Everything here is behind the experimental gate `keyboard_bindings` (off by
default) and the keyboard's allowlist family `bindings`. **Nothing here has run on hardware.**

## What

- **Remapping and the Fn layer.** A key can send up to four keyboard usages (combinations such as Ctrl+A), a media
  key, a mouse button (1–8), the CPI button, the wheel up/down or left/right, nothing, or its factory binding. The
  same on the Fn layer (while Fn is held) and, for the 68 adjustable keys, past a second, deeper actuation point.
- **The Fn key** can be moved (`35 <key>`); the keys lit while it is held (`3C <mask>`) are then recomputed from
  the merged Fn layer.
- **The deep-press point** (`38 61 44 02`) is set per key in the switches panel, deeper than the key's first point.
- **Readback**: `ReadBindings` reads all three layers; the app and `ggctl bind` show what the keyboard reported.
- The keyboard keeps all of this in RAM, so the daemon sends it again each time the keyboard connects, like the
  switches. Off (`enabled: false`) sends nothing, and **undoes nothing**: what gglinux sent stays on the keyboard
  until it's unplugged (the app and `ggctl bind` say so). The same holds for the gate turning off.

## Protocol source

Apex Control, `docs/PROTOCOL.md` at `f7db8c5` (CC BY 4.0; local copy
`gglinux-audit-2026-11-05/evidence/885690054d3f0e15.txt`), sections **"Key bindings (remapping)"** (with "`function`
values", "The resting state is a self-mapping, not `UNBOUND`", "`0x62` is a built-in firmware action, and the Fn
layer ships populated", "Reading bindings back") and **"Actuation & rapid trigger"** (the `38 61 44 02` row, the
threshold table and the 68 analog codes), plus "Implementation notes verified on hardware" (86 LEDs, 91 rebindable
keys, 68 adjustable switches).

| Report | Kind | Bytes (after report id 0) |
|---|---|---|
| write a layer | feature | `36 <layer> <count> [key, function, code×4] × count` |
| read a layer | feature, then get feature | `B6 <layer> <count> [key, 0×5] × count` → `B6 <err> <layer> <count> blocks` |
| Fn key | output | `35 <key>` |
| Fn-lit keys | output | `3C <32-byte mask>`, bit `key & 7` of byte `key / 8` |
| deep-press point | feature | `38 61 44 02 [key, h, l] × 68`, the primary table's thresholds |

Code: `crates/gg-device-keyboard/src/bindings.rs` (builders, decoder, merge; byte-for-byte tests including the
source's `04 51 04 00 00 00` and Caps = `51 E0 04 00 00`), `crates/gglinuxd/src/keyboard/bindings.rs` (store, read,
merge, write, re-apply on connect), `crates/gglinuxd/src/keyboard/switches.rs` (the deep-press table).

## Decisions

1. **A write always starts from a read made on this connection.** A layer is written whole, so the daemon reads
   all three layers first (once per connection; that first read is the *base*), applies the user's bindings on
   top and writes. With no read, nothing is written. A key the user doesn't name keeps its block **byte for byte**,
   so bindings set by other software or an onboard profile survive; so does a key the user names with the action
   it already has (the keyboard's own packing of the code bytes stays).
   - **A base that looks blank is refused** (review R1, F14). A normal layer with fewer than a quarter of its 91
     keys typing themselves is taken for a bad read: it isn't kept as the base, nothing is written, and the
     message says to unplug and replug and to report it if it repeats. The next change reads again. An Fn layer
     with no `62` block refuses changes to the Fn layer only (the message says replugging loads the keyboard's
     own Fn layer again). There's no "I really cleared them" override: the contract has no field for it.
   - **After an onboard slot is loaded** (`B2`, a3's `ggctl onboard load`) the base is forgotten (review R1, F5):
     the next change reads all three layers again, each **twice** keeping the second answer, since the first read
     after a command may still show the slot the keyboard ran before. The bindings aren't sent straight after the
     load; the keyboard runs the slot's own until the next change or reconnect. The switch settings are sent
     again at the next loop (onboard review F9), and a deep-press table of gglinux's is forgotten too. The hook is
     in `keyboard.rs` where the onboard `Load` is dispatched, and only a load that succeeded triggers it.
   - **Known gap:** a slot switch made on the keyboard itself (its own menu) isn't seen. The `0xFFC1` interface
     sends "config-change events", but gglinux doesn't read them, and their payload isn't described. After such a
     switch the next bindings change writes the *old* slot's layers (with the change) over the new slot's, and
     `ggctl bind read` shows the new slot without changing the base. Workaround: unplug and replug the keyboard (or
     load the slot with `ggctl onboard load`) after switching slots on the keyboard.
2. **The keyboard's own shortcuts are never touched.** Every block with function `62` (the nine factory shortcuts
   on F9–F12, Left GUI, O, I, T, Q) is written back exactly as read. A binding that would change one is refused
   before anything is saved or sent ("…one of the keyboard's own shortcuts…; gglinux keeps it"). "Default" on such
   a key keeps it. Opaque actions (`62`, macros `71`, host-assisted `72`, unnamed functions) can only be kept where
   the key already has them, never made up.
3. **"Default"** writes the self-map `51 <key> 00 00 00` on the normal layer and `00` on the Fn and deep layers. A
   key dropped from the list since the last apply also goes back to its default; this undoes gglinux's own earlier
   writes even when the base read already contains them (after a daemon restart without unplugging). It's a
   trade-off (review R1, F7): if a profile or other software had bound that key before gglinux did, removing
   gglinux's binding writes the factory self-map, not what the keyboard had.
4. **Only what differs is written.** The daemon remembers what it wrote on this connection; a layer equal to what
   the keyboard holds isn't sent again. It never trusts a read straight after a write to tell what the keyboard
   holds (the fakes lag one read, like the keyboard). A write that fails half-way keeps what went through (the
   next try sends only the rest).
5. **Fn key and mask** go out when `fn_key` is set, and again only when either changes. Going back to "the
   keyboard's own" (`fn_key: None`) after gglinux moved it sends `35 F0` (the SteelSeries key, the factory value in
   the slot image) once, with the mask recomputed (review R1, F3). The mask goes out only with an Fn key chosen, so
   a new Fn binding with no Fn key chosen isn't lit while Fn is held; and when it goes out it **replaces** the
   keyboard's mask, which can't be read back and might hold more bits than the bound keys (review R1, F6). In
   M11d, compare `fn_mask(read)` with the slot image's bytes 40–71 before relying on it.
6. **Gate off refuses the request.** With `keyboard_bindings` off, `ReadBindings` and `SetBindings` answer the
   contract's refusal ("Keyboard: key bindings and deep press hasn't been tried on your hardware yet: turn it on in
   Settings → Experimental first") and nothing is saved or sent. The task text says "validates, saves, and if …
   gated on … writes"; I followed the brief's hard rule instead (the daemon refuses a device-writing feature while
   its flag is off). `on_connect` re-applies only with the gate on. Turning the gate on doesn't apply saved
   bindings by itself (the worker's `SetExperimental` doesn't reach this module); the next change or reconnect does.
7. **The deep-press table** goes after the switch settings, only with `Switches.second` non-empty and the bindings
   family on. With the gate off the bytes sent are exactly those of before (tested byte for byte). When the gate
   comes on while connected, the table goes out alone at the next loop. Gate off holds it back silently; the
   switches panel says why. Removing the **last** deep press after a table went out on this connection sends one
   more table with "none" (`FF FF`, unverified, see below) for every key, so the keyboard doesn't keep the old point
   (review R1, F4); after a reconnect or a slot load nothing is sent for an empty list. A deep press must reach the
   keyboard **deeper** than the key's first press: the daemon compares the thresholds actually sent (`h`), since
   0.1/0.2 mm and 3.8/3.9/4.0 mm each send the same one (review R1, F8). The contract's check in tenths stays.
8. **Bindings and deep-press points are validated** as the contract does, then: normal/Fn keys must be among the 91,
   deep-press keys among the 68 adjustable ones, the Fn key among the 91.
9. **Saved** in `<config>/bindings.json` (`enabled`, `bindings`), written atomically; an unreadable file is set
   aside and the defaults (off) used.
10. **App:** the panel shows once the daemon answers `get_bindings`. Gate off: the head, the refusal sentence, the
    greyed toggle and the greyed "Read from the keyboard" button. Gate on and bindings on: layer tabs, the keyboard
    picture (gglinux's bindings outlined with a short label, the keyboard's own from the last read labelled, keys
    with no binding on the tab greyed), an editor for the picked key, the Fn key picker. The deep-press point is a
    section of the switches panel, greyed with the same sentence while the gate is off.
11. **Reading back** (review R1, F13): if the answer to `B6` is still another command's (`38 00` after the switch
    tables, `40 00` after a colour frame), the read waits 20 ms and asks once more. A second wrong answer is an
    error naming what came back. A real `B6` answer that's wrong isn't asked again.
12. **The allowlist holds the builders' rule** (review R1, F9): a `36` must carry the whole layer (91 keys on layers
    0 and 1, 68 on layer 2), and each `B6` block must be the key alone (`[key, 0 × 5]`).

## Assumed, unverified until a capture or the first read on hardware

- **Which 91 keys** (review R1, F1: settle read-only in M11d before the first write). The source gives the count,
  not the list. I took every lit key (`04`–`45`, `49`–`52`, `E0`–`E7`, `F0`, `FB`) and every adjustable key (adding
  `64`, `87`–`8B`): 92 codes, and left out `FB` (the play/pause button beside the screen), assuming it is hard-wired
  like the six media buttons. A wrong guess does **not** always make the first read fail: the keyboard answers only
  for the keys asked, so a real key missing from the list (`FB` is the likely one) goes unnoticed and is left out of
  every write, and a key the keyboard lacks might still get a plain block. The blank-read guard (decision 1) only
  catches a read that looks empty. Check the list against a slot image's normal layer (all `51 <hid>` self-maps)
  first.
- **`FF FF` for "no deep-press point"** in the RAM table (review R1, F2). The source gives `FF FF` as "disabled" only
  for the flash image's second-actuation area. Marked in code (`NO_SECOND_POINT`). The first deep press sends it to
  the other 67 keys, and removing the last one sends it to all 68. Check offsets 1306–1445 of a slot image without
  dual actuation are all `FF` before deep press is turned on.
- **The report kinds** (`35`/`3C` as output reports, the rest feature) follow the source's table; the allowlist
  checks bytes only.
- **What the keyboard does with a 36 write that leaves keys out.** We always write all 91 (or 68); chunking past 106
  keys exists in the builder but can't happen on this keyboard.
- **Mouse-button bindings** have never been seen to click on this keyboard (the source says so too); the editor
  says it. Mouse, CPI, wheel and pan bindings send four zero code bytes: the source lists the function values
  without their code bytes, so the zeros are a guess (review R1, F10). Keep them out of the first hardware session.
- The read decodes host-assisted (`72`) and unnamed functions, and a `51` block sending no key, as nothing the
  contract can name: they are left out of `BindingsState.read` (and kept byte for byte by any write).
- **How long the keyboard takes** to put a `B6` answer in its read slot: no delay is given; one 20 ms retry is a
  guess (decision 11). Watch for "answered 0x38 0x00" or "0x40 0x00" in M11d.
