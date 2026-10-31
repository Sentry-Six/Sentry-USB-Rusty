#!/bin/bash
# Exercise the real sync function without changing the host clock.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
python3 - "$root/run/archiveloop" "$work/sync.sh" <<'PY'
from pathlib import Path
import sys
source = Path(sys.argv[1]).read_text()
start = source.index('function set_time () {')
end = source.index('\nfunction timesyncloop', start)
Path(sys.argv[2]).write_text(source[start:end])
PY
source "$work/sync.sh"
mode=success
save_calls=0
fallback_calls=0
log() { printf '%s\n' "$*" >> "$work/log"; }
ntpdig() { [ "$mode" = success ]; }
sntp() { fallback_calls=$((fallback_calls + 1)); [ "$mode" = fallback ]; }
sleep() { :; }
fake-hwclock() {
  [ "$1" = save ]
  save_calls=$((save_calls + 1))
  [ "$mode" != save_failure ]
}
set_time
[ "$save_calls" = 1 ]
[ "$fallback_calls" = 0 ]
mode=fallback
set_time
[ "$save_calls" = 2 ]
[ "$fallback_calls" = 1 ]
mode=failure
set_time
[ "$save_calls" = 2 ]
[ "$fallback_calls" = 6 ]
# A persistence failure must not terminate archiveloop under set -e.
ntpdig() { return 0; }
mode=save_failure
set_time
[ "$save_calls" = 3 ]
rg -q 'Failed to save synchronized time' "$work/log"
echo 'archive time sync tests passed'
