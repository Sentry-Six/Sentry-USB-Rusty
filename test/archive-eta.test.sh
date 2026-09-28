#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
eval "$(awk '/^function archive_eta_sample / {keep=1} /^function archive_progress_monitor / {exit} keep {print}' "$root/run/archiveloop")"

RATE_MILLI=0
archive_eta_sample 1 3200 5 1 5 0
[[ $ETA_SECONDS == null && $ETA_STATE == estimating ]]
archive_eta_sample 60 3200 30 60 30 0
[[ $ETA_STATE == ready && $ETA_SECONDS == 1570 ]]
baseline=$ETA_SECONDS
archive_eta_sample 61 3200 35 61 35 0
[[ $ETA_SECONDS -lt $((baseline * 2)) ]]
archive_eta_sample 61 3200 95 61 95 60
[[ $ETA_SECONDS == null && $ETA_STATE == stalled ]]
archive_eta_sample 3200 3200 3600 100 120 0
[[ $ETA_SECONDS == 0 && $ETA_STATE == complete ]]

# New jobs cannot inherit the previous transfer's learned rate.
RATE_MILLI=0
archive_eta_sample 2 10 15 2 15 0
[[ $ETA_SECONDS == null && $ETA_STATE == estimating ]]
# Exercise the publishing loop with real symlink progress and a controlled clock.
# All filesystem paths stay in the fixture; no Pi state or network is touched.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/overlay" "$work/mutable" "$work/bin"
printf '1000.00 0.00\n' > "$work/uptime"
printf '#!/bin/sh\nexit 0\n' > "$work/bin/send-live-activity"
chmod +x "$work/bin/send-live-activity"
for ((i = 1; i <= 174; i++)); do
  printf '%s\n' "$i" >> "$work/list"
  ln -s "$work/source" "$work/overlay/$i"
done

eval "$(awk '/^function write_archive_checkpoint / {keep=1} /^function finish_archive_checkpoint / {keep=0} keep {print}' "$root/run/archiveloop" |
  sed "s@/mutable/@$work/mutable/@g")"
eval "$(awk '/^function archive_progress_monitor / {keep=1} /^function clean_empty_cam_directories / {exit} keep {print}' "$root/run/archiveloop" |
  sed -e "s@/proc/uptime@$work/uptime@g" -e "s@/mutable/@$work/mutable/@g" -e "s@/root/bin/@$work/bin/@g")"

# macOS ships Bash 3; production Bash supplies the same mapfile operation.
if ! type mapfile >/dev/null 2>&1; then
  mapfile() {
    [[ "$1" == -t && "$2" == pending ]]
    local entry
    while IFS= read -r entry; do pending+=("$entry"); done
  }
fi
(
  # Bash 3 treats an empty array as unset, unlike the Pi's Bash 5.
  if [ "${BASH_VERSINFO[0]}" -lt 4 ]; then set +u; fi
  step=0
  completed=0
  ARCHIVE_CYCLE_ID=fixture-cycle
  sleep() {
    step=$((step + 1))
    local elapsed target
    case "$step" in
      1) elapsed=5; target=1 ;;
      2) elapsed=30; target=99 ;;
      3) elapsed=35; target=100 ;;
      4) elapsed=95; target=100 ;;
      5) elapsed=100; target=105 ;;
      6) elapsed=130; target=140 ;;
      7) elapsed=150; target=174 ;;
      *) exit 0 ;;
    esac
    printf '%s.00 0.00\n' "$((1000 + elapsed))" > "$work/uptime"
    while [ "$completed" -lt "$target" ]; do
      completed=$((completed + 1))
      rm "$work/overlay/$completed"
    done
  }
  write_archive_status() { printf '%s\n' "$1" >> "$work/statuses"; }
  archive_progress_monitor 174 "$work/list" "$work/overlay"
)
python3 - "$work" <<'PYTEST'
import json
import pathlib
import sys

work = pathlib.Path(sys.argv[1])
samples = [json.loads(line) for line in (work / 'statuses').read_text().splitlines()]
assert [s['current'] for s in samples] == [1, 99, 100, 100, 105, 140, 174]
assert [s['eta_state'] for s in samples] == [
    'estimating', 'ready', 'ready', 'stalled', 'ready', 'ready', 'complete',
]
assert all(s['job_id'] == 'fixture-cycle' and s['total'] == 174 for s in samples)
assert all(s['sampled_at'] >= s['started_at'] for s in samples)
assert all(s['eta_seconds'] > 0 for s in samples if s['eta_state'] == 'ready')
assert all(s['eta_seconds'] is None for s in samples if s['eta_state'] in ('estimating', 'stalled'))
assert samples[-1]['eta_seconds'] == 0
checkpoint = json.loads((work / 'mutable/archive_in_progress.json').read_text())
assert checkpoint['current'] == 174 and checkpoint['total'] == 174
assert checkpoint['job_id'] == 'fixture-cycle'
PYTEST
echo 'archive ETA warmup, smoothing, stall, recovery, completion and monitor publishing passed'
