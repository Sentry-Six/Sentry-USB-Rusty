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
log_start = source.index('function log () {')
log_end = source.index('\nfunction log_errors_on_exit', log_start)

start = source.index('  # Apply after custom additions')
end = source.index('  local -r sentry_count=', start)
block = source[start:end].replace('/opt/sentryusb/sentryusb', 'filter_binary')
Path(sys.argv[2]).write_text(source[log_start:log_end] + '\nfilter_candidates() {\n' + block + '\n}\n')
assert source.index('filterfile "${sentrylist}"') < start < source.index('log "Starting recording archiving"')
# Exercise the real category selection as well as the driving-only gate.
start = source.index('  local -a savedclipsopt', source.index('function archive_teslacam_clips'))
end = source.index('  # Copy lists of previously-archived files', start)
with Path(sys.argv[2]).open('a') as script:
    script.write('enumerate_candidates() {\n' + source[start:end] + '\n}\n')
PY
source "$work/filter.sh"
sentrylist="$work/candidates"
LOG_FILE="$work/archive.log"
overlaymerged="$work/overlay"
printf '0\n' > "$work/filter-calls"
unmount_calls=0
mode=success
date() { printf 'test-time\n'; }
umount() { unmount_calls=$((unmount_calls + 1)); }
filter_binary() {
  local filter_calls
  filter_calls=$(cat "$work/filter-calls")
  printf '%s\n' "$((filter_calls + 1))" > "$work/filter-calls"
  [ "$1" = filter-archive-recent-driving ]
  [ "$2" = --root ] && [ "$3" = "$overlaymerged" ]
  [ "$4" = --input ] && [ "$5" = "$sentrylist" ]
  [ "$6" = --output ] && [ "$7" = "${sentrylist}.driving" ]
  [ "$#" = 7 ]
  if [ "$mode" = failure ]; then
    # Even an error without a trailing newline must be logged.
    printf 'ERROR synthetic filter error' >&2
    return 1
  fi
  echo 'INFO RecentClips filter: fixture progress' >&2
  printf '%s\n' selected > "$7"
}
# A mixed list must pass through byte-for-byte without consulting metadata.
cat > "$work/all-candidates" <<'EOF'
RecentClips/2026-11-06/2026-11-06_10-00-00-front.mp4
RecentClips/2026-11-06/2026-11-06_10-00-00-right_repeater.mp4
RecentClips/2026-11-06/2026-11-06_11-00-00-front.mp4
RecentClips/2026-11-06/2026-11-06_12-00-00-front.mp4
SavedClips/event/front.mp4
SentryClips/event/front.mp4
TeslaTrackMode/event/front.mp4
EOF
overlaylower="$work/index"
while IFS= read -r clip; do
  mkdir -p "$overlaylower/$(dirname "$clip")"
  # Deliberately unavailable sources: selection must not require SEI reads.
  ln -s "$work/missing-source" "$overlaylower/$clip"
done < "$work/all-candidates"
ARCHIVE_SAVEDCLIPS=true
ARCHIVE_SENTRYCLIPS=true
ARCHIVE_TRACKMODECLIPS=true
ARCHIVE_RECENTCLIPS=true
unset ARCHIVE_RECENTCLIPS_DRIVING_ONLY
enumerate_candidates
sort "$sentrylist" > "$work/enumerated"
sort "$work/all-candidates" > "$work/expected"
cmp "$work/enumerated" "$work/expected"

# Missing, empty and explicitly false all mean archive without the SEI filter.
mode=failure
for driving_setting in unset '' false; do
  if [ "$driving_setting" = unset ]; then
    unset ARCHIVE_RECENTCLIPS_DRIVING_ONLY
  else
    ARCHIVE_RECENTCLIPS_DRIVING_ONLY="$driving_setting"
  fi
  cp "$work/all-candidates" "$sentrylist"
  printf '%s\n' stale-filter-output > "${sentrylist}.driving"
  filter_candidates
  [ "$(cat "$work/filter-calls")" = 0 ]
  cmp "$sentrylist" "$work/all-candidates"
done

# Disabling RecentClips excludes them regardless of the driving-only setting.
ARCHIVE_RECENTCLIPS=false
ARCHIVE_RECENTCLIPS_DRIVING_ONLY=true
enumerate_candidates
if grep -q '^RecentClips/' "$sentrylist"; then
  echo 'RecentClips included while disabled' >&2
  exit 1
fi
sed '/^RecentClips\//d' "$work/all-candidates" | sort > "$work/expected"
sort "$sentrylist" > "$work/enumerated"
cmp "$work/enumerated" "$work/expected"

ARCHIVE_RECENTCLIPS=true
mode=success
cp "$work/all-candidates" "$sentrylist"
ARCHIVE_RECENTCLIPS_DRIVING_ONLY=true
filter_candidates
[ "$(cat "$work/filter-calls")" = 1 ]
[ "$(cat "$sentrylist")" = selected ]
filter_candidates
[ "$(cat "$work/filter-calls")" = 2 ]
grep -q '^test-time: INFO RecentClips filter: fixture progress$' "$LOG_FILE"
mode=failure
printf '%s\n' original > "$sentrylist"
if filter_candidates; then echo 'filter failure did not abort archiving' >&2; exit 1; fi
[ "$(cat "$sentrylist")" = original ]
[ "$unmount_calls" = 1 ]
grep -q '^test-time: ERROR synthetic filter error$' "$LOG_FILE"
echo 'archive driving filter runtime tests passed'
