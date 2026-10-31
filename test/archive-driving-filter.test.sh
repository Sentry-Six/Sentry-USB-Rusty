#!/bin/bash
# Check runtime wiring without mounting overlays or contacting an archive.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
python3 - "$root/run/archiveloop" "$work/filter.sh" <<'PY'
from pathlib import Path
import sys
source = Path(sys.argv[1]).read_text()
start = source.index('  # Apply after custom additions')
end = source.index('  local -r sentry_count=', start)
block = source[start:end].replace('/opt/sentryusb/sentryusb', 'filter_binary')
Path(sys.argv[2]).write_text('filter_candidates() {\n' + block + '\n}\n')
assert source.index('filterfile "${sentrylist}"') < start < source.index('log "Starting recording archiving"')
PY
source "$work/filter.sh"
sentrylist="$work/candidates"
overlaymerged="$work/overlay"
filter_calls=0
unmount_calls=0
mode=success
log() { :; }
umount() { unmount_calls=$((unmount_calls + 1)); }
filter_binary() {
  filter_calls=$((filter_calls + 1))
  [ "$1" = filter-archive-recent-driving ]
  [ "$2" = --root ] && [ "$3" = "$overlaymerged" ]
  [ "$4" = --input ] && [ "$5" = "$sentrylist" ]
  [ "$6" = --output ] && [ "$7" = "${sentrylist}.driving" ]
  [ "$8" = --min-speed-mps ] && [ "$9" = "${ARCHIVE_RECENTCLIPS_MIN_SPEED_MPS-0.5}" ]
  [ "$mode" != failure ] || return 1
  printf '%s\n' selected > "$7"
}
printf '%s\n' original > "$sentrylist"
filter_candidates
[ "$filter_calls" = 0 ]
[ "$(cat "$sentrylist")" = original ]
ARCHIVE_RECENTCLIPS_DRIVING_ONLY=true
filter_candidates
[ "$filter_calls" = 1 ]
[ "$(cat "$sentrylist")" = selected ]
ARCHIVE_RECENTCLIPS_MIN_SPEED_MPS=2.5
filter_candidates
[ "$filter_calls" = 2 ]
mode=failure
printf '%s\n' original > "$sentrylist"
if filter_candidates; then echo 'filter failure did not abort archiving' >&2; exit 1; fi
[ "$(cat "$sentrylist")" = original ]
[ "$unmount_calls" = 1 ]
echo 'archive driving filter runtime tests passed'
