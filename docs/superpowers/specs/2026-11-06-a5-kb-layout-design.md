# A5 kb-layout: the real Apex Pro TKL Gen 3 board, and layouts by region (audit K11)

## What

`gg-device-keyboard::layout` draws the keyboard as `(HID code, label, x, y, width)` in key
units. Until now it had one table, `ANSI_TKL`: a generic 87-key US TKL with Print Screen,
Scroll Lock and Pause. This unit adds the boards the Apex Pro TKL Gen 3 really comes as:

| Region byte (`F5`) | Board | Keys drawn |
|---|---|---|
| 1, none, or unknown | `US` (ANSI) | 85 = 84 keys + the `0xFB` button |
| 3 | `UK` (ISO) | 86 |
| 4 | `DE` (ISO, QWERTZ) | 86 |
| 6 | `FR` (ISO, AZERTY) | 86 |
| 10 | `NORDIC` (ISO) | 86 |
| 20 | `TR` (ISO, Turkish Q) | 86 |
| 13 | `JIS` | 90 |

- `layout::for_region(region: Option<u8>) -> &'static [Key]` picks one (US for `None` and
  for any byte not in the table).
- `layout::REGIONS: [(&str, u8); 7]` lists `(name, region)` for a future override setting.
- `layout::SCREEN: (x, y, w, h)` says where the screen sits. It isn't a key; the app
  can draw it later, and the tests check that no key covers it.
- `layout::Key` names the tuple. `WIDTH` stays 18.25 on every board.

`ANSI_TKL` stays as it was, for now: the daemon's effects tests, the keys test and the
GameSense zones name Print Screen/Pause through it, and those files belong to the lead. Once
the daemon and the fake take `for_region`, those tests move and `ANSI_TKL` goes. The report
lists the exact changes.

## Protocol source

Apex Control, `docs/PROTOCOL.md` at commit `f7db8c5` (CC BY 4.0), local copy
`gglinux-audit-2026-11-05/evidence/885690054d3f0e15.txt`:

- "Implementation notes verified on hardware": 86 LEDs `0x04–0x45`, `0x49–0x52`,
  `0xE0–0xE7`, `0xF0`, `0xFB`; `0xFB` is the play/pause button beside the screen, with an
  LED; `0xF1`, `0xF5–0xF7`, `0xFE`, `0xFF` are hard-wired media/screen buttons, neither lit
  nor rebindable; `0x46–0x48` (Print Screen / Scroll Lock / Pause) don't exist on this board.
- "68 analog HID codes": `4–40, 42–57, 100, 135–139, 224–231, 240`. The ISO key `0x64` (100)
  and the JIS keys `0x87–0x8B` (135–139) are in it, so those boards have them.
- "Queries & misc": region IDs `1 US, 3 UK, 4 Germany, 6 France, 10 Nordic, 13 Japan,
  20 Turkish`. The layout byte (`F2`: 0 US, 1 EU, 2 JP) isn't read; the region byte decides.

## Decisions

- **Only keys with an LED or a switch are keys.** The screen and its roller have neither, so
  they're empty space in the table; the `0xFB` button is a key, labelled "Media".
- **Positions.** The US board keeps the usual TKL grid (same x/y as `ANSI_TKL` for every key
  it keeps). The top-right corner is inferred: the screen over Ins/Home (x 15.25–17.0) and
  `0xFB` at (17.25, 0) over PgUp. Reviews say "in the top right corner above the utility
  keys there's a small OLED display, scroll wheel, and media button" (Laptop Mag), and
  SteelSeries' full-size Gen 3 photo has the roller above the button at the right of the
  screen. **The owner checks this against his keyboard in M11d.**
- **ISO shape.** Left Shift 1.25 with `0x64` beside it; `0x32` at the end of the home row;
  no `0x31`. Right Alt is labelled "AltGr".
- **The tall Enter.** The drawing (and `gg-ipc::KeyInfo`) has no key height, so the ISO and
  JIS Enter is drawn 1.5 wide on the row where it starts (x 13.5, y 2.5). The home row ends
  at 13.75, leaving the Enter's lower part as a gap. A height field would be a contract
  change; not asked for.
- **JIS shape.** Number row `半/全 1…0 - ^ ¥ BS` (Backspace 1 wide); `@ [` and `; : ]`;
  `0x87` (ろ, labelled `\`) left of a 1.75 right Shift; bottom row Ctrl/Win/Alt as on US,
  then `無変換` 1, Space 3.25, `変換` 1, `かな` 1, then Alt/Fn/Menu/Ctrl where US has them.
- **Labels.** Only the character legends change by region (Z/Y on DE, A/Q/Z/W/M on FR,
  Å/Ö/Ä on Nordic, Ç/Ş/Ğ/Ü/İ/Ö on TR, the JIS symbols). Modifier and navigation labels stay in
  English, like the rest of the app. The French number row shows its digits.
- **One width.** Every board is 18.25 wide, so the app's size and the visualizer's 18 bands
  don't change.
- **Tables.** The US and JIS tables are written out; the five ISO boards are the ISO table
  with some labels changed by a `const fn` (`relabel`), which stops the build if it names a
  key the board doesn't have.

## Tests (`src/layouts/tests.rs`)

For every board: codes ⊆ the frame's key list ∪ the analog list (`proto::KEYS`,
`proto::ANALOG_KEYS`), and ⊆ Apex Control's LED list ∪ the analog list; no duplicate codes;
no overlapping rectangles and none over the screen; every key inside `WIDTH` and the right
edge at exactly `WIDTH`; exactly the analog keys of its family (US `0x31`; ISO `0x32`,
`0x64`; JIS `0x32`, `0x87–0x8B`) plus all 24 fixed keys and nothing else; no `0x46–0x48`, no
hard-wired button, `0xFB` on the top row right of the screen; the main-block rows tile 15
units with no holes (the ISO/JIS home row ends at 13.75); the shared keys sit where US has
them; region labels; the region byte → board mapping.

## Unverified

- The top-right positions (screen, roller, `0xFB`) and that `0xFB` is the "media" button
  the reviews mention: from the protocol doc's wording and photos of the full-size board,
  not from this keyboard. M11d.
- The ISO and JIS boards: shapes from the usual ISO/JIS layouts and the analog list; no
  ISO or JIS Gen 3 was looked at. Whether the ISO/JIS boards light `0x64`, `0x32` and
  `0x87–0x8B` is inferred from the analog list (Apex Control's LED list is one board's).
- The bottom-row widths on JIS.
