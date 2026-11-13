# GameSense screen and bitmap handlers (M12 c2) — design

**Status:** built 2026-11-13 on branch `m11-c2-gamesense-screens`; not tried with a real game or on the keyboard
yet. **Audit items:** G01 (OLED screen handlers), G02 (full-keyboard bitmap and partial-bitmap handlers).

## What

- **Screen handlers.** A game's `screen` handler for `screened` or `screened-128x40` now draws on the Apex Pro TKL
  Gen 3's 128 × 40 screen: text lines (`has-text`, `prefix`/`suffix`, the value, `bold`, `wrap`), progress bars,
  raw 1-bit pictures (`image-data`, and the event's own `image-data-128x40`), `length-millis`, `repeats`, ranged
  `datas` (`low`/`high`), and the data accessors `context-frame-key` and `arg`. The game's frame goes over the
  user's own screen (gglinux's clock, text, picture, animation, monitor, or the keyboard's own menus) while it's
  due, and the user's screen comes back when the frames end, the game goes quiet (its timer, 15 s by default), is
  stopped, turned off or removed.
- **Bitmap handlers.** `bitmap` and `partial-bitmap` handlers (`rgb-per-key-zones`) paint every key from the
  event's 132-colour `bitmap` grid, through the same GameSense layer as colour handlers. `partial-bitmap` leaves the
  keys of its `excluded-events` alone (the handler's list, or the event frame's own `excluded-events` when sent).
- **ggctl:** `ggctl gamesense` lists each game, whether it's drawing and on the screen now, each event with what its
  handlers draw on, and what a screen handler asks for that isn't drawn. `ggctl gamesense test screen` plays a
  3.6 s sequence on the screen through the same path a game uses.
- **App:** the Games panel says what each event draws ("HEALTH: keys and screen"), marks a game that is on the
  screen now, lists what's left out, and has a "Test the screen" button. "Run test" now also shows the health on
  the screen (label and bar) while the F-key bar sweeps.

## Sources

- Screen handlers: GameSense SDK `json-handlers-screen.md` (pinned: audit `evidence/a86106a8387c6f76.txt`). Every
  example in it is a test fixture (`crates/gg-gamesense/src/screen/tests.rs`, `registry/screen_tests.rs`).
- Bitmap handlers: GameSense SDK `json-handlers-full-keyboard-lighting.md` (pinned: audit
  `evidence/feb4d7947abfa372.txt`), commit `5b69e24` of `SteelSeries/gamesense-sdk`.
- No new device command: the screen is drawn with the live picture report (`1F 81` + 640 bytes) and handed back
  with `1F 82`, exactly as gglinux's own screen pictures are (Apex Control, CC BY 4.0, commit `f7db8c5`, section
  "OLED (128 × 40, 1 bpp)"; see `2026-11-06-a4-kb-screen-design.md`). The keys are painted with the lighting
  frames GameSense colour handlers already use.

## Decisions

- **Where it lives.** Parsing, timing and drawing are pure, in `gg-gamesense`: `screen.rs` (handler → `ScreenSpec`,
  and `Show`, the frames playing from a moment), `screen/draw.rs` (a frame → `Bitmap`), `screen/arg.rs` (the `arg`
  forms), `bitmap.rs` (grid → keys). The registry starts a `Show` when an event changes and composes bitmaps in
  `paint`. Time is passed in, so every timing test runs on a made-up clock.
- **Reaching the screen.** The keyboard's `Overlay` trait gained `screen(now) -> Option<Overlaid>` (default
  `None`); GameSense's overlay answers it from the registry. Each loop the keyboard worker hands the answer to
  `ScreenState::set_overlay`. While there is one it's what `ScreenState` draws, over any setting; when it goes, the
  setting's picture comes back, or the screen is handed back to the keyboard (`1F 82`) if the setting is the
  keyboard's own. The overlay says when its picture next changes (the next frame, or the game going quiet), so the
  loop sleeps until then, as for an animation. `keyboard.rs` changed in three places only: the trait method and its
  `pub use`, the line that hands the picture over, and `takes_keyboard`, which now asks `ScreenState::draws()` (a
  game's frame counts as gglinux drawing, so the keyboard's region is still asked before anything is sent).
- **Which frames show.** The last event that "writes the screen" wins: a new show replaces the one playing, and
  after a show's last frame the screen goes back to the user's own (the doc's "return to the background image"),
  not to an earlier event's frame. An event writes the screen when it *changes*: a new value, or every event of a
  `value_optional` event. The same value again doesn't restart the frames; the doc warns about exactly this
  caching and points games to `value_optional`. Only the active game (the most recent, enabled, not stopped,
  within its timer) shows, as for the keys. A `screened-128x40` handler wins over a `screened` one of the same
  event; handlers for other sizes (`screened-128x36`, …) and zones are another device's.
- **Timing.** `length-millis` 0 (or none) holds the frame until another event; frames after a held frame never
  show. Otherwise frames play in turn; the last frame's `repeats` says how often the list plays: `false` once,
  `true` or `0` forever, a number *n* that many times in all (the doc says both "how many times to loop through
  the frames after the first time" and "the number of times to display the frames"; gglinux takes the second).
  `repeats` on other frames is ignored, as the doc says. Timing counts whole milliseconds from the event.
- **Layout (gglinux's own; the SDK gives none).** Rows of the keyboard's 8 × 8 font, centred left to right, the
  stack centred top to bottom, 2 px apart when they fit (5 rows fill the screen with none). A single-line frame
  whose text fits one row is drawn as big as fits, up to 3×, like the user's own text screen. `wrap: n` gives a
  line up to n more rows, breaking between words where it can; text that still doesn't fit is cut (no scrolling).
  `bold` draws the text twice, a pixel apart. A bar is an outline 8 px high across the row (a pixel in from each
  side), filled from the left, a pixel inside the outline. With an `icon-id`, the 32 leftmost columns are the icon's
  (the doc: "displayed in the 32 leftmost pixels") and the rows use the rest. Pictures are drawn as sent, most
  significant bit first.
- **Left out, and said so.** Icon pictures: the SDK's icon list (`event-icons.md`) isn't among the pinned docs, so
  the icon's 32 columns stay blank and the handler gets a note ("icon 7 is left out …"), shown by ggctl and the app.
  `arg` is GoLisp in GG; gglinux evaluates only the doc's own forms: `self`, `(key: e)`, `(/ a b)` (two whole
  numbers give a whole number), `(string-upcase e)`, literals and the empty `arg` (a static label). Anything else
  shows no value and gets a note. A picture of the wrong size in a `screened` handler draws dark with a note; in a
  `screened-128x40` handler it's an error, as is any malformed screen or bitmap handler (the game hears why, as for
  key handlers). The pinned doc has no `priority` key; "priority" here is the game frame's over the user's screen.
- **Bitmap key map.** The doc: colour `i` is on a 22 × 6 grid, "the first color specified corresponds to the top
  left of the keyboard, the 23rd color corresponds to the left side of the second row", "automatically mapped to the
  nearest appropriate keys", and "any parts of the array that do not map to a key on the user's keyboard are
  ignored". So the index order comes from the doc (row `i / 22`, column `i % 22`). The doc doesn't say how wide a
  cell is: gglinux reads the grid as a full-size keyboard's (6 rows: function row and the 5 below; 22 columns one
  key wide: 15 main, 3 navigation, 4 keypad), which is what makes "parts that don't map to a key" possible on a
  smaller board. Each key takes the cell under its middle on its row (US layout positions, as the GameSense zones
  use); the keypad's 4 columns land on no key of the TKL. Every key gets its own cell (tested). **This is gglinux's
  reading of the doc, not checked against GG**: a hands-on comparison (the same bitmap sent to GG on Windows and to
  gglinux) would settle it.
- **Composition.** Bitmap handlers paint in the same pass as colour handlers, in the order the events changed: later
  changes paint over earlier ones. A full `bitmap` that keeps updating covers every key, as the doc says ("control
  an entire keyboard's lighting each update"); `partial-bitmap` never paints its excluded events' keys, so those
  stay visible whatever order things change in. A cell that isn't `[r, g, b]` (0–255) paints nothing; colours past
  the 132nd are ignored; no `bitmap` in the frame paints nothing.
- **What an event keeps.** Only what its handlers read: colours for context-color handlers (as before), the text,
  numbers and true/false values its screen lines read (text cut at 256 characters), `image-data-128x40` only for a
  handler with a picture frame (640 bytes, else dropped), `bitmap` only for a bitmap handler, `excluded-events` only
  for a partial one (valid names, at most 128).
- **Screen care.** A game's picture is a picture of gglinux's: it moves with the burn-in shift and goes dark while
  nobody is at the computer, even over the keyboard's own screen (whose own care setting is about the keyboard's
  menus).
- **Rate.** A game can send events far faster than the screen should be rewritten (each picture is a 645-byte
  report). A game's pictures go out at most every 100 ms (`OVERLAY_GAP`, the floor an animation has too); a picture
  that changes sooner waits, the loop wakes when the gap is up, and the newest picture goes then, so the last frame
  is never lost. Going dark for nobody being there never waits.

## Why no new Experimental flag

The unit asked for `gamesense_screen` unless the existing switches are enough. They are:
- no new device command family: the screen picture (`1F 81`) and the hand-back (`1F 82`) are the ones gglinux's own
  text, clock, picture, animation and monitor screens already send ungated, and the keys use the existing lighting
  frames;
- nothing changes unless the user turned GameSense on (off by default; it's a part, and needs the keyboard part),
  a game is enabled (the per-game switch, on for a new game as before), and that game sends an event with a screen
  handler while it's the active game;
- the screen goes back by itself when the frames end or the game goes quiet, stops, is turned off or removed.

## Tests

- `gg-gamesense`: every screen example of the SDK doc as a fixture, drawn and compared pixel for pixel with
  pictures built from the layout above (text, prefix/suffix, bold, progress bar, wrap, accessors, icon, raw
  picture, the event's own picture, the doc's 128x36 picture, ranges); timing of `length-millis`, held frames and
  `repeats` (true, a count, 0, ignored on other frames); parse errors. Registry, on a made-up clock: a timed frame
  ending, a held frame lasting until the game goes quiet and coming back with a heartbeat, a new event replacing a
  show and a repeated value not, `value_optional` custom text, the event's own picture, the exact size winning, only
  the active game showing; a full bitmap, the doc's partial-bitmap example with and without the frame's
  `excluded-events`, a bitmap composed with a colour handler on another zone in both orders, summaries.
- `gglinuxd`: `ScreenState` with an overlay over a text setting and over the keyboard's own screen (sent once,
  handed back once), the loop's wake time, screen care on a game's picture, the 100 ms gap (the newest picture
  goes when it's up; going dark doesn't wait); GameSense's overlay through its clock
  (the test game's health on the screen, the screen test's three frames and the hand-back), game infos.
- `ggctl`, the app's Games row text, gg-ipc's JSON (and a GameInfo from an older daemon).

## Pictures

The frames as the keyboard gets them (every screen example of the SDK doc, the doc's own 128x36 picture in a
128x40 frame, the test game's frames) and each key's bitmap cell are written as text by an ignored test:
`GG_SCREEN_PICTURES=<dir> cargo test -p gglinuxd --bin gglinuxd -- --ignored screen_pictures` (add
`GG_SDK_IMAGE_128X36=<the doc's 576 numbers>` for the doc's picture). They were looked at for this note; the doc's
picture decodes to the SteelSeries logo, so the bit order is right.

## Hands-on checks for the owner

1. `ggctl gamesense on`, then `ggctl gamesense test screen`: the keyboard's screen shows "GameSense / on the
   screen", then "Progress" with a bar at 60 %, then "60 %" big, 1.2 s each, then your own screen again. Try it
   with your screen set to the clock, and to the keyboard's own.
2. "Run test" in Games: the F-keys sweep and the screen shows "Health N" with a bar, following the keys; 20 s later
   your own screen is back.
3. A real game with screen handlers (e.g. one listed under Games after it ran): its text should be readable and
   centred; tell us if text is cut that GG shows whole (GG may scroll long text; gglinux cuts it).
4. Bitmap: only with a game or tool that sends `bitmap`/`partial-bitmap`. Compare which keys light against GG on
   Windows if you can; the grid-to-key map is gglinux's reading of the SDK doc.
