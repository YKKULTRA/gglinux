# Parity with SteelSeries GG: where each feature stands

One row per feature of the GG feature audit of 2026-11-05 (83 rows, ids as in the audit), brought up to the code of
M11 (2026-11-13). The audit compared gglinux at `30620d8` with GG; this file says what has happened to each row since.
The byte-level detail behind each device row is in [PROTOCOLS.md](PROTOCOLS.md), and how the parts fit together in
[ARCHITECTURE.md](ARCHITECTURE.md).

## What the statuses mean

| Status | Meaning |
|---|---|
| verified on his hardware | Passed on the owner's own devices: a PASS line in the hardware test log or the hands-on acceptance log of 2026-11-13 (both kept with the M11 captures, not in this repository; the hardware log's times are PC local time, UTC+3, the hands-on log's UTC). E07 and E08 were verified earlier, in M4a and M2 (see PLAN.md). The note names any part of the row that wasn't tried. |
| built, waiting for hands-on | Built and tested against fake devices. It sends a device something that hasn't passed on his hardware yet, or needs his fingers or eyes to judge. |
| built, software only | Built and tested against fakes. It runs on the PC and sends the devices nothing new (only requests that exist already); it still has to be tried on his desktop. |
| dropped by owner | Not going to be built: on 2026-11-13 the owner cut further Sonar and Moments work and the Aim Trainer ("lets just cut sonar, or keep the features you built until now, do the same for moments also, do not add aim trainer and stuff / i never use these anyway", "keep the work tho, dont just delete it"). What was already built stays and keeps working; the note says what that is. |
| deliberately omitted | Left out on purpose, for the reason in the note. |
| broader scope | Needs hardware or a transport this project doesn't target. |
| not a gap | GG doesn't offer it for these devices, or there is nothing to do here. |
| not started | Wanted, not built yet. A row counts as built only when someone can use it; partly built work is in the note. |

A gate is an experimental switch (Settings → Experimental, or `ggctl experimental <name> on`): off by default, and
while it is off the daemon refuses the feature's device commands. "Captures" are our own USB captures of GG 123
(`apex-*`, `rival-*`, `arctis-*`); "R2" is the review of wave B.

## Count

| Status | Rows |
|---|---|
| verified on his hardware | 14 |
| built, waiting for hands-on | 15 |
| built, software only | 13 |
| dropped by owner | 24 |
| deliberately omitted | 1 |
| broader scope | 3 |
| not a gap | 6 |
| not started | 7 |
| **all** | **83** |

## Mouse (Rival 3 Wireless Gen 2)

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| M01 | Sleep timer / battery-saving controls | verified on his hardware | `gg-device-mouse` (`2B`, read `AB`); `ggctl mouse sleep`; mouse card | `mouse_power` | Read back from the mouse after the pacing fix (hardware log, 15:20–15:45); the app's control passed (hands-on step 8a); a 1-minute timer put it to sleep after about a minute (hands-on log, 15:30). Bytes from captures rival-1 … rival-3b. |
| M02 | High-Efficiency Mode | verified on his hardware | `2E 00`, read `AE`; `ggctl mouse radio efficient` | `mouse_power` | Read back from the mouse (hardware log, 14:39–14:57). What it does to battery life wasn't measured. |
| M03 | Wireless Stability Enhancement | verified on his hardware | `2E 02 01`, read `AE`; `ggctl mouse radio stable`; mouse card | `mouse_power` | Read back from the mouse (hardware log); the app's control passed (hands-on step 8a). |
| M04 | Bluetooth Smoothing | not started | — | — (`mouse_power`'s description names it; nothing uses it) | GG sends nothing over the 2.4 GHz dongle when it is toggled; it applies only in Bluetooth mode (Rival captures, 2026-11-08), and gglinux talks to the mouse only through the dongle. |
| M05 | Recorded / timed macros | not started | — | — | GG plays mouse macros on the PC: a macro on a button sends the mouse only the save `09` (captures rival-4 … rival-7). A gglinux version would run on the PC, like the spare-key actions (M09), not send a mouse command. |
| M06 | Separate X and Y DPI in the desktop app | built, software only | `gglinux-ui` mouse panel ("Separate X and Y") | none | The app sends the per-axis request that already existed. Separate X and Y DPI was felt on the mouse when set through `ggctl` (hands-on step 7) and read back (hardware log); the app's own X/Y boxes haven't been used by hand yet. |
| M07 | Choose keyboard keys as mouse-button bindings in the GUI | verified on his hardware | `gglinux-ui` mouse panel, "Keyboard key…" / "Media key…" | none | Picking a key in the app for the Back button typed `b` (hands-on log, 15:29). |
| M08 | Re-pair mouse and dongle | not a gap | — | — | GG 123 shows no pairing control for this mouse (captures rival-4 … rival-7), so there is nothing to copy. |
| M09 | System actions and text-based bindings | built, software only | `gglinuxd/src/actions.rs`; Settings → Actions; `ggctl actions` | none | A mouse button sends a spare key F13–F24 (an ordinary key binding); the daemon hears it through the desktop's shortcuts portal and runs a program, types text (wtype, on wlroots desktops), applies a profile, saves a clip or presses a media key. Binds for all twelve keys are in the owner's Hyprland config, but no action has been fired in a test yet; typing text on Plasma waits for a session with him. |
| M10 | Angle snapping / movement straightening | not started | — | — | GG does it on the PC: saving it sends the mouse only `09` (capture rival-3b). A gglinux version would have to filter the mouse's movement on the PC, which needs read access to its input node (a udev rule: ask the owner first). |
| M11 | Acceleration and deceleration tuning | not started | — | — | Same as M10: done on the PC by GG, nothing sent to the mouse (capture rival-3b). |
| M12 | Lift-off distance / advanced sensor controls | not a gap | — | — | GG 123 shows no lift-off or advanced sensor setting for this mouse (captures rival-4 … rival-7). |
| M13 | Direct Bluetooth configuration | broader scope | — | — | gglinux, like rivalcfg, talks to the mouse through its 2.4 GHz dongle only. Configuring it over a Bluetooth link would be a second transport, and nothing shows GG doing that for this mouse. |
| M14 | Custom RGB / configurable indicator colours | not a gap | — | — | SteelSeries says this mouse's indicator colours can't be changed in GG; the wireless Gen 2 has no RGB lighting. |

## Keyboard (Apex Pro TKL Gen 3)

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| K01 | Protection Mode | built, waiting for hands-on | release modes `03`/`04` in `38 62`; `ggctl switches protection`; switches panel | `keyboard_protection` | Bytes from captures apex-2 … apex-8. The gate's refusal and the tables going out without error passed (hardware log, ~14:40), but whether a protected key ignores a light touch wasn't tried: on hold, like every switch table, since the phantom key presses of 2026-11-13 (hands-on step 4 skipped). |
| K02 | Dual actuation / 2-in-1 action keys | built, waiting for hands-on | second point `38 61 44 02` and deep layer `36 02`; `ggctl bind deep+w …`; switches and key bindings panels | `keyboard_bindings` | GG's exact bytes typed `w` on a light press and `wxw` on a full one in a finger test through a probe (hands-on F1). Through the daemon, deep-layer writes were dropped at random (4 of 16); every layer write is now read back and written again (b1) and put back on failure (R2 S3), not yet tried. On hold with the switch tables. GG's limit of 8 deep-press bindings is enforced; GG's Dual Bindings in a slot aren't written. |
| K03 | Normal key remapping and key combinations | verified on his hardware | `36` normal layer; `ggctl bind`; key bindings panel | `keyboard_bindings` | Caps Lock → `b` typed `b` (hands-on step 1); Caps Lock → Ctrl+A read back from the keyboard (hardware log). Combinations were read back, not typed. |
| K04 | Fn / SteelSeries layer editing and trigger selection | verified on his hardware | `36` Fn layer, `35`, `3C`; `ggctl bind fn+…`; key bindings panel | `keyboard_bindings` | Fn+F1 → `c` typed `c` (hands-on step 2); Fn+F1 → mute read back with the keyboard's nine own Fn shortcuts unchanged (hardware log). Moving the Fn key (trigger selection) hasn't been tried on the keyboard. |
| K05 | Macro recorder, library and playback bindings | built, waiting for hands-on | `gg-device-keyboard/src/macros.rs`; `ggctl macro`; macros panel | `keyboard_macros` and `keyboard_onboard` | Written into an onboard slot exactly as GG stores them (captures apex-20 … apex-22, apex-30): once, a number of times, while held, or toggled, with the wait between plays. No recorder: steps are typed as words (`a`, `ctrl+c`, `wait:100`, `text:"…"`). Two keys and 64 steps a macro for now. A write never loads the slot. Never on the keyboard yet. |
| K06 | Write settings to onboard flash | verified on his hardware | slot transaction `02`/`03`/`83`/`B1` (`Keyboard::write_slot`); `ggctl onboard write\|restore` | `keyboard_onboard` | On spare slot 5: a backup written back, and a new screen picture written; both read back byte for byte (hardware log, 14:10–14:11). What a write can put in a slot: the screen picture, Rapid Tap pairs and macros; not yet the per-key switch tables or other bindings. |
| K07 | Onboard profile-slot management | verified on his hardware | `83` reads, backups, `B2` loads; `ggctl onboard`; onboard panel | `keyboard_onboard` | All five slots read twice, byte-identical, CRC and schemas right, backups saved (hardware log, 13:31). Loads went out without error, but what the keyboard did after one wasn't watched; switching slots by hand was skipped (hands-on step 6), since a load also swaps the switch tables. |
| K08 | Read existing settings instead of displaying host intent | verified on his hardware | binding reads `B6`, slot reads `83`; `ggctl bind read`, `ggctl onboard read` | `keyboard_bindings`, `keyboard_onboard` | All three binding layers read back three times, identical, after the 13 ms fix (hardware log). The switch tables have no read: for them gglinux shows what it sent. |
| K09 | Animated OLED GIFs | verified on his hardware | `gglinuxd/src/keyboard/screen/anim.rs`; `ggctl screen gif`; screen panel | none | A 12-picture GIF at 0.1 s a picture looked right to him (hands-on step 5c). |
| K10 | OLED System Monitor | verified on his hardware | `keyboard/screen/sysmon.rs`, `gpu.rs`; `ggctl screen monitor`; screen panel | none | CPU, CPU temperature, RAM and GPU, then GPU, GPU temperature and the clock, looked right (hands-on steps 5a, 5b); a GPU temperature bug was found and fixed on the way (hardware log). The rule that a sleeping graphics card is never woken isn't exercised on his PC, whose card is always awake. |
| K11 | Non-US keyboard layout/region-aware editor | built, software only | `gg-device-keyboard/src/layout.rs`; the app's keyboard picture | none | The region read works on his US board and the US Gen 3 board is drawn (hardware log). The ISO and JIS drawings, and which analog keys they lack, are inferred: no such keyboard to check. GameSense zones and bitmaps use the US positions. |

## Profiles

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| P01 | Full device/game profiles | built, software only | `gglinuxd/src/profiles/`; `ggctl profile`; profiles panel | each section's own gate | Six sections (headset, mouse, switches, key bindings, screen, lighting), applied through the daemon's own handlers; a gated part is skipped with a note. No Sonar section (Sonar is frozen). Not tried on his desktop yet. |
| P02 | Foreground / Alt-Tab aware switching | built, software only | `gglinuxd/src/profiles/focus.rs`; KWin script `gglinux-focus` | none | Follows the focused window on Hyprland (its event socket) or on KDE Plasma with gglinux's KWin script (in the Arch package; turn it on once), after half a second; your own settings come back when you switch away. Tested against a fake Hyprland socket; the KWin script hasn't run in KWin yet. |
| P03 | QuickSet ready-made game preset library | built, software only | `gglinuxd/src/profiles/presets/`; `ggctl profile preset` | none | gglinux's own switch presets (Competitive FPS, MOBA, Typing, Default switches), not SteelSeries', to apply or copy; profiles import and export as JSON files. Applying one turns gglinux's switch settings on, which are on hold on his keyboard. |

## Lighting

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| L01 | PrismSync / cross-device coordination | not a gap | — | — | The keyboard is the only SteelSeries RGB device here (the Rival 3 Wireless Gen 2 has no RGB, and the Nova Pro isn't a Prism target), so there is nothing to sync. |
| L02 | Prism-style per-zone effect composition | built, software only | `gglinuxd/src/keyboard/zones.rs`, `gg-ipc/src/lighting.rs`; `ggctl lighting zones`; keyboard card | none | Named zones (presets or picked keys), each with its own effect, stacked; saved lighting becomes the zone "all" unchanged. Drawn with the lighting frames verified in M4a. |
| L03 | Independent Active / Reactive / Idle lighting layers | built, software only | `gglinuxd/src/keyboard/idle.rs`; `ggctl lighting idle`; keyboard card | none | An idle scene some minutes after the desktop says you're away (logind or the screensaver), not after the last key press (that would need to read the keyboard). The app sets one effect over every key; `ggctl` can set a whole stack. |
| L04 | Reflect screen-colour sampling | not started | `gglinuxd/src/keyboard/reflect.rs` (partly) | `lighting_reflect` | Partly built: the zone effect, its gate and a fake screen for tests. The screen capture itself (the ScreenCast portal with a PipeWire video stream, a new crate, and a one-time screen pick with him) isn't, so a Reflect zone shows "stopped". |

## Headset (Arctis Nova Pro Wireless)

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| H01 | Bluetooth auto-on at headset startup | built, waiting for hands-on | `06 b2`; `ggctl headset bluetooth-at-power-on`; headset card | `headset_extras` | Bytes from capture arctis-2. |
| H02 | Bluetooth call behavior: do nothing / attenuate / mute game audio | built, waiting for hands-on | `06 b3`; `ggctl headset bluetooth-call`; headset card | `headset_extras` | Bytes from capture arctis-2: nothing, game 12 dB lower, or game muted. |
| H03 | Bluetooth connection and radio-pairing status in UI | built, software only | decoded from the `06 b0` poll; `ggctl headset`; headset card | none | Read from replies the daemon already asks for every second; the bytes agree with captures arctis-1 (headset off) and arctis-5 (on). GG shows no pairing command for this model, so none is offered. |
| H04 | High / low hardware gain control | built, waiting for hands-on | `06 27`; `ggctl headset gain`; headset card | `headset_extras` | Bytes from capture arctis-1. It is the base station's output gain (the old code called it a mic gain). Try it at low volume first. |
| H05 | Line-out Speakers / Stream mode | built, waiting for hands-on | `06 43`; `ggctl headset line-out`; headset card | `headset_extras` | Bytes from capture arctis-3. |
| H06 | Hardware line-out stream mix: Main / Aux / Mic | built, waiting for hands-on | `06 47`; `ggctl headset stream-mix`; headset card | `headset_extras` | Bytes from capture arctis-3. The app's sliders send the other two levels as gglinux last wrote them (R2 H1), until a read-back with three different levels has been checked. |
| H07 | Detailed / simple base-station home view | built, waiting for hands-on | `06 89`; `ggctl headset home-screen`; headset card | `headset_extras` | Bytes from capture arctis-2. |
| H08 | Read current EQ, mic volume, sidetone and gain | built, software only | decoded from the `06 20` poll; `ggctl headset`; headset card | none | Read from the reply the daemon polls every second. The offsets are Arctis Sound Manager's and agree with capture arctis-5, but neighbouring bytes held equal values there, so not every offset is proven (R2 H2). |
| H09 | Native OLED dim timer / idle-brightness settings | built, waiting for hands-on | `06 83`, `06 80`; `ggctl headset screen-dim`; headset card | `headset_extras` | The dim timeout from captures arctis-2, arctis-3 and arctis-5 (60 min = `83 06`), read back from `06 80`. GG shows no separate idle brightness for this model, so none is offered. |
| H10 | Physical ChatMix dial integration | not started | — | — | The owner lifted the old exclusion (2026-11-06), but his base station's knob is damaged; if it is built, it would be off by default. |
| H11 | USB source selection / base-station pairing commands | not a gap | — | — | GG 123 shows no USB source or pairing control for this base station (captures arctis-1 … arctis-3). |

## Sonar

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| S01 | Spatial / virtual surround audio | dropped by owner | — | — | No spatializer was built. |
| S02 | Different physical output per channel | dropped by owner | `gg-audio` | — | Built and kept: one physical output shared by all channels. |
| S03 | Prioritized backup-device switching and exclusions | dropped by owner | — | — | Not built. |
| S04 | Microphone parametric EQ | dropped by owner | `gg-audio` | — | Built and kept: the mic's noise suppression, gate and compressor; no mic EQ. |
| S05 | Incoming Chat noise reduction | dropped by owner | — | — | Not built. |
| S06 | Game/mic preset catalog and preset sharing | dropped by owner | `gg-audio` | — | Built and kept: EQ editing, AutoEQ import, settings backups; no preset library. |
| S07 | Mute states and keyboard shortcut controls | dropped by owner | `gg-audio` | — | Built and kept: channel volumes (down to zero); no mute states or shortcuts. |
| S08 | Include and submix microphone in the Stream output | dropped by owner | `gg-audio` | — | Built and kept: the Stream mix of the four playback channels, without the mic. |
| S09 | Microphone test recorder / playback monitor | dropped by owner | — | — | Not built. |
| S10 | Separate per-channel volume boost | dropped by owner | `gg-audio` | — | Built and kept: channel volume and the headphone preamp. |
| S11 | Built-in Game / Chat / Spatial test sounds | dropped by owner | — | — | Not built. |
| S12 | Separate noise / impact reduction controls | dropped by owner | `gg-audio` | — | Built and kept: RNNoise on the mic. |
| S13 | Hide/disable Media and Aux individually | dropped by owner | — | — | Not built. |
| S14 | Channel effects in the Stream mix | dropped by owner | `gg-audio` | — | Built and kept: stream feeds taken before each channel's EQ. |
| S15 | Personal master bus and audition the audience mix | dropped by owner | `gg-audio` | — | Built and kept: the stream's master level. |

## Moments

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| O01 | Event-driven automatic highlights | dropped by owner | `gglinuxd/src/clips/` | — | Built and kept: games-only clips (the buffer runs while a game from the rules runs); no event detection. |
| O02 | Event markers on clip timelines | dropped by owner | — | — | Not built. |
| O03 | In-app trim editor and non-destructive edits | dropped by owner | — | — | Not built. |
| O04 | Independent multitrack audio editing | dropped by owner | `gglinuxd/src/clips/` | — | Built and kept: desktop-audio and mic toggles for clips. |
| O05 | Montage builder, text overlays and GIF effects | dropped by owner | — | — | Not built. |
| O06 | Thumbnails, in-app previews, tags, game filters and favorites | dropped by owner | `gglinuxd/src/clips/` | — | Built and kept: the clip list (name, size, time). |
| O07 | Rename/export/share workflow | dropped by owner | `gglinuxd/src/clips/` | — | Built and kept: the clip files and opening their folder. |
| O08 | Capture resolution and explicit game-vs-screen mode | dropped by owner | `gglinuxd/src/clips/` | — | Built and kept: frame rate, quality, length and audio settings. |

## GameSense

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| G01 | OLED screen handlers | built, software only | `gg-gamesense/src/screen.rs`; `ggctl gamesense test screen`; Games panel | none | Drawn through the keyboard's live frame, which passed on his keyboard (K09, K10), so no new device command and no gate. Icons aren't drawn and the text layout is gglinux's own. Not tried with a real game. |
| G02 | Full-keyboard bitmap and partial-bitmap handlers | built, software only | `gg-gamesense/src/bitmap.rs` | none | Paints every key through the GameSense lighting layer. Which key takes which cell of the SDK's 22 × 6 grid is gglinux's reading of the SDK doc, not checked against GG. |
| G03 | Discord integration and dedicated CS game-state app | not started | — | — | Not started; ask the owner. |
| G04 | Tactile and non-keyboard hardware handlers | broader scope | — | — | No haptic or other non-keyboard GameSense device here. |

## Suite

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| T01 | Support for other SteelSeries hardware | broader scope | — | — | gglinux supports three exact USB IDs; other SteelSeries devices need their own protocol work. |
| T02 | Firmware updates and recovery tools | deliberately omitted | the keyboard allowlist | — | A failed flash can brick a device, so gglinux never frames firmware-update or bootloader commands. The plan keeps updates in GG itself and a native flasher research-only until the owner says yes. |
| T03 | 3D Aim Trainer | dropped by owner | — | — | No Aim Trainer (owner, 2026-11-13). |
| T04 | Cloud Sync | not a gap | Settings → backup and restore | — | SteelSeries dropped Cloud Sync; local backup and restore exist. |

## Already built before M11

| ID | Feature | Status now | Where | Gate | Note |
|---|---|---|---|---|---|
| E01 | DPI stages, polling, simple mouse binds and onboard save | verified on his hardware | `gg-device-mouse`; `ggctl mouse`; mouse card | none (read-backs: `mouse_power`) | DPI felt faster and slower, and separate X/Y (hands-on step 7); DPI and polling rate, saved and not, read back (hardware log); a button remap through the app, and a saved DPI that came back after switching the mouse off and on (hands-on log, 15:29). |
| E02 | Actuation, Rapid Trigger and Rapid Tap | built, waiting for hands-on | `38 61/62/65/66/67`; `ggctl switches`; switches panel | none (off until turned on) | The tables went to his keyboard without error but were never checked by hand, and are on hold since the phantom key presses. Since M11 only the changed table goes, as GG sends it (capture apex-10). |
| E03 | Text, clock, static OLED picture and screen care | built, waiting for hands-on | `gglinuxd/src/keyboard/screen.rs`; `ggctl screen`; screen panel | none | The live frame they use passed with the System Monitor and an animation (hands-on step 5), and his clock has run on it daily since 2026-10-29 (hardware log); text, a still picture and screen care weren't checked on their own. |
| E04 | ANC, transparency, sidetone, mic level/LED, auto-off, radio mode and EQ | built, waiting for hands-on | `gg-device-headset`; `ggctl headset`; headset card | none | None of these writes has been tried on the headset yet. Their values are now read from the base station (H08). |
| E05 | Mixer, routing, ChatMix slider, output EQ, AutoEQ, mic chain, Smart Volume | built, waiting for hands-on | `gg-audio`; Sonar page | none | Frozen as built (owner, 2026-11-13): it stays and keeps working, nothing more is added. Never turned on live (it restarts PipeWire: ask first). |
| E06 | Instant replay, save hotkey, clip list and games-only buffer | built, waiting for hands-on | `gglinuxd/src/clips/`; Moments page | none | Frozen as built (owner, 2026-11-13). Not tried with a real recorder yet. |
| E07 | Keyboard static/wave lighting and onboard hand-back | verified on his hardware | `gg-device-keyboard`; keyboard card | none | Seen by the owner in M4a (2026-09-27, PLAN.md), before the M11 logs. |
| E08 | Base-station OLED burn-in protection | verified on his hardware | `gglinuxd/src/headset/screen.rs`; headset card | none | Seen in M2 (2026-09-27, PLAN.md): the screen blanks, stays black, and lights on real events; the re-blank ran about two hours. |
| E09 | Tray, start at login, CLI, part toggles and local backup/restore | built, software only | `gglinux-ui`, `ggctl`, `gglinuxd/src/parts.rs` | none | M10's live test on Hyprland covered the tray, a hidden start, the one-copy guard and Quit (PLAN.md); the app's Experimental switch reached the daemon and followed a change made elsewhere (hands-on step 11). Plasma, backup and restore, and `makepkg -si` not tried. |
