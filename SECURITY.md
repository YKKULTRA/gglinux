# Security policy

## Supported versions

gglinux has no releases yet. Security fixes go to the `master` branch; please test against it
before reporting.

## Reporting a vulnerability

Please report security problems privately, not in a public issue:

- Use GitHub's private vulnerability reporting: the **Report a vulnerability** button on the
  repository's **Security** tab.
- If that isn't available to you, open a public issue that only asks for a private contact, with
  no details of the problem.

Please include what an attacker needs (local user, another user on the same machine, a web page, a
game, ...), what they gain, and the steps to reproduce. Expect an acknowledgement within a week.
This is a one-person project, so a fix may take longer; you'll be kept informed and credited unless
you'd rather not be.

## What gglinux exposes, and how it is limited

gglinux runs entirely as your user. It is useful to know which surfaces exist when deciding whether
something is a vulnerability.

### Device access (udev)

- `60-gglinux.rules` grants the active local session access to exactly three HID control
  interfaces: Apex Pro TKL Gen 3 interface 1, Arctis Nova Pro Wireless interface 4 and Rival 3
  Wireless Gen 2 interface 3. It uses logind's `uaccess`, so the access ends with the session.
- The keyboard's typing interfaces are never opened by default, because they carry every
  keystroke. Early development versions (before 2026-09-28) opened every SteelSeries interface;
  re-running the installer replaces that rule and clears the access it left behind.
- The optional `61-gglinux-reactive.rules` does open the Apex's typing interfaces, so that
  reactive lighting can see key presses. With it installed, **any program you run can read what you
  type on that keyboard**. It is opt-in and documented as such. gglinux reads key presses only
  while reactive lighting is on and keeps them in memory only until their light fades.

### HID writes

- gglinux only opens the interfaces above, and has no request that sends arbitrary bytes to a
  device.
- **Keyboard writes are allowlisted.** Only the opcodes in `ALLOWED`
  (`crates/gg-device-keyboard/src/proto.rs`: `0x23 0x40 0x41 0x4B 0x90 0xA3 0xF5`) can be framed,
  and the allowlist is checked again at the write. The firmware-update commands that share that
  interface cannot be sent. A unit test asserts the exact allowlist.
- **Headset and mouse writes** come only from fixed packet builders with range-checked inputs,
  each covered by byte-level tests.
- No firmware updates. Settings are written to a device's onboard memory only when you explicitly
  ask (for example `ggctl mouse ... --save`).

A way to make gglinux send bytes outside these builders, or an opcode outside the keyboard
allowlist, is a security bug.

### The daemon's socket

`gglinuxd` listens on `$XDG_RUNTIME_DIR/gglinux/gglinuxd.sock`. `$XDG_RUNTIME_DIR` is private to
your user, so only your own processes can reach it. Any of your processes can control gglinux
through it, which is the same trust level as the devices' ACLs.

### GameSense server

When GameSense is on, gglinux serves the GameSense HTTP API on `127.0.0.1` (never on other
interfaces). Like SteelSeries' own server, any local process can send it game events. To keep web
pages out, it refuses any request with an `Origin` header and requires `Content-Type:
application/json` on every POST. It caps open connections at 32 and times out slow headers (5 s)
and requests (10 s). Handler definitions kept in memory have size limits.

While on, it writes a `coreProps.json` (`{"address":"127.0.0.1:<port>"}`) into each Steam Proton
prefix so Windows games can find it, and removes only files it wrote.

### Other things it runs and writes

- The daemon starts PipeWire's command-line tools (`pw-dump`, `pw-cli`, `pw-metadata`,
  `pw-record`, `wpctl`), `systemctl --user` (to restart PipeWire for Sonar) and
  `gpu-screen-recorder`; the app starts `xdg-open` and `systemctl --user`. All of them get argument
  lists, never a shell command line.
- It writes its settings to `~/.config/gglinux/`, Sonar's PipeWire drop-in to
  `~/.config/pipewire/pipewire.conf.d/`, and clips to your videos folder. A settings backup being
  restored can only write the known settings files in `~/.config/gglinux/`; paths that try to leave
  it are refused.
- The audio visualizer analyses what is playing in memory and never records it. The clip
  recorder only keeps a replay buffer while clips are on.

## Out of scope

- Anything that needs root, or another process already running as your user with the same access
  gglinux has (it can talk to the devices and the socket directly).
- Bugs in PipeWire, Slint, gpu-screen-recorder or the kernel's hidraw driver. Please report
  those upstream; a heads-up is still welcome if gglinux should work around one.
