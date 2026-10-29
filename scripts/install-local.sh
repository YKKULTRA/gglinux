#!/usr/bin/env bash
# Install gglinux for the current user: the daemon and ggctl -> ~/.local/bin, systemd --user
# unit, the UI with its desktop entry, and the udev rule that grants your desktop session access
# to SteelSeries devices.
#
# Usage: ./scripts/install-local.sh [--no-udev] [--no-build] [--no-ui] [--reactive]
#
# --no-build installs what an earlier build left, and skips ggctl or the UI when that build
# didn't make them — so a udev-only re-run on an older checkout still reaches the udev step.
#
# --no-ui skips building and installing the UI (it needs npm and webkit2gtk-4.1).
#
# --reactive also installs the optional rule that lets reactive lighting see key presses on
# the Apex Pro TKL Gen 3 — which lets any program you run read what you type on it.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
do_udev=1
do_build=1
do_ui=1
do_reactive=0
for arg in "$@"; do
  case "$arg" in
    --no-udev)  do_udev=0 ;;
    --no-build) do_build=0 ;;
    --no-ui)    do_ui=0 ;;
    --reactive) do_reactive=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

# Whether binary $1 is there to install. Only --no-build can leave it missing; then $2 is
# named as skipped instead of `install` stopping the script.
built() {
  [[ $do_build -eq 1 || -f "$1" ]] && return
  echo "==> $2 isn't built; skipping it (run without --no-build to build it)"
  return 1
}

if [[ $do_build -eq 1 ]]; then
  echo "==> building (release)"
  # Into the repo's own target/, where the steps below look, even if CARGO_TARGET_DIR or a
  # cargo config names another target dir; the UI's build does the same.
  CARGO_TARGET_DIR="$repo/target" cargo build --release --manifest-path "$repo/Cargo.toml" --bins
fi

echo "==> installing gglinuxd -> ~/.local/bin"
install -Dm755 "$repo/target/release/gglinuxd" "$HOME/.local/bin/gglinuxd"
if built "$repo/target/release/ggctl" ggctl; then
  echo "==> installing ggctl -> ~/.local/bin"
  install -Dm755 "$repo/target/release/ggctl" "$HOME/.local/bin/ggctl"
fi

echo "==> installing systemd --user unit"
install -Dm644 "$repo/data/systemd/gglinuxd.service" \
  "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/gglinuxd.service"
systemctl --user daemon-reload
# A daemon already running keeps running the binary it started from, and `enable --now` below
# leaves a running one alone, so restart it onto the new one. A stopped one stays stopped.
systemctl --user try-restart gglinuxd

if [[ $do_ui -eq 1 ]]; then
  if [[ $do_build -eq 1 ]]; then
    echo "==> building the UI (release)"
    (cd "$repo/ui" && npm ci \
      && CARGO_TARGET_DIR="$repo/ui/src-tauri/target" npm run tauri build -- --no-bundle)
  fi
  if built "$repo/ui/src-tauri/target/release/gglinux" "the UI"; then
    echo "==> installing the UI -> ~/.local/bin/gglinux"
    install -Dm755 "$repo/ui/src-tauri/target/release/gglinux" "$HOME/.local/bin/gglinux"
    # ~/.local/bin may not be on the desktop session's PATH, so the entry names the full path.
    sed "s|^Exec=.*|Exec=\"$HOME/.local/bin/gglinux\"|" "$repo/data/gglinux.desktop" \
      | install -Dm644 /dev/stdin "$HOME/.local/share/applications/gglinux.desktop"
    install -Dm644 "$repo/ui/src-tauri/icons/128x128.png" \
      "$HOME/.local/share/icons/hicolor/128x128/apps/gglinux.png"
  else
    do_ui=0  # nothing new in the app menu, so no closing note about it
  fi
fi

if [[ $do_udev -eq 1 ]]; then
  echo "==> installing udev rule (needs root)"
  sudo install -Dm644 "$repo/data/udev/60-gglinux.rules" /etc/udev/rules.d/60-gglinux.rules
  if [[ $do_reactive -eq 1 ]]; then
    echo "==> installing the optional reactive-lighting rule:"
    echo "    programs you run will be able to read what you type on the Apex"
    sudo install -Dm644 "$repo/data/udev/61-gglinux-reactive.rules" /etc/udev/rules.d/61-gglinux-reactive.rules
  fi
  sudo udevadm control --reload
  # Re-apply to already-plugged devices so you don't have to replug.
  sudo udevadm trigger --subsystem-match=hidraw --action=add
  if [[ ! -e /etc/udev/rules.d/61-gglinux-reactive.rules ]]; then
    # Earlier gglinux rules opened the Apex's typing interfaces too, and a node keeps an
    # ACL after its rule stops granting it — clear those so the old access really ends.
    for sys in /sys/class/hidraw/hidraw*; do
      props="$(udevadm info --query=property "$sys" 2>/dev/null || true)"
      if grep -qx 'ID_VENDOR_ID=1038' <<<"$props" && grep -qx 'ID_MODEL_ID=1642' <<<"$props" \
        && grep -qxE 'ID_USB_INTERFACE_NUM=0[02]' <<<"$props"; then
        sudo setfacl -b "/dev/$(basename "$sys")"
      fi
    done
  fi
fi

cat <<EOF

Done. Start it with:
    systemctl --user enable --now gglinuxd
Check it:
    systemctl --user status gglinuxd
    journalctl --user -u gglinuxd -f
EOF
if [[ $do_ui -eq 1 ]]; then
  cat <<EOF

Open gglinux from the app menu. To have it start with your desktop, the daemon included,
tick Settings → "Start gglinux when I log in".
EOF
fi
