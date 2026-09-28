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
echo 'archive ETA warmup, smoothing, stall, completion and reset passed'
