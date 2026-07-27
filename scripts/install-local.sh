#!/usr/bin/env bash
# Install gglinux for the current user: binary -> ~/.local/bin, systemd --user unit,
# and the udev rule that grants your desktop session access to SteelSeries devices.
#
# Usage: ./scripts/install-local.sh [--no-udev] [--no-build]
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
do_udev=1
do_build=1
for arg in "$@"; do
  case "$arg" in
    --no-udev)  do_udev=0 ;;
    --no-build) do_build=0 ;;
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
  sudo udevadm control --reload
  # Re-apply to already-plugged devices so you don't have to replug.
  sudo udevadm trigger --subsystem-match=hidraw --action=add
fi

cat <<EOF

Done. Start it with:
    systemctl --user enable --now gglinuxd
Check it:
    systemctl --user status gglinuxd
    journalctl --user -u gglinuxd -f
EOF
