#!/usr/bin/env bash
# Install gglinux for the current user: binary -> ~/.local/bin, systemd --user unit,
# and the udev rule that grants your desktop session access to SteelSeries devices.
#
# Usage: ./scripts/install-local.sh [--no-udev] [--no-build] [--reactive]
#
# --reactive also installs the optional rule that lets reactive lighting see key presses on
# the Apex Pro TKL Gen 3 — which lets any program you run read what you type on it.
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
do_udev=1
do_build=1
do_reactive=0
for arg in "$@"; do
  case "$arg" in
    --no-udev)  do_udev=0 ;;
    --no-build) do_build=0 ;;
    --reactive) do_reactive=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

if [[ $do_build -eq 1 ]]; then
  echo "==> building (release)"
  cargo build --release --manifest-path "$repo/Cargo.toml" --bins
fi

echo "==> installing gglinuxd -> ~/.local/bin"
install -Dm755 "$repo/target/release/gglinuxd" "$HOME/.local/bin/gglinuxd"

echo "==> installing systemd --user unit"
install -Dm644 "$repo/data/systemd/gglinuxd.service" \
  "${XDG_CONFIG_HOME:-$HOME/.config}/systemd/user/gglinuxd.service"
systemctl --user daemon-reload

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
