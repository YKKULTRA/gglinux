# C5: lighting zones, an idle scene and Reflect (L02, L03, L04)

Unit c5-lighting of wave C (M12 in the full-parity plan). Closes the audit rows L02 (per-zone effect
composition), L03 (an idle lighting layer) and L04 (Reflect screen colours), as far as they can be built
without the owner's desktop.

## What GG does (sources)

- **Prism zones and layers.** SteelSeries support article 15899120883853 ("Creating different illumination
  configurations in Prism", audit source `prism_layers`): Prism has Active, Reactive and Idle configurations,
  each with its own per-zone effects.
- **Reflect.** Support article 15898869633165 ("What's the difference between Prism, PrismSync and Reflect",
  audit source `reflect`, pinned in `evidence/937c25bb6cad8c3b.txt`): Reflect "utilizes the colors extracted from
  designated sampling areas on your monitor and applies them to the selected illumination zones on your device",
  and "on keyboards you can do per-key Reflect customization". A second article in the same evidence file says
  Reflect doesn't work with HDR and samples the primary monitor.

Everything else here is gglinux's own: the zone presets (WASD, arrows, F-row, number row, every key), the
five-minute idle default, the idle scenes' slow speeds, the frame cap (15 a second), the smoothing (halfway per
frame) and the sampling grid (4 × 4 points per key). None of them is GG's.

## Data (gg-ipc, `lighting.rs`)

`Lighting` gains two fields, both optional in old files:

- `zones: Option<Vec<Zone>>`, the stack, bottom first; later zones paint over earlier ones. A `Zone` is a
  `name`, its `keys` (`{"kind":"all"}` or `{"kind":"keys","codes":[…]}`) and an `effect` (`ZoneEffect`:
  `static`, `per_key`, `breathing`, `wave`, `gradient`, `spectrum`, `visualizer`, `reactive`, `reflect`).
- `idle: IdleLighting { enabled, after_minutes, zones }`: the idle scene's own stack; empty is every key dark.

**Migration is by reading, not rewriting.** `zones: None` (every file and every client from before) reads
through `Lighting::stack()` as one zone, `all`, over every key, with `ZoneEffect::from(effect)`; the old
static effect becomes `per_key`, which shows `Lighting::keys` exactly as before. `None` is not written back
(`skip_serializing_if`), so an old file saved again is the same JSON plus the idle defaults (test
`lighting_saved_before_zones_reads_as_one_zone_over_every_key_and_saves_back_the_same`, with the daemon's real
pretty-printed file shape). `zones_mut()` writes the stack out the first time a zone is edited. This keeps
code that builds a `Lighting` with only `effect` (profiles, presets, older apps) working unchanged; `effect`
counts only while `zones` is `None`. `Some(vec![])` is a legal, dark stack.

A wave or gradient runs across its own zone, from its leftmost key's centre to its rightmost; the `all` zone
runs across the whole board (0 to 18.25 key units), so it draws exactly what the one effect drew (test
`the_one_zone_of_old_lighting_draws_exactly_what_the_effect_drew`). Reactive and Reflect zones are
see-through where they have nothing to show: between presses, and before a frame of the screen has come.

The global reactive overlay, brightness and the mic-mute light stay as they were, over the whole stack.

`KeyboardState` gains `lighting_idle` (the idle scene is drawn now) and `reflect: ReflectStatus`
(`off`, `running`, `stopped {why}`).

## Daemon

- `keyboard/zones.rs`: a `Scene` built once a frame from the stack and the board; `color(code, inputs)`
  paints a key through every zone that has it.
- `keyboard/effects.rs`: `frame_color` = the scene's colour, the global presses over it, then brightness.
- `keyboard/idle.rs`: the idle seam. "Nobody there" is the same word screen care rests on:
  `presence.rs` sends `Command::SetAway` from logind (`Active`, `LockedHint`, `IdleHint`), the screensaver's
  `GetActive`, and sleep. The idle scene starts `after_minutes` after the first "away" and ends at the first
  "back". Input devices are never read. Because the desktop's own idle time comes first, the minutes are on top
  of it; 0 means as soon as the desktop says so.
- `keyboard/reflect.rs`: the `ScreenSampler` trait (`latest() -> Result<Option<Frame>, String>`, frames are
  BGRx with a stride) and `Reflect`, which starts a sampler while a Reflect zone is drawn with its gate on,
  takes at most 15 frames a second, maps each frame onto the keyboard's own board (from `layout::for_region`)
  with a 4 × 4 point grid per key, and moves each key halfway toward its new colour per frame. A sampler that
  won't start or ends is tried again after 5 s.
- `keyboard.rs`: the loop picks the active stack (the idle scene's while it shows), draws it, runs the key
  reader for reactive zones too, the audio capture for audio zones too, and syncs Reflect. Frames stream at
  30 a second while any zone of the active stack animates.

**The real sampler isn't built.** The ScreenCast portal (`org.freedesktop.portal.ScreenCast`: CreateSession,
SelectSources with `persist_mode` 2 and a stored `restore_token`, Start, OpenPipeWireRemote) hands back a
PipeWire node; reading video frames from it needs a PipeWire crate (`pipewire` 0.8, `libspa`), which the
project doesn't have. `start_portal()` returns `ReflectStatus::NOT_BUILT` and opens nothing; the one-time
screen pick is to be done with the owner. Everything after the sampler (gate, cap, smoothing, mapping, CPU) is
proven with a fake that serves synthetic frames and, like a real stream, has no frame on the first ask.

**Gate.** `Feature::LightingReflect` ("Lighting: Reflect screen colours"), off by default. While it's off,
a `set_lighting` that brings a Reflect zone in is refused with the gate's sentence; lighting that already has
one still changes otherwise, and its Reflect zones stay see-through with status `stopped`. Turning the gate off
stops the capture at once.

**CPU.** Mapping a frame reads 16 points per key (about 1,350 pixels for the 84-key board), whatever the
frame's size. The test `ten_seconds_of_full_hd_frames_cost_little_cpu` runs 150 synthetic 1920 × 1080 frames
through the whole path and requires under 5 ms a frame (the budget at 15 a second is 66 ms); it prints the
measured time: 92 µs a frame (14 ms for all 150) in a debug build on the owner's PC, 2026-11-13.

## ggctl and the app

- `ggctl lighting [on|off]`, `lighting zones add NAME KEYS EFFECT…`, `zones set|keys|rm|up|down`,
  `lighting idle on [MIN]|off`, `idle scene off|EFFECT…`, `lighting reflect on|off`. Each edit reads the lighting
  twice (a lagging daemon may answer the first read with the old one), edits it and sends it back.
  `reflect on` puts an every-key Reflect zone over the last every-key zone, so smaller zones still show over
  the screen's colours; `reflect off` removes Reflect zones.
- The app's lighting panel: the zone list (a click picks a zone for editing and picks its keys on the keyboard
  picture), add from a preset or the picked keys, up/down/remove, the edited zone's effect and its colours and
  speeds, "Use picked keys for …", painting as before (applying paint makes a zone show painted keys if none
  does), a "While you're away" section (on, minutes, one of five scenes) and a Reflect switch greyed with the
  gate's sentence while the gate is off.

## Not done

- The real ScreenCast/PipeWire sampler (see above), and with it HDR and monitor choice.
- The app edits the idle scene as one effect over every key; a full idle stack can be set with ggctl or the
  IPC only (the app then says so).
- The fake daemon's lighting answers at once, as it did before this unit; only its Reflect status follows
  the gate.
