#!/usr/bin/env bash
set -euo pipefail

repo=$(cd "$(dirname "$0")/.." && pwd)
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Exercise the compatibility installer's real load/verification functions
# without touching /etc or starting networking services.
sed -n '/^function nm_verify_ap () {/,/^}/p; /^function nm_load_ap_file () {/,/^}/p' \
  "$repo/setup/pi/configure-ap.sh" > "$tmp/functions.sh"
source "$tmp/functions.sh"

AP_SSID='Road: Wi\Fi'
AP_IP='192.168.66.1'
read_rc=0
load_rc=0
profile=$(printf 'ap\n%s\n%s/24' "$AP_SSID" "$AP_IP")
log_progress() { echo "$1"; }
nmcli() {
  echo "$*" >> "$tmp/calls"
  if [ "$1" = con ] && [ "$2" = load ]; then
    if [ "$load_rc" -ne 0 ]; then echo 'invalid connection file' >&2; fi
    return "$load_rc"
  fi
  if [ "$read_rc" -ne 0 ]; then
    echo 'NetworkManager is not running' >&2
    return "$read_rc"
  fi
  printf '%s\n' "$profile"
}

nm_verify_ap
nm_load_ap_file /fixture/SENTRYUSB_AP.nmconnection

load_rc=1
: > "$tmp/calls"
if nm_load_ap_file /fixture/SENTRYUSB_AP.nmconnection > "$tmp/error" 2>&1; then
  echo 'FAIL: load failure was ignored' >&2; exit 1
fi
grep -q 'invalid connection file' "$tmp/error"
[ "$(wc -l < "$tmp/calls" | tr -d ' ')" = 1 ]

load_rc=0
read_rc=8
if nm_load_ap_file /fixture/SENTRYUSB_AP.nmconnection > "$tmp/error" 2>&1; then
  echo 'FAIL: unreadable profile passed verification' >&2; exit 1
fi
grep -q 'NetworkManager is not running' "$tmp/error"

read_rc=0
for profile in '' $'ap\nstale SSID\n192.168.66.1/24'; do
  if nm_load_ap_file /fixture/SENTRYUSB_AP.nmconnection > "$tmp/error" 2>&1; then
    echo 'FAIL: missing/stale profile passed verification' >&2; exit 1
  fi
  grep -q 'has not loaded the expected AP profile' "$tmp/error"
done

echo 'AP profile shell tests passed'
