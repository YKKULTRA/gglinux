# Keyboard screen: animations and the System Monitor (M11 A4) — design

**Status:** built 2026-11-06 on branch `m11-a4-kb-screen`; not tried on the keyboard yet. **Audit items:** K09
(animated OLED GIFs), K10 (OLED System Monitor).

## What

- **Animation:** the Apex Pro TKL Gen 3's 128 × 40 screen plays up to 256 pictures, each for its own delay
  (0.1 to 10 s), looping. The app reads GIF, APNG and WebP; `ggctl screen gif <file>` reads GIF.
- **System Monitor:** one to four readings, a line each, read and drawn again once a second: CPU use, CPU
  temperature, memory, GPU use, GPU temperature, a clock.

## Protocol source

No new device command. Both draw through the screen's live frame, the one text, clock and pictures already use:
Apex Control (CC BY 4.0, commit `f7db8c5`), section **"OLED (128 × 40, 1 bpp)"**: Feature report `1F 81` + 640
bytes, SSD1306 page-major, "Use **live** (`1F 81`) for frequently-updated content (clock, stats) — it does not
touch flash". It's already on the keyboard allowlist and needs no experimental gate. The persist command
(`38 83`) stays unsent.

## Decisions

- **Sources.** Each kind of screen is a `ScreenSource` (`frame(now)`, `next_due(now)`) in
  `crates/gglinuxd/src/keyboard/screen/`: `anim::Animation`, `sysmon::Monitor`. `Now` gained a monotonic
  `instant`. Screen care stays in `ScreenState`, applied on top of every source: the pixel shift moves each
  frame, and while resting the screen is dark and the loop isn't woken for the source at all.
- **Timing.** An animation's rounds are counted from its first frame on the monotonic clock, so a late wake
  skips a frame rather than slowing the animation, and the wall clock changing doesn't make it jump.
  `next_due` is the time left in the current picture plus 2 ms; the worker sleeps until then. Two identical
  pictures in a row go out once. Measured through the fake transport, at 100 ms a picture: **30 pictures in
  3.00 s = 9.99 a second, 6446 bytes a second** (645-byte feature reports). The contract's 100 ms floor seems
  right: 10 control transfers a second is a small load, and the GIFs people use mostly run at 10–20 fps.
  Whether the keyboard keeps up, and what each transfer costs on its USB link, is unverified.
- **System Monitor readers** take their roots (`/proc`, `/sys`) as arguments; tests use made-up trees. CPU use
  comes from two `/proc/stat` samples a second apart (iowait counts as idle), memory is `MemTotal −
  MemAvailable` in GiB ("9.1/62 GB"), the CPU temperature is hwmon `coretemp` "Package id 0", else the first
  `k10temp` sensor. A reading that can't be had shows "--". The picture is cached per wall-clock second, so a
  fast lighting loop doesn't make it read files 30 times a second.
- **Layout.** Labels on the left, values right-aligned, the block centred; its size comes from the widest
  line each chosen reading usually has, so it doesn't jump as values change (two short readings are drawn
  twice the size). The 8 × 8 font has no `°`, so the monitor draws its own.
- **GPU rule** (`screen/gpu.rs`). The owner's NVIDIA card sleeps on purpose. Only the kernel's own files are
  read: each PCI device's `vendor`/`class`, then `power/runtime_status`. `nvidia-smi
  --query-gpu=utilization.gpu,temperature.gpu --format=csv,noheader,nounits` runs only when **every** NVIDIA
  display device reads `active` and is bound to the `nvidia` driver (with two cards, asking would wake the
  sleeping one). It's spawned without a shell, stopped after 1.5 s, on its own thread so the keyboard's loop
  never waits for it. Otherwise the screen shows "GPU idle". To avoid *holding* an awake card, it's asked at
  most every 2 s and never sooner than the card's `power/autosuspend_delay_ms` + 1 s after the last time.
  Under `cfg(test)` the real runner panics instead of spawning, and the tests use a fake that fails if it's
  called while the card sleeps.
- **App.** The screen panel's tabs are now Keyboard's own, Text, Clock, Picture, Animation, System monitor.
  One file chooser serves Picture and Animation; what the file holds decides the tab. Each frame is fitted
  once on reading (so a big GIF never sits in memory whole), then thresholded/dithered like a still whenever
  the slider moves. More than 256 pictures are cut, and the panel says so; delays under 0.1 s are slowed to
  0.1 s, and the panel says that too. The System Monitor tab, like the clock, sends at once; it can't turn off
  the last reading nor a fifth one on.
- **ggctl** can't use the `image` crate (not a dependency of `gglinuxd`), so it has a small GIF reader of its
  own (LZW, colour tables, transparency, disposal 2/3, interlacing) and a copy of the app's dithering. It
  scales with a plain average where the app uses a triangle filter, so a picture can differ by a pixel.

## Unverified

- Everything on the real keyboard: the frame rate it takes, whether its own menus fight a fast animation.
- Whether NVIDIA's driver honours `autosuspend_delay_ms` for its idle timer, so the 2 s / idle-time spacing
  really lets an awake card fall asleep between two questions. Check with the card awake once: its
  `runtime_status` should still go back to `suspended` while the monitor shows GPU readings.
- AMD and Intel graphics: no GPU reading ("--"). amdgpu's `gpu_busy_percent` would need the same
  awake-only rule.
- The `image` crate's APNG and animated WebP readers are used as they are; only GIF is tested here.
- A 256-frame animation is about 600 KB of JSON in every keyboard state message (the contract carries the
  frames); fine locally, worth a summary field if it ever shows.
