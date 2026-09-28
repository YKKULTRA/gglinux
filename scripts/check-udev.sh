#!/usr/bin/env bash
# Check which SteelSeries hidraw nodes the repo's udev rules open to the desktop user,
# without installing them: `udevadm test` loads the repo copies from a scratch directory,
# where a file of the same name shadows any installed copy. CURRENT_TAGS is what this run
# tags; TAGS also carries sticky tags from earlier events, so it is not used.
#
# Needs the devices plugged in; needs no root.
#
# Usage: ./scripts/check-udev.sh [--reactive]
set -euo pipefail

repo="$(cd "$(dirname "${BASH_SOURCE[0]}")/.." && pwd)"
reactive=0
for arg in "$@"; do
  case "$arg" in
    --reactive) reactive=1 ;;
    *) echo "unknown option: $arg" >&2; exit 2 ;;
  esac
done

dir="$(mktemp -d)"
trap 'rm -rf "$dir"' EXIT
cp "$repo/data/udev/60-gglinux.rules" "$dir/"
if [[ $reactive -eq 1 ]]; then
  cp "$repo/data/udev/61-gglinux-reactive.rules" "$dir/"
else
  printf '# shadows an installed copy: checking the default rule alone\n' >"$dir/61-gglinux-reactive.rules"
fi

# Whether gglinux should open this product:interface to the desktop user.
expected() {
  case "$1:$2" in
    1642:01 | 12e0:04 | 1872:03) echo yes ;;
    1642:00 | 1642:02) if [[ $reactive -eq 1 ]]; then echo yes; else echo no; fi ;;
    *) echo no ;;
  esac
}

prop() { sed -n "s/^$1=//p" <<<"$2"; }

seen=0
fail=0
for sys in /sys/class/hidraw/hidraw*; do
  props="$(udevadm info --query=property "$sys" 2>/dev/null || true)"
  [[ "$(prop ID_VENDOR_ID "$props")" == 1038 ]] || continue
  pid="$(prop ID_MODEL_ID "$props")"
  ifn="$(prop ID_USB_INTERFACE_NUM "$props")"
  seen=$((seen + 1))
  tags="$(udevadm test -D "$dir" --action=add "$sys" 2>/dev/null | sed -n 's/^ *CURRENT_TAGS=//p')"
  got=no
  [[ "$tags" == *:uaccess:* ]] && got=yes
  want="$(expected "$pid" "$ifn")"
  mark=ok
  [[ "$got" == "$want" ]] || { mark=WRONG; fail=1; }
  printf '%-9s 1038:%s iface %s  opened=%-3s want=%-3s %s\n' "$(basename "$sys")" "$pid" "$ifn" "$got" "$want" "$mark"
done

if [[ $seen -eq 0 ]]; then
  echo "no SteelSeries hidraw nodes found — plug a device in" >&2
  exit 2
fi
exit $fail
