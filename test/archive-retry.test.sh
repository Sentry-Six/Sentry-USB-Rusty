#!/bin/bash
# Exercise real loop decisions with a simulated clock/network; no Pi or sleeps.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
python3 - "$root/run/archiveloop" "$work" <<'PY'
import re, sys
from pathlib import Path
source = Path(sys.argv[1]).read_text()
names = ['home_archive_retry_pace', 'wait_after_archive_cycle', 'run_archive_cycle',
         'wait_for_archive_to_be_reachable', 'archive_clips']
functions = []
for name in names:
    match = re.search(r'^function ' + name + r'\b[^\n]*\n.*?^}', source, re.M | re.S)
    assert match, name
    functions.append(match[0].replace('/tmp/', sys.argv[2] + '/').replace('/root/bin/', sys.argv[2] + '/bin/'))
Path(sys.argv[2], 'functions').write_text('\n'.join(functions))
PY
source "$work/functions"
pipeline_source=$(declare -f archive_clips)
log() { printf '%s\n' "$*" >> "$work/log"; }
archive_is_reachable() { [[ $reachable == 1 ]]; }
travel_mode_active() { [[ $travel == 1 ]]; }
travel_mode_fast_retry_active() { [[ $fast == 1 ]]; }
travel_mode_pace() { travel_waits=$((travel_waits + 1)); }
wait_for_archive_to_be_unreachable() { departure_waits=$((departure_waits + 1)); }
archive_cancel_requested() { [[ $cancel == 1 ]]; }
archive_cancel_cleanup() { :; }
away_mode_active() { return 1; }
archive_clips() {
  attempts=$((attempts + 1))
  attempt_was_retry=$ARCHIVE_RETRY_ATTEMPT
  return "$transfer_result"
}
sleep() {
  SECONDS=$((SECONDS + $1))
  slept=$((slept + $1))
  case "$during_sleep" in
    disconnect) reachable=0 ;;
    reconnect) reachable=1 ;;
    manual) touch "$work/archive_is_reachable" ;;
    travel) travel=1 ;;
  esac
}
reset() {
  reachable=1 travel=0 fast=0 cancel=0 transfer_result=0
  departure_waits=0 travel_waits=0 attempts=0 slept=0 during_sleep=''
  ARCHIVE_CYCLE_FAILED=0 ARCHIVE_CYCLE_CANCELLED=0
  ARCHIVE_HOME_RETRY_DELAY=0 ARCHIVE_HOME_RETRY_PENDING=0
  export ARCHIVE_SYSTEM=cifs
  rm -f "$work/archive_is_reachable"
}

for backend in cifs nfs rsync rclone; do
  reset
  ARCHIVE_SYSTEM=$backend transfer_result=1
  run_archive_cycle
  [[ $ARCHIVE_CYCLE_FAILED == 1 && $attempt_was_retry == 0 ]]
  for delay in 60 120 240 300 300; do
    slept=0
    wait_after_archive_cycle
    [[ $slept == "$delay" && $departure_waits == 0 ]]
    [[ $ARCHIVE_HOME_RETRY_PENDING == 1 ]]
    run_archive_cycle
    [[ $attempt_was_retry == 1 && $ARCHIVE_CYCLE_FAILED == 1 ]]
  done
  # A later success ends the retry sequence and restores ordinary departure waiting.
  wait_after_archive_cycle
  transfer_result=0
  run_archive_cycle
  [[ $attempt_was_retry == 1 && $ARCHIVE_CYCLE_FAILED == 0 ]]
  wait_after_archive_cycle
  [[ $departure_waits == 1 && $ARCHIVE_HOME_RETRY_DELAY == 0 && $ARCHIVE_HOME_RETRY_PENDING == 0 ]]
  run_archive_cycle
  [[ $attempt_was_retry == 0 ]]
done

reset
run_archive_cycle
wait_after_archive_cycle
[[ $slept == 0 && $departure_waits == 1 ]]
ARCHIVE_SYSTEM=none ARCHIVE_CYCLE_FAILED=1
wait_after_archive_cycle
[[ $slept == 0 && $departure_waits == 2 ]]

for travel_setting in 0 1; do
  reset
  travel=$travel_setting cancel=1
  run_archive_cycle
  [[ $attempts == 0 && $ARCHIVE_CYCLE_CANCELLED == 1 && $ARCHIVE_CYCLE_FAILED == 0 ]]
  wait_after_archive_cycle
  [[ $departure_waits == 1 && $travel_waits == 0 && $slept == 0 ]]
done
for fast_setting in 0 1; do
  reset
  travel=1 fast=$fast_setting transfer_result=1
  run_archive_cycle
  wait_after_archive_cycle
  [[ $travel_waits == 1 && $departure_waits == 0 && $slept == 0 ]]
  run_archive_cycle
  [[ $attempt_was_retry == "$fast_setting" ]]
done

reset
ARCHIVE_CYCLE_FAILED=1 reachable=0 ARCHIVE_HOME_RETRY_DELAY=300
wait_after_archive_cycle
[[ $slept == 0 && $departure_waits == 0 && $ARCHIVE_HOME_RETRY_DELAY == 0 ]]
reachable=1 during_sleep=disconnect
wait_after_archive_cycle
[[ $slept == 1 && $ARCHIVE_HOME_RETRY_PENDING == 0 && $ARCHIVE_HOME_RETRY_DELAY == 0 ]]
reachable=1 during_sleep=''
run_archive_cycle
[[ $attempt_was_retry == 0 ]]

# Losing the network exactly after the cooldown must also reset retry state.
reset
ARCHIVE_CYCLE_FAILED=1 ARCHIVE_HOME_RETRY_PENDING=1 ARCHIVE_HOME_RETRY_DELAY=300
reachable=0 during_sleep=reconnect
wait_for_archive_to_be_reachable
[[ $ARCHIVE_HOME_RETRY_PENDING == 0 && $ARCHIVE_HOME_RETRY_DELAY == 0 ]]
run_archive_cycle
[[ $attempt_was_retry == 0 ]]

reset
ARCHIVE_CYCLE_FAILED=1 during_sleep=manual
wait_after_archive_cycle
[[ $slept == 1 && ! -e "$work/archive_is_reachable" ]]
[[ $ARCHIVE_HOME_RETRY_PENDING == 0 && $ARCHIVE_HOME_RETRY_DELAY == 0 && $departure_waits == 0 ]]
run_archive_cycle
[[ $attempt_was_retry == 0 ]]

reset
ARCHIVE_CYCLE_FAILED=1 during_sleep=travel
wait_after_archive_cycle
[[ $slept == 1 && $travel_waits == 1 && $ARCHIVE_HOME_RETRY_PENDING == 0 ]]

# A connect helper can mount the archive before failing on another step.
# Exercise the actual early-failure pipeline: release that partial mount,
# retain failure state and schedule a retry without touching footage.
reset
eval "$pipeline_source"
archive_run_command() { return 1; }
mkdir -p "$work/bin"
printf '#!/bin/bash\nprintf disconnected > "%s"\n' "$work/disconnected" > "$work/bin/disconnect-archive.sh"
chmod +x "$work/bin/disconnect-archive.sh"
export LOG_FILE="$work/log"
run_archive_cycle
[[ $ARCHIVE_CYCLE_FAILED == 1 && -f "$work/disconnected" ]]
wait_after_archive_cycle
[[ $slept == 60 && $ARCHIVE_HOME_RETRY_PENDING == 1 ]]

echo 'PASS: failed transfers retry with capped backoff; success, cancellation, departure, manual sync and Travel Mode retain their behavior'
