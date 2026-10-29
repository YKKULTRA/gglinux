#!/usr/bin/env bash
# SPDX-License-Identifier: GPL-3.0-or-later
# Redraw the README's screenshots (docs/screenshots/*.png) without a display or any devices.
# Each picture is the app's `shot` example drawing one page against a fresh fake daemon
# (gglinuxd's `fakemouse` example), so the devices and values in it are made up.
#
# Usage: ./scripts/screenshots.sh [name ...]     # default: every picture below
#
# It never reaches a running gglinux: the fake daemon and the app get their own temporary
# XDG_RUNTIME_DIR and XDG_CONFIG_HOME, and no display or session bus.
#
# The pictures are aimed by SHOT_SCROLL (card:N, end:N, line:N or pixels; see
# crates/gglinux-ui/examples/shot.rs), and every run prints the cards and lines it found on
# the page, so when a page changes and a picture drifts, the right number is in the output.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
out="$repo/docs/screenshots"

# name | page | width | height | SHOT_SCROLL | SHOT_FIT | SHOT_DO
pictures=(
  "engine|engine|1180|820|||"
  # The keyboard is the Engine page's second card, after the row with the mouse and the
  # headset side by side. WASD (keys 35, 51, 52 and 53) get painted, and their own actuation
  # point.
  "keyboard|engine|1180|1780|card:2-18||keyboard.set-enabled:1,keyboard.set-effect:0,keyboard.key:35,keyboard.key:51,keyboard.key:52,keyboard.key:53,keyboard.paint-selected,switches.set-enabled:1,switches.key:35,switches.key:51,switches.key:52,switches.key:53,switches.set-own:1,switches.set-rapid:1"
  "sonar|sonar|1180|820|||"
  # The EQ editor ends the Sonar card; one band is dragged up so the curve isn't flat.
  "eq|sonar|1180|540|end:1||sonar.eq-drag:4"
  "settings|settings|1180|1200||1|"
  # Instant replay on, so the page shows its settings rather than the off state.
  "moments|moments|1180|820|||moments.enable"
)

wanted=("$@")
want() {
  [[ ${#wanted[@]} -eq 0 ]] && return
  local w
  for w in "${wanted[@]}"; do [[ $w == "$1" ]] && return; done
  return 1
}

echo "==> building the shot and fakemouse examples"
cargo build --locked --manifest-path "$repo/Cargo.toml" --example shot --example fakemouse
target="$(cargo metadata --format-version 1 --no-deps --manifest-path "$repo/Cargo.toml" \
  | sed -n 's/.*"target_directory":"\([^"]*\)".*/\1/p')"
bin="$target/debug/examples"

tmp="$(mktemp -d)"
fake=""
stop_fake() {
  if [[ -n $fake ]]; then
    kill "$fake" 2>/dev/null || true
    wait "$fake" 2>/dev/null || true
    fake=""
  fi
}
trap 'stop_fake; rm -rf "$tmp"' EXIT

# The app and the daemon see only the temporary folders: no display, no session bus, no
# desktop name (which changes the Settings page) and an empty config (no autostart entry).
quiet_env=(env -u DISPLAY -u WAYLAND_DISPLAY -u DBUS_SESSION_BUS_ADDRESS
  -u HYPRLAND_INSTANCE_SIGNATURE -u XDG_CURRENT_DESKTOP "XDG_CONFIG_HOME=$tmp/config")

mkdir -p "$out" "$tmp/config"
for row in "${pictures[@]}"; do
  IFS='|' read -r name page width height scroll fit steps <<<"$row"
  want "$name" || continue
  echo "==> $name"
  run="$tmp/run-$name"
  mkdir -p -m 700 "$run"
  "${quiet_env[@]}" "XDG_RUNTIME_DIR=$run" "$bin/fakemouse" >"$tmp/fake-$name.log" 2>&1 &
  fake=$!
  for _ in $(seq 100); do
    [[ -S $run/gglinux/gglinuxd.sock ]] && break
    sleep 0.1
  done
  if [[ ! -S $run/gglinux/gglinuxd.sock ]]; then
    echo "the fake daemon didn't start:" >&2
    tail -20 "$tmp/fake-$name.log" >&2
    exit 1
  fi
  "${quiet_env[@]}" "XDG_RUNTIME_DIR=$run" SHOT_SCROLL="$scroll" SHOT_FIT="$fit" SHOT_DO="$steps" \
    "$bin/shot" "$page" "$out/$name.png" "$width" "$height"
  stop_fake
done

echo "==> done:"
ls -l "$out"/*.png
