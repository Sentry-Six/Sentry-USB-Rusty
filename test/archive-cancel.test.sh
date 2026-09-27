#!/bin/bash
# Cancellation must stop only this transfer, skip subsequent stages, and
# re-enter the ordinary unreachable wait without poisoning the next cycle.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export ARCHIVE_CONTROL_DIR="$work"
source "$root/run/archive-control.sh"
log() { :; }

archive_cycle_begin
first=$ARCHIVE_CYCLE_ID
sleep 60 &
unrelated=$!
(
  sleep 0.2
  touch "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$first"
) &
request=$!
rc=0
archive_run_command bash -c 'echo "$BASHPID" > "$1"; sleep 60 & wait' _ "$work/child" || rc=$?
wait "$request"
[[ $rc == 125 ]] || { echo "cancel must return 125, got $rc"; exit 1; }
kill -0 "$unrelated" || { echo 'cancel killed an unrelated job'; exit 1; }
kill "$unrelated"
wait "$unrelated" 2>/dev/null || true
! kill -0 "$(cat "$work/child")" 2>/dev/null || { echo 'transfer survived cancellation'; exit 1; }

# A delayed request from an old dashboard must never cancel a later cycle.
archive_cycle_end
archive_cycle_begin
[[ $first != "$ARCHIVE_CYCLE_ID" ]]
touch "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$first"
archive_run_command bash -c 'echo completed' > "$work/result"
[[ $(cat "$work/result") == completed ]]

# Exercise the real pipeline with hardware/network boundaries replaced.
eval "$(awk '/^function archive_wait_for_processor / {keep=1} /^function slowblink / {exit} keep {print}' "$root/run/archiveloop" | sed "s@/root/bin/@$work/bin/@g")"
real_wait_source=$(declare -f archive_wait_for_processor)
mkdir "$work/bin"
for cmd in connect-archive.sh disconnect-archive.sh post-archive-process.sh send-live-activity; do
  printf '#!/bin/bash\necho %s >> "%s"\n' "$cmd" "$work/calls" > "$work/bin/$cmd"
  chmod +x "$work/bin/$cmd"
done
has_cam_disk() { return 0; }
archive_teslacam_clips() {
  touch "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$ARCHIVE_CYCLE_ID"
  return 1
}
archive_is_reachable() { return 0; }
clear_archive_status() { echo clear >> "$work/calls"; }
cloud_upload_step() { echo cloud >> "$work/calls"; }
LOG_FILE="$work/log"
CAM_CLEANUP_DEFERRED="$work/cleanup-deferred"
archive_wait_for_processor() { :; }
usb_gadget_is_active() { return 0; }
rc=0
archive_clips || rc=$?
[[ $rc == 125 ]] || { echo "pipeline treated cancellation as failure/success ($rc)"; exit 1; }
grep -q disconnect-archive.sh "$work/calls"
! grep -Eq 'post-archive-process|cloud|complete' "$work/calls" || { echo 'cancelled pipeline continued'; exit 1; }

# Cancellation during post-processing must also skip cloud and final success.
archive_cycle_end
archive_cycle_begin
: > "$work/calls"
archive_teslacam_clips() { return 0; }
travel_mode_active() { return 1; }
ensure_usb_drives_connected() { :; }
MUSIC_MOUNT="$work/music"
cat > "$work/bin/post-archive-process.sh" <<'SCRIPT'
#!/bin/bash
touch "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$ARCHIVE_CYCLE_ID"
sleep 60
SCRIPT
rc=0
archive_clips || rc=$?
[[ $rc == 125 ]]
grep -q disconnect-archive.sh "$work/calls"
! grep -Eq 'cloud|complete' "$work/calls"

# In particular, skip footage deletion/free-space cleanup, even when called
# after some transfers finished. The live footage and pending ledger survive.
eval "$(awk '/^function clean_cam_mount / {keep=1} /^# Retire snapshot-farm/ {exit} keep {print}' "$root/run/archiveloop")"
CAM_MOUNT="$work/cam"
mkdir -p "$CAM_MOUNT/TeslaCam/RecentClips"
printf 'untransferred footage' > "$CAM_MOUNT/TeslaCam/RecentClips/short.mp4"
CAM_CLEANUP_PENDING="$work/pending"
touch "$CAM_CLEANUP_PENDING"
ensure_cam_file_is_mounted() { echo 'cancelled cleanup mounted cam' >&2; exit 1; }
clean_cam_mount freespace
[[ -s "$CAM_MOUNT/TeslaCam/RecentClips/short.mp4" && -f "$CAM_CLEANUP_PENDING" ]]

# A reboot drops the active cycle but must not turn a cancelled archive's
# pending obligation into permission to delete footage before a later success.
[[ -e "$CAM_CLEANUP_DEFERRED" ]] || { echo 'cancel did not defer cleanup across boot'; exit 1; }
archive_cycle_end
clean_cam_mount freespace
clean_cam_mount boot
[[ -s "$CAM_MOUNT/TeslaCam/RecentClips/short.mp4" && -f "$CAM_CLEANUP_PENDING" ]]

eval "$(awk '/^function wait_after_archive_cycle / {keep=1} /^function wifi_cycle / {exit} keep {print}' "$root/run/archiveloop")"
wait_for_archive_to_be_unreachable() { echo waiting >> "$work/calls"; }
travel_mode_active() { return 0; }
travel_mode_pace() { echo retry >> "$work/calls"; }
ARCHIVE_CYCLE_CANCELLED=1
wait_after_archive_cycle
grep -q '^waiting$' "$work/calls"
! grep -q '^retry$' "$work/calls"
archive_cycle_end
[[ ! -e "$ARCHIVE_CONTROL_DIR/archive-cycle" ]]

# A request accepted during final teardown still chooses departure waiting.
archive_cycle_begin
ARCHIVE_CYCLE_CANCELLED=0
touch "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$ARCHIVE_CYCLE_ID"
archive_cycle_end
[[ $ARCHIVE_CYCLE_CANCELLED == 1 ]] || { echo 'late cancellation lost during cycle close'; exit 1; }

# A detached JSON export/cloud batch still owns the cycle even when the
# processor itself has finished. Do not unmount on that first status poll.
eval "$real_wait_source"
curl() {
  if [ ! -e "$work/polled" ]; then
    touch "$work/polled"
    echo '{"running":false,"archive_work_running":true}'
  else
    echo '{"running":false,"archive_work_running":false}'
  fi
}
sleep() { touch "$work/waited-for-export"; }
archive_wait_for_processor
[[ -f "$work/waited-for-export" ]] || { echo 'cleanup failed to wait for detached export'; exit 1; }
echo 'archive cancel tests passed'
