#!/bin/bash
# Focused tests for waiting on car write-idle before USB teardown and for
# reclaiming orphaned exFAT clusters during image fsck.
set -euo pipefail

script=${1:-run/archiveloop}

extract() {
  awk -v f="$1" '$0 ~ "^function " f "[ (]" {keep=1} keep {print} keep && /^}/ {exit}' "$script"
}

for fn in car_write_counter car_busy_via_ble wait_for_car_write_idle wait_for_write_gap \
  disconnect_usb_drives_from_host ensure_music_file_is_mounted copy_music_files \
  exfat_fsck_supports_rescue reclaim_exfat_lost_found cam_free_space_mb \
  cam_free_space_low connect_usb_drives_to_host clean_and_retire_short_recordings \
  retry_deferred_usb_maintenance wait_for_archive_to_be_unreachable
do
  src=$(extract "$fn")
  [ -n "$src" ] || { echo "function $fn not found in $script" >&2; exit 1; }
  eval "$src"
done
fix_errors_in_image_source=$(extract fix_errors_in_image)
eval "${fix_errors_in_image_source//\/sbin\/fsck/test_fsck}"

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
CAR_BUSY_STATUS=75
CAR_BLE_GATE_FILE="$workdir/ble-gate.txt"
SHORT_RECORDING_IGNORELIST="$workdir/ignore_files"
CAM_FREE_PROBE="$workdir/cam_free_probe.bin"
CAM_CLEANUP_PENDING="$workdir/cam-cleanup-pending"
CAM_CLEANUP_DEFERRED="$workdir/cam-cleanup-deferred"
LOG_FILE="$workdir/log"
: > "$LOG_FILE"
failures=0
fail() { echo "$*" >&2; failures=$((failures + 1)); }

logs=""
log() { logs+="$*"$'\n'; }

# --- car_write_counter ---------------------------------------------------------
car_write_counter_source=$(extract car_write_counter)
eval "${car_write_counter_source//\/proc\//$workdir/proc/}"
mkdir -p "$workdir/proc/4242"
printf 'rchar: 100\nwchar: 7000000\nsyscr: 1\nsyscw: 2\nread_bytes: 0\nwrite_bytes: 6500000\n' > "$workdir/proc/4242/io"
pgrep() { echo 4242; }
counter=$(car_write_counter) || fail "car_write_counter failed on a readable io file"
[ "$counter" = 7000000 ] || fail "car_write_counter printed '$counter', want wchar alone (7000000)"
pgrep() { :; }
if car_write_counter > /dev/null; then fail "car_write_counter succeeded without a gadget thread"; fi
unset -f pgrep

# --- car_busy_via_ble ---------------------------------------------------------
ble_gate() { # sentry shift age [sentry_stale]
  {
    echo "sentry_mode=$1"
    [ -z "${4:-}" ] || echo "sentry_mode_stale=true"
    echo "charging_state=Disconnected"
    echo "shift_state=$2"
    echo "updated=$(( $(date +%s) - $3 ))"
  } > "$CAR_BLE_GATE_FILE"
}
expect_ble() {
  local label="$1" want="$2" got=idle
  car_busy_via_ble && got=busy
  [ "$got" = "$want" ] || fail "BLE $label: $got, want $want"
}
rm -f "$CAR_BLE_GATE_FILE"; expect_ble "no telemetry" idle
ble_gate Off Park 10;       expect_ble "parked, Sentry off" idle
ble_gate Off Drive 10;      expect_ble "driving" busy
ble_gate Off Reverse 10;    expect_ble "reversing" busy
ble_gate Armed Park 10;     expect_ble "Sentry armed" busy
ble_gate Aware absent 10;   expect_ble "Sentry aware" busy
ble_gate Idle Park 10;      expect_ble "Sentry idle" idle
ble_gate Armed Park 10 yes; expect_ble "stale Sentry reading" idle
ble_gate Off Unknown 10;    expect_ble "unknown gear" idle
ble_gate Off Drive 600;     expect_ble "stale file" idle
rm -f "$CAR_BLE_GATE_FILE"

# --- wait_for_car_write_idle -------------------------------------------------
# Simulated clock: each sleep advances one tick. "steady" writes 1 MB every
# tick until writes_until; "bursts" repeats 2 writing ticks and 3 quiet ones,
# like the field log of a recording car (idle by a 5 s rule within seconds).
tick=0
usb_active=true
writes_until=0
pattern=steady
counter_fails_at=-1
unreachable_at=999999
cancel_at=-1
sleep() { tick=$((tick + 1)); }
usb_gadget_is_active() { [ "$usb_active" = true ]; }
car_write_counter() {
  [ "$counter_fails_at" -lt 0 ] || [ "$tick" -lt "$counter_fails_at" ] || return 1
  local t=$tick
  (( t < writes_until )) || t=$writes_until
  if [ "$pattern" = bursts ]
  then
    echo $(( (t / 5 * 2 + (t % 5 < 2 ? t % 5 : 2)) * 1000000 ))
  else
    echo $((t * 1000000))
  fi
}
archive_is_reachable() { [ "$tick" -lt "$unreachable_at" ]; }
retry() { "$@"; }
archive_cancel_requested() { [ "$cancel_at" -ge 0 ] && [ "$tick" -ge "$cancel_at" ]; }

reset_wait() {
  tick=0 usb_active=true writes_until=0 pattern=steady counter_fails_at=-1
  unreachable_at=999999 cancel_at=-1 logs=""
  unset CAR_IDLE_WAIT_SECS CAR_IDLE_QUIET_SECS CAR_BUSY_SEEN_AT
  rm -f "$CAR_BLE_GATE_FILE"
}

expect_wait() {
  local label="$1" want_rc="$2" want_tick="$3" want_log="${4:-}" rc=0
  wait_for_car_write_idle || rc=$?
  [ "$rc" -eq "$want_rc" ] || fail "$label: expected rc $want_rc, got $rc"
  [ "$tick" -eq "$want_tick" ] || fail "$label: expected to wait $want_tick s, waited $tick s"
  if [ -n "$want_log" ] && [[ $logs != *"$want_log"* ]]
  then
    fail "$label: missing log '$want_log' in: $logs"
  fi
}

reset_wait; usb_active=false
expect_wait "inactive gadget" 0 0
[ -z "$logs" ] || fail "inactive gadget logged: $logs"

reset_wait; CAR_IDLE_WAIT_SECS=0; writes_until=999999
expect_wait "wait disabled" 0 0

reset_wait; counter_fails_at=0
expect_wait "no mass-storage thread" 0 0

reset_wait
expect_wait "car already idle (default 90s window)" 0 90 "Car idle for 90s"

reset_wait; writes_until=30
expect_wait "car stops writing" 0 120 "Car idle for 90s (waited 120s)"

reset_wait; pattern=bursts; writes_until=999999
expect_wait "recording in bursts is not idle" 2 240 "Car still busy after 240s (longest quiet gap 3s)"
[ "${CAR_BUSY_SEEN_AT:-}" = "$SECONDS" ] || fail "busy result not remembered"

# The next disconnect in the same cycle defers without another wait.
tick=0 logs=""
expect_wait "busy just seen: no second wait" 2 0 "not waiting again"
tick=0 logs=""; CAR_BUSY_SEEN_AT=$((SECONDS - 300)); writes_until=0
expect_wait "busy memory expires" 0 90 "Car idle for 90s"
[ -z "${CAR_BUSY_SEEN_AT:-}" ] || fail "idle result kept the busy memory"

reset_wait; CAR_IDLE_QUIET_SECS=20
expect_wait "quiet window override" 0 20 "Car idle for 20s"

reset_wait; ble_gate Armed Park 0
expect_wait "BLE: Sentry armed, no writes" 2 240 "Sentry Mode is armed"
reset_wait; ble_gate Off Park 0
expect_wait "BLE: parked, Sentry off" 0 90 "Car idle for 90s"

reset_wait; writes_until=999999; unreachable_at=20
expect_wait "car leaves home while writing" 1 30 "Archive became unreachable"

reset_wait; writes_until=999999; unreachable_at=0; CAR_IDLE_WAIT_SECS=40
expect_wait "already away (boot): busy, no veto" 2 40 "Car still busy after 40s"

reset_wait; writes_until=999999; cancel_at=3
expect_wait "cancelled while waiting" 1 3 "Archive cancelled"

reset_wait; writes_until=999999; counter_fails_at=7
expect_wait "gadget goes away while waiting" 0 7 "no longer active"

# A forced disconnect still waits (up to 60 s) for a 5 s write gap, and
# ignores BLE, since Sentry stays armed.
expect_gap() {
  local label="$1" want_tick="$2" want_log="$3"
  wait_for_write_gap
  [ "$tick" -eq "$want_tick" ] || fail "write gap $label: waited $tick s, want $want_tick"
  [[ $logs == *"$want_log"* ]] || fail "write gap $label: missing '$want_log' in: $logs"
}
reset_wait; ble_gate Armed Park 0
expect_gap "armed but not writing" 5 "Forced disconnect after 5s (at a write gap)"
reset_wait; writes_until=12
expect_gap "burst ends" 17 "Forced disconnect after 17s (at a write gap)"
reset_wait; pattern=bursts; writes_until=999999
expect_gap "no gap within 60 s" 60 "Forced disconnect after 60s (no write gap found)"
reset_wait; counter_fails_at=0
wait_for_write_gap
[ "$tick" -eq 0 ] || fail "write gap waited without a gadget thread"
reset_wait

# --- disconnect policy ------------------------------------------------------
lock_calls=0
free_low=1
free_low_calls=0
gap_waits=0
with_gadget_lock() { lock_calls=$((lock_calls + 1)); }
wait_for_write_gap() { gap_waits=$((gap_waits + 1)); }
wait_for_car_write_idle() { return "$idle_rc"; }
cam_free_space_low() { free_low_calls=$((free_low_calls + 1)); return "$free_low"; }

expect_disconnect() {
  local label="$1" purpose="$2" want_rc="$3" want_locks="$4" rc=0
  lock_calls=0 free_low_calls=0 logs=""
  disconnect_usb_drives_from_host $purpose || rc=$?
  [ "$rc" -eq "$want_rc" ] || fail "$label: expected rc $want_rc, got $rc"
  [ "$lock_calls" -eq "$want_locks" ] || fail "$label: $lock_calls teardown(s), want $want_locks"
}

idle_rc=0 CAM_FREE_MB_AT=$SECONDS gap_waits=0
expect_disconnect "idle" cam-cleanup 0 1
[ "$gap_waits" -eq 0 ] || fail "idle disconnect waited for a write gap"
[ "$free_low_calls" -eq 0 ] || fail "idle disconnect measured free space"
[ -z "${CAM_FREE_MB_AT:-}" ] || fail "free-space reading kept across a teardown"
idle_rc=1
expect_disconnect "vetoed" cam-cleanup 1 0
idle_rc=2 free_low=0
expect_disconnect "busy, low space, force not allowed yet" cam-cleanup "$CAR_BUSY_STATUS" 0
[ "$free_low_calls" -eq 0 ] || fail "free space probed before a force was allowed"
[[ $logs == *'Deferring USB disconnect'* ]] || fail "busy deferral not logged"
CAM_CLEANUP_MAY_FORCE=1
idle_rc=2 free_low=1
expect_disconnect "busy, space ok: defer" cam-cleanup "$CAR_BUSY_STATUS" 0
idle_rc=2 free_low=0 gap_waits=0
expect_disconnect "busy, short of space, force allowed" cam-cleanup 0 1
[[ $logs == *'WARNING: disconnecting USB while the car is busy'* ]] || fail "forced disconnect not logged"
[ "$gap_waits" -eq 1 ] || fail "forced disconnect did not wait for a write gap"
idle_rc=2 free_low=0
expect_disconnect "busy music sync is never forced" "" "$CAR_BUSY_STATUS" 0
[ "$free_low_calls" -eq 0 ] || fail "music disconnect measured free space"
unset CAM_CLEANUP_MAY_FORCE

# Startup connect leaves a presented gadget alone while the car is busy.
usb_active=true idle_rc=2 lock_calls=0
connect_usb_drives_to_host || fail "busy startup connect failed"
[ "$lock_calls" -eq 0 ] || fail "busy startup connect cycled a presented gadget"
usb_active=false idle_rc=0 lock_calls=0
connect_usb_drives_to_host
[ "$lock_calls" -eq 1 ] || fail "startup connect skipped an inactive gadget"
usb_active=true idle_rc=0 lock_calls=0
connect_usb_drives_to_host
[ "$lock_calls" -eq 1 ] || fail "idle startup connect skipped the cycle"

# A deferred music sync reports the deferral, not a failure.
mount_calls=0 copy_calls=0
ensure_mountpoint_is_mounted_with_retry() { mount_calls=$((mount_calls + 1)); }
archive_run_command() { copy_calls=$((copy_calls + 1)); }
disconnect_usb_drives_from_host() { return "$CAR_BUSY_STATUS"; }
rc=0; copy_music_files || rc=$?
[ "$rc" -eq "$CAR_BUSY_STATUS" ] || fail "deferred music sync returned $rc"
[ "$mount_calls" -eq 0 ] || fail "music image mounted while gadget may be bound"
[ "$copy_calls" -eq 0 ] || fail "music copied while gadget may be bound"
grep -q 'Music sync deferred' "$script" || fail "archive_clips does not report a music deferral"

# --- clean_cam_mount records a busy deferral -------------------------------
clean_cam_mount_source=$(extract clean_cam_mount)
eval "${clean_cam_mount_source//\/tmp\/last_clean_time/$workdir/last_clean_time}"
eval "$(extract ensure_cam_file_is_mounted)"
CAM_MOUNT="$workdir/cam"
pending_marks=0
mark_cam_cleanup_pending() { pending_marks=$((pending_marks + 1)); }
for mode in freespace boot
do
  rm -f "$workdir/last_clean_time"
  pending_marks=0 rc=0
  clean_cam_mount "$mode" || rc=$?
  [ "$rc" -eq "$CAR_BUSY_STATUS" ] || fail "busy $mode cleanup returned $rc"
  want=0; [ "$mode" = freespace ] && want=1
  [ "$pending_marks" -eq "$want" ] || fail "busy $mode cleanup marked pending $pending_marks time(s), want $want"
done

# --- short recording links are retired only after the cleanup ran ----------
clean_rc=0 retire_calls=0 retire_args=""
clean_cam_mount() { return "$clean_rc"; }
retire_short_recording_links() { retire_calls=$((retire_calls + 1)); retire_args="$*"; }
ARCHIVE_ALL_STAGES_OK=1 clean_rc=0
clean_and_retire_short_recordings /tmp/list /index
[ "$retire_calls" -eq 1 ] && [ "$retire_args" = "/tmp/list /index" ] || fail "cleaned shorts not retired ($retire_calls: $retire_args)"
[ "$ARCHIVE_ALL_STAGES_OK" = 1 ] || fail "successful cleanup flagged a failed stage"
retire_calls=0 clean_rc=$CAR_BUSY_STATUS
clean_and_retire_short_recordings /tmp/list /index
[ "$retire_calls" -eq 0 ] || fail "shorts retired although the cleanup was deferred"
[ "$ARCHIVE_ALL_STAGES_OK" = 0 ] || fail "deferred cleanup did not hold back auto-update"

# --- at-home retry ----------------------------------------------------------
clean_calls=0 ensure_calls=0 travel=false media_dirty=false clean_rc=0 seen_force=""
travel_mode_active() { [ "$travel" = true ]; }
has_cam_disk() { return 0; }
clean_cam_mount() {
  clean_calls=$((clean_calls + 1)); seen_force=${CAM_CLEANUP_MAY_FORCE:-unset}
  [ "$clean_rc" -ne 0 ] || usb_active=false
  return "$clean_rc"
}
ensure_usb_drives_connected() { ensure_calls=$((ensure_calls + 1)); usb_active=true; }
cam_user_media_may_need_sync() { [ "$media_dirty" = true ]; }

expect_retry() {
  local label="$1" at_home_for="$2" want_clean="$3" want_ensure="$4" want_retire="$5"
  clean_calls=0 ensure_calls=0 retire_calls=0
  retry_deferred_usb_maintenance "$at_home_for"
  [ "$clean_calls" -eq "$want_clean" ] || fail "$label: $clean_calls cleanup(s), want $want_clean"
  [ "$ensure_calls" -eq "$want_ensure" ] || fail "$label: $ensure_calls reconnect(s), want $want_ensure"
  [ "$retire_calls" -eq "$want_retire" ] || fail "$label: $retire_calls retirement(s), want $want_retire"
}

rm -f "$CAM_CLEANUP_PENDING" "$CAM_CLEANUP_DEFERRED"; usb_active=true
expect_retry "nothing pending" 300 0 0 0
touch "$CAM_CLEANUP_PENDING"
expect_retry "pending cleanup, then retire and reconnect" 300 1 1 1
[ "$retire_args" = "$SHORT_RECORDING_IGNORELIST" ] || fail "retry retired from '$retire_args'"
[ "$seen_force" = 0 ] || fail "force allowed after 300s at home"
clean_rc=$CAR_BUSY_STATUS usb_active=true
expect_retry "cleanup deferred again" 300 1 0 0
expect_retry "force allowed after 15 minutes at home" 900 1 0 0
[ "$seen_force" = 1 ] || fail "force not allowed after 900s at home"
CAR_BUSY_FORCE_AFTER_SECS=3600
expect_retry "force delay override" 900 1 0 0
[ "$seen_force" = 0 ] || fail "CAR_BUSY_FORCE_AFTER_SECS ignored"
unset CAR_BUSY_FORCE_AFTER_SECS; clean_rc=0
touch "$CAM_CLEANUP_DEFERRED"
expect_retry "user-cancel deferral wins" 300 0 0 0
rm -f "$CAM_CLEANUP_DEFERRED"; travel=true
expect_retry "travel mode never cycles" 300 0 0 0
travel=false; rm -f "$CAM_CLEANUP_PENDING"; media_dirty=true
expect_retry "deferred media sync" 300 0 1 0
media_dirty=false; usb_active=false
expect_retry "inactive gadget restored" 300 0 1 0

# The home wait runs the retry every CAR_IDLE_RECHECK_SECS and reports how
# long the car has been home. Each simulated sleep advances 100s; the car
# leaves after 10 polls.
polls=0 retry_ages=""
retry() { "$@"; }
archive_is_reachable() { polls=$((polls + 1)); [ "$polls" -le 10 ]; }
retry_deferred_usb_maintenance() { retry_ages+="$1 "; }
sleep() { SECONDS=$((SECONDS + 100)); }
CAR_IDLE_RECHECK_SECS=300
wait_for_archive_to_be_unreachable
[ "$retry_ages" = "300 600 900 " ] || fail "home wait retried at '$retry_ages', want '300 600 900 '"

# --- cam_free_space_mb ------------------------------------------------------
probe_cp_rc=0 probe_mount_rc=0 stat_out="5000000 4096"
cp() { [ "$probe_cp_rc" -eq 0 ] || return "$probe_cp_rc"; : > "${@: -1}"; echo "${@: -1}" > "$workdir/probe-target"; }
mount() { [ "$probe_mount_rc" -eq 0 ] || return "$probe_mount_rc"; echo "$*" > "$workdir/probe-mount"; }
umount() { :; }
stat() { echo "$stat_out"; }
mountoptsforimage_stub() { echo "exfat umask=000,offset=1048576,time_offset=-420"; }
eval "$(extract cam_free_space_mb | sed 's#/root/bin/mountoptsforimage#mountoptsforimage_stub#')"

mb=$(cam_free_space_mb) || fail "free space probe failed"
[ "$mb" = 20480 ] || fail "free space probe reported '$mb' MB, want 20480"
[ "$(cat "$workdir/probe-target")" = "$CAM_FREE_PROBE" ] || fail "probe copied to the wrong file"
[[ $(cat "$workdir/probe-mount") == "-t exfat -o ro,umask=000,offset=1048576,time_offset=-420 $CAM_FREE_PROBE "* ]] ||
  fail "probe not mounted read-only: $(cat "$workdir/probe-mount")"
[ ! -e "$CAM_FREE_PROBE" ] || fail "probe copy left behind"
probe_cp_rc=1
if cam_free_space_mb > /dev/null; then fail "probe without reflink support reported a value"; fi
probe_cp_rc=0 probe_mount_rc=32
if cam_free_space_mb > /dev/null; then fail "unmountable probe reported a value"; fi
[ ! -e "$CAM_FREE_PROBE" ] || fail "probe copy left behind after mount failure"
unset -f cp mount umount stat

eval "$(extract cam_free_space_low)"
free_mb=25000
cam_free_space_mb() { [ "$free_mb" != fail ] && echo "$free_mb"; }
expect_low() {
  local label="$1" want="$2" got=no
  unset CAM_FREE_MB_AT
  cam_free_space_low && got=yes
  [ "$got" = "$want" ] || fail "$label: low=$got, want $want"
}
expect_low "25 GB free" no
free_mb=19999; expect_low "just under the 20 GB target" yes
CAM_FORCE_CLEANUP_FREE_GB=30 free_mb=25000; expect_low "threshold override" yes
unset CAM_FORCE_CLEANUP_FREE_GB
free_mb=fail logs=""; expect_low "unmeasurable" no
[[ $logs == *'could not measure'* ]] || fail "unmeasurable free space not logged"

# The probe result is reused for CAM_FREE_PROBE_INTERVAL.
cam_free_space_mb() { echo probe >> "$workdir/probes"; echo 25000; }
: > "$workdir/probes"
unset CAM_FREE_MB_AT
cam_free_space_low || true
cam_free_space_low || true
[ "$(wc -l < "$workdir/probes")" -eq 1 ] || fail "probe ran $(wc -l < "$workdir/probes") times within its interval"
CAM_FREE_MB_AT=$((SECONDS - 1801))
cam_free_space_low || true
[ "$(wc -l < "$workdir/probes")" -eq 2 ] || fail "expired probe result was reused"

# --- fix_errors_in_image ----------------------------------------------------
fstype=exfat
rescue_help='  -s | --rescue        Assign orphaned clusters to files'
fsck_rc=0
reclaim_calls=0
losetup_find_show() { echo /dev/loop9; }
losetup() { :; }
blkid() { echo "$fstype"; }
fsck.exfat() { echo "$rescue_help"; }
timestamp() { cat; }
test_fsck() { echo "$*" > "$workdir/fsck-args"; echo "fsck output"; return "$fsck_rc"; }
reclaim_exfat_lost_found() { reclaim_calls=$((reclaim_calls + 1)); }

run_fsck_case() {
  local label="$1" want_args="$2" want_reclaims="$3"
  reclaim_calls=0
  rm -f "$workdir/fsck-args"
  fix_errors_in_image /backingfiles/cam_disk.bin
  [ "$(cat "$workdir/fsck-args")" = "$want_args" ] ||
    fail "$label: fsck args '$(cat "$workdir/fsck-args")', want '$want_args'"
  [ "$reclaim_calls" -eq "$want_reclaims" ] ||
    fail "$label: $reclaim_calls reclaim(s), want $want_reclaims"
}

# fsck.exfat -s exits 1 even on a clean volume, so exFAT always reclaims.
fstype=exfat fsck_rc=1
run_fsck_case "exfat" "/dev/loop9p1 -- -p -s" 1
fstype=exfat fsck_rc=0
run_fsck_case "exfat, fsck exit 0" "/dev/loop9p1 -- -p -s" 1
fstype=vfat fsck_rc=1
run_fsck_case "vfat unchanged" "/dev/loop9p1 -- -p" 0
fstype=exfat fsck_rc=1 rescue_help='  -y | --repair-yes'
run_fsck_case "exfatprogs without --rescue" "/dev/loop9p1 -- -p" 0
grep -q 'fsck output' "$LOG_FILE" || fail "fsck output not logged"

# --- reclaim_exfat_lost_found ----------------------------------------------
eval "$(extract reclaim_exfat_lost_found)"
mounted_src=""
mount_rc=0
umount_calls=0
findmnt() { [ "$1 $2 $3" = "-n -S $mounted_src" ]; }
mount() {
  [ "$mount_rc" -eq 0 ] || return "$mount_rc"
  last_mnt="$4"
  case "$populate" in
    files) mkdir -p "$4/LOST+FOUND"; head -c 2097152 /dev/zero > "$4/LOST+FOUND/FILE0000000.CHK" ;;
    empty) mkdir -p "$4/LOST+FOUND" ;;
  esac
}
umount() {
  umount_calls=$((umount_calls + 1))
  [ ! -e "$1/LOST+FOUND" ] || fail "LOST+FOUND still present at unmount"
}

logs="" populate=files
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin
[[ $logs =~ Reclaimed\ [0-9]+\ MB\ of\ orphaned\ clusters\ \(1\ rescued\ file\(s\)\)\ on\ /backingfiles/cam_disk.bin ]] ||
  fail "reclaim not logged: $logs"
[ ! -e "$last_mnt" ] || fail "reclaim left its mount point behind"
[ "$umount_calls" -eq 1 ] || fail "reclaim did not unmount"

# exfatprogs 1.2.0 leaves an empty LOST+FOUND after every -s run
logs="" populate=empty umount_calls=0
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin
[ -z "$logs" ] || fail "empty LOST+FOUND was logged: $logs"
[ "$umount_calls" -eq 1 ] || fail "empty reclaim did not unmount"

logs="" populate=none umount_calls=0
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin
[ -z "$logs" ] || fail "nothing to reclaim but logged: $logs"

logs="" mount_rc=32 umount_calls=0
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin
[[ $logs == *"could not mount"* ]] || fail "mount failure not logged"
[ "$umount_calls" -eq 0 ] || fail "unmounted after failed mount"

logs="" mount_rc=0 mounted_src=/dev/loop9p1
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin
[[ $logs == *"is mounted; not reclaiming"* ]] || fail "mounted partition was not skipped"

[ "$failures" -eq 0 ] || exit 1
echo "archiveloop car-idle/exFAT tests passed"
