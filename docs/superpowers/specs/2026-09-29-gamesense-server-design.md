# GameSense server (M6a) — design

**Status:** approved in conversation 2026-09-29. **Milestone:** M6, part a. M6b, the
game-independent effects, is a separate spec; it plugs into the overlay interface defined here.

## Goal

gglinux speaks SteelSeries' **GameSense SDK**, so games and apps that support it can drive the
Apex Pro TKL Gen 3's lighting. That includes Windows games under Proton, which find the server
through a `coreProps.json` that gglinux writes into each Proton prefix.

The owner accepts that nothing installed today uses the SDK. This spec found none: no
`coreProps.json` or GameSense strings in any installed game's binaries. CS2 lighting in GG goes
through Valve's Game State Integration instead; that's a different feature and out of scope here.

Success:
- a GameSense client (curl, the built-in Test game, or a real game) lights the zones it binds;
- a Proton game finds the server with no manual setup;
- with gglinux lighting off, a game takes the keyboard over and hands it back 15 s after going
  quiet;
- no web page can drive the server;
- turning GameSense off removes every file it wrote.

Out of scope:
- screen handlers (the Apex OLED is blocked, M4b; the base-station screen stays dark for burn-in
  protection);
- tactile handlers, mouse and headset zones (no such hardware here);
- CS2 Game State Integration;
- the M6b built-in effects;
- Flatpak Steam;
- a native-Linux `coreProps.json` location (none exists).

## Facts

**SDK** (`SteelSeries/gamesense-sdk` docs, read 2026-09-29):

- **Endpoints**, all POST with JSON bodies:
  - `/game_metadata`: `game`; optional `game_display_name`, `developer`,
    `deinitialize_timer_length_ms` (1000–60000, default 15000).
  - `/register_game_event`: `game`, `event`; optional `min_value` (0), `max_value` (100),
    `icon_id` (0), `value_optional` (false).
  - `/bind_game_event`: the `register_game_event` fields plus `handlers`.
  - `/game_event`: `game`, `event`, `data: { value: int, frame?: object }`.
  - `/multiple_game_events`: `game`, `events: [ { event, data } ]`.
  - `/game_heartbeat`, `/stop_game`, `/remove_game`: each takes `game`.
  - `/remove_game_event`: `game`, `event`.
  - `GET /supports_multiple_game_events`.
- **Names:** `game` and `event` allow only `A-Z 0-9 - _`.
- **Timing:** a game initialises on its first event and deactivates after its deinitialize timer
  with no event or heartbeat.
- **Repeated values:** with `value_optional: false`, an event repeating its cached value is not
  re-processed.
- **Colour handler fields:**
  - `device-type`: `keyboard`, `rgb-per-key-zones`, `rgb-zoned-device`, `rgb-N-zone`, and
    others;
  - `zone` (named), or `custom-zone-keys` (an array of HID codes) instead;
  - `mode`: `color` | `percent` | `count` | `context-color`;
  - `color`: `{red, green, blue}`, or `{gradient: {zero, hundred}}`, or ranges
    `[{low, high, color}]` with inclusive bounds;
  - `rate`: `{frequency, repeat_limit}`, each a number or ranges `[{low, high, frequency|repeat_limit}]`;
    frequency 0 (the default) means steady;
  - `context-frame-key`, used by `context-color` mode.
- **Mode semantics:**
  - `percent`: lights the zone's keys in order for a 0–100 value, dimming the last lit key by the
    fraction it's lit.
  - `count`: lights `value` keys.
  - Zone order is left to right, then top to bottom.
- **Per-key zones:** named keys (`a`–`z`, `keyboard-1`–`keyboard-0`, `f1`–`f12`, `escape`,
  `spacebar`, `l-ctrl`, …) and key groups (`function-keys`, `number-keys`, `q-row`, `a-row`,
  `z-row`, `main-keyboard`, `nav-cluster`, `arrows`, `keypad`, `keypad-nums`, `macro-keys`,
  `all-macro-keys`, `all`, `logo`). `keyboard` zones are `function-keys`, `main-keyboard`,
  `keypad`, `number-keys` and `macro-keys`.

**This machine** (2026-09-29):
- native Arch `steam`, one library at `~/.local/share/Steam` (from `libraryfolders.vdf`), 17
  `compatdata` prefixes;
- PROTOCOLS.md §4: pressure-vessel shares the host network namespace, so a Proton game reaches
  `127.0.0.1:<port>` on the host.

## Design

### 1. Engine: new crate `gg-gamesense` (pure, no I/O)

**`proto`:**
- serde types for every request, and name validation (`^[A-Z0-9_-]{1,128}$`);
- handlers parse leniently: an unknown `device-type`, or a screen/tactile/mouse/headset handler,
  is kept as *unsupported* and never painted, so a game's whole bind doesn't fail over one handler.

**`zones`:** named zones and keys mapped to HID codes on `layout::ANSI_TKL`, ordered left to
right and top to bottom:

| Zone | Keys on the TKL |
|---|---|
| `function-keys` | F1–F12 |
| `number-keys` | 1–0 |
| `q-row` | Q–P |
| `a-row` | A–L |
| `z-row` | Z–M |
| `main-keyboard` | the alphanumeric block, number row to bottom row (not Esc, the F-row, nav cluster or arrows) |
| `nav-cluster` | Ins, Home, PgUp, Del, End, PgDn |
| `arrows` | ↑ ← ↓ → |
| `all` | every key |
| `keypad`, `keypad-nums`, `macro-keys`, `all-macro-keys`, `logo` | none (painting them does nothing) |

Single-key names map to their code; `ss-key`, `pound` and the `m0`–`m5` macro keys map to
nothing. `custom-zone-keys` keeps its given order, dropping codes not on the layout.

**`registry`:**
- Games: metadata, a per-game `enabled` flag (default on), and events (bounds, `value_optional`,
  handlers).
- Live state per event: the last value, the last frame, and when that value arrived.
- Per game: the last activity time (any event or heartbeat) and whether it was stopped. A new event
  or heartbeat after `stop_game` makes the game active again.
- Every mutation takes an explicit `now` (monotonic seconds, `f64`), so the registry is testable
  without a clock.

**`paint(now) -> Option<Layer>`**, where `Layer` maps HID code → RGB:
- **The active game** is the enabled, not-stopped game whose last activity is within its
  deinitialize timer and is the most recent. `None` if there isn't one.
- **Which events paint:** those with a value (or with `value_optional` and at least one event
  received). Where they overlap, the one whose value changed most recently wins.
- **Percent:** `pct = clamp((value − min) / (max − min) · 100, 0, 100)`. A gradient uses `pct`.
- **Ranges** (colour and rate) compare the raw value, bounds inclusive; a value outside every
  range paints nothing.
- **`color`:** all zone keys take the colour.
- **`percent`:** `pct/100 · n` keys lit in order. The partial key is scaled by its fraction and
  the rest of the zone is black, because the zone belongs to the event.
- **`count`:** the first `clamp(value, 0, n)` keys are lit and the rest black.
- **`context-color`:** the colour is `frame[context-frame-key]` as `{red, green, blue}`. If
  that's missing, nothing is painted.
- **Flashing:** with frequency `f > 0`, the colour is on for the first half of each `1/f` period,
  counted from when the value arrived, and off (black) for the second half. After
  `repeat_limit` flashes it stays on.
- **Repeated values:** a repeated identical value doesn't restart the flash count, unless
  `value_optional`.

**Limits:** at most 64 games, 128 events per game and 32 handlers per event. Beyond these the
request is rejected with an error.

### 2. Server (daemon: `gglinuxd/src/gamesense.rs`)

**Transport:**
- `axum` on `127.0.0.1:<port>`, on the daemon's tokio runtime.
- The port is picked once (a free ephemeral port) and saved. If it's taken on a later start, a new
  one is picked and saved, and the prefixes are rewritten.
- Served HTTP/1 only, through hyper-util with a timer, so no local program can hold the daemon's
  file descriptors: at most 32 connections at once (more wait in the listen queue); headers must
  arrive within 5 s, which also closes a kept-alive connection left idle that long; a whole request
  gets 10 s, then 408. Stopping closes every connection, giving a request in progress 0.5 s.

**Request checks** (a browser can't pass them):
- any request carrying an `Origin` header → 403;
- a POST whose `Content-Type` isn't `application/json` → 415;
- bodies over 64 KiB → 413.

**Responses:** success is 200 with `{}`; errors are 400 with `{"error": "<message>"}`.

**Persistence:**
- `~/.config/gglinux/gamesense.json` holds the `enabled` switch, the `port`, and each game's
  metadata, `enabled` flag, events and handlers, the handlers as the JSON the game sent.
- Live values aren't saved.
- Written atomically, as `lighting.json` is.

**The `enabled` switch** defaults to **off**:
- On: start the server, write the prefixes, scan for new ones.
- Off: stop the server and remove our files.

**Test game:** a `gamesense_test` request runs a built-in game through the same registry code, in
process, not over HTTP. It's the game `GGLINUX_TEST` ("gglinux test"), with one event, `HEALTH`:
- `percent` mode on `function-keys`;
- a gradient from red at 0 to green at 100;
- 4 Hz flashing for values 0–20;
- a 20 s sweep 100 → 0 → 100, one value every 100 ms, then `stop_game`.

### 3. Proton bridge (`gg-gamesense::prefixes`, file I/O behind plain functions)

- **Libraries:** read `"path"` entries from `libraryfolders.vdf` under `~/.local/share/Steam`
  and `~/.steam/steam`, deduplicated by canonical path.
- **Prefixes:** every `<library>/steamapps/compatdata/<id>/pfx/drive_c/ProgramData` that exists.
- **Write:** `ProgramData/SteelSeries/SteelSeries Engine 3/coreProps.json` =
  `{"address":"127.0.0.1:<port>"}`.
  - Directories are created as needed.
  - Nothing is written if the file already holds exactly that.
  - **A file that isn't ours is never overwritten**, e.g. a real GG install inside a prefix. "Ours"
    means it parses as JSON whose only key is `address`, with a `127.0.0.1` address.
- **Scan:** every 1 s while enabled, so a new prefix gets the file while Proton is still setting
  it up. `libraryfolders.vdf` is re-read every 30 s.
- **Remove** (on disable): delete only files that are ours, then the two directories if empty.
- The UI reports how many prefixes currently hold our file.

### 4. Keyboard worker: overlays and takeover

- **The overlay interface:** `trait Overlay: Send + Sync { fn paint(&self, now: Instant) -> Option<Layer> }`.
  The keyboard worker gets a list of overlays at spawn; GameSense is the first, and M6b adds more.
  A new `Command::Wake` makes the worker draw at once; the GameSense server sends it after every
  event.
- **Order:** base effect → reactive → overlay keys on top (they replace the colour). Brightness
  applies to everything.
- **Frames:** 30 fps while any overlay is active.
- **Takeover:** frames are drawn when lighting is enabled **or** an overlay is active. While an
  overlay is active with lighting off, keys it doesn't paint show the saved effect. The onboard
  profile can't be shown, because gglinux can't read it.
- **Hand-back:** when the last overlay goes inactive with lighting off, the worker hands back to
  the onboard profile. That's 15 s after the game goes quiet, by default.
- Reactive lighting still requires lighting on, as before.

### 5. IPC and UI

**`gg-ipc`:**
- `GameInfo { name, display_name, developer, enabled, active, events: Vec<String> }`;
- `GameSenseState { enabled, address: Option<String>, prefixes: u32, games: Vec<GameInfo> }`;
- requests: `get_gamesense_state`, `set_gamesense_enabled { enabled }`,
  `set_game_enabled { game, enabled }`, `remove_game { game }`, `gamesense_test`;
- event: `gamesense_changed { state }`;
- the fake daemon handles all of these.

**UI:** a **Games** tab with:
- the GameSense switch, with a note that turning it on writes `coreProps.json` into Steam's Proton
  prefixes and turning it off removes them;
- the address, and "in N Proton prefixes";
- **Run test**;
- the games list: display name (else name), developer, an active dot, events, an on/off switch,
  and Remove.

## Testing

**Unit (`gg-gamesense`):**
- the SDK's documented example JSON parses;
- names are validated;
- unsupported handlers are kept and ignored;
- every zone's codes are on the layout, in order;
- `color` / `percent` (partial dimming, the rest black) / `count` / `context-color`;
- static, gradient and range colours, and a value outside all ranges;
- `min`/`max` scaling;
- flashing on/off timing and `repeat_limit`;
- a repeated value doesn't restart flashing, unless `value_optional`;
- the most recently changed event wins an overlapping key;
- the active game, the deinitialize timeout, `stop_game`, and a disabled game;
- the limits.

**Prefixes:** against a temporary fake Steam tree:
- the vdf is parsed;
- files are written only where `ProgramData` exists;
- writes are idempotent;
- a foreign file is never overwritten;
- removal deletes only our files and empty directories.

**Server:** in process, through tower's `oneshot`:
- `Origin` → 403, wrong content type → 415, oversize → 413;
- register → bind → event → the overlay paints;
- a heartbeat keeps the game active;
- `stop_game`.

**Keyboard:**
- composition order;
- takeover while lighting is off, and hand-back once the overlay goes inactive (a pure decision
  helper).

**Hardware (the owner):**
1. Turn GameSense on: the file appears in a prefix
   (`find ~/.local/share/Steam/steamapps/compatdata -name coreProps.json`).
2. Run Test: an F-key health bar sweeps and flashes when low.
3. With lighting off, Test takes the keyboard over and hands it back after it stops.
4. Turn GameSense off: the files are gone.
5. Record the prefix files in the system-changes note.
