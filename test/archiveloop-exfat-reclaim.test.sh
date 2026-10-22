#!/bin/bash
# Focused tests for reclaiming orphaned exFAT clusters during image fsck.
set -euo pipefail

script=${1:-run/archiveloop}

extract() {
  awk -v f="$1" '$0 ~ "^function " f "[ (]" {keep=1} keep {print} keep && /^}/ {exit}' "$script"
}

for fn in exfat_fsck_supports_rescue release_exfat_reclaim_mount reclaim_exfat_lost_found \
  fix_errors_in_images ensure_music_file_is_mounted copy_music_files
do
  src=$(extract "$fn")
  [ -n "$src" ] || { echo "function $fn not found in $script" >&2; exit 1; }
  eval "$src"
done
fix_errors_in_image_source=$(extract fix_errors_in_image)
eval "${fix_errors_in_image_source//\/sbin\/fsck/test_fsck}"

workdir=$(mktemp -d)
trap 'rm -rf "$workdir"' EXIT
LOG_FILE="$workdir/log"
: > "$LOG_FILE"
EXFAT_RECLAIM_MOUNT="$workdir/exfat-reclaim"
EXFAT_RECLAIM_STAMP="$workdir/exfat-reclaimed"
IMAGE_STILL_MOUNTED=2
failures=0
fail() { echo "$*" >&2; failures=$((failures + 1)); }

logs=""
log() { logs+="$*"$'\n'; }
sleep() { :; }

# Mount table stubs: "leftover" = something mounted at the reclaim path.
leftover=false umount_rc=0 umount_calls=0 umount_args="" mount_calls=0 mount_rc=0 populate=none last_mnt="" mounted_src=""
mountpoint() { [ "$2" = "$EXFAT_RECLAIM_MOUNT" ] && [ "$leftover" = true ]; }
findmnt() { [ "$1 $2 $3" = "-n -S $mounted_src" ]; }
mount() {
  mount_calls=$((mount_calls + 1))
  [ "$mount_rc" -eq 0 ] || return "$mount_rc"
  last_mnt="${*: -1}"
  leftover=true
  case "$populate" in
    files) mkdir -p "$last_mnt/LOST+FOUND"; head -c 2097152 /dev/zero > "$last_mnt/LOST+FOUND/FILE0000000.CHK" ;;
    empty) mkdir -p "$last_mnt/LOST+FOUND" ;;
  esac
}
umount() {
  umount_calls=$((umount_calls + 1))
  umount_args+="$* "
  [ "$umount_rc" -eq 0 ] || return "$umount_rc"
  [ ! -e "${*: -1}/LOST+FOUND" ] || fail "LOST+FOUND still present at unmount"
  leftover=false
}
reset_mounts() { logs="" leftover=false umount_rc=0 umount_calls=0 umount_args="" mount_calls=0 mount_rc=0 populate=none last_mnt="" mounted_src=""; rm -f "$EXFAT_RECLAIM_STAMP"; }

# --- release_exfat_reclaim_mount ----------------------------------------------
reset_mounts
release_exfat_reclaim_mount "leftover found" || fail "release failed with nothing mounted"
[ "$umount_calls" -eq 0 ] && [ -z "$logs" ] || fail "release acted with nothing mounted"

reset_mounts; leftover=true
release_exfat_reclaim_mount "leftover found" || fail "release of a leftover failed"
[[ $logs == *"leftover found"* ]] || fail "leftover release not logged"

# A busy unmount is retried, never done lazily, and reported as a failure.
reset_mounts; leftover=true umount_rc=32
if release_exfat_reclaim_mount; then fail "busy mount reported as released"; fi
[ "$umount_calls" -eq 5 ] || fail "busy unmount tried $umount_calls times, want 5"
[[ $umount_args != *"-l"* ]] || fail "busy mount was unmounted lazily"
[[ $logs == *"is still mounted"* ]] || fail "stuck mount not reported"

# --- fix_errors_in_image ----------------------------------------------------
fstype=exfat
rescue_help='  -s | --rescue        Assign orphaned clusters to files'
fsck_rc=0 reclaim_rc=0 reclaim_calls=0 losetup_calls=0
losetup_find_show() { losetup_calls=$((losetup_calls + 1)); echo /dev/loop9; }
losetup() { :; }
blkid() { echo "$fstype"; }
fsck.exfat() { echo help >> "$workdir/help-calls"; echo "$rescue_help"; }
timestamp() { cat; }
test_fsck() { echo "$*" > "$workdir/fsck-args"; echo "fsck output"; return "$fsck_rc"; }
real_reclaim_source=$(declare -f reclaim_exfat_lost_found)
reclaim_exfat_lost_found() { reclaim_calls=$((reclaim_calls + 1)); return "$reclaim_rc"; }

run_fsck_case() {
  local label="$1" want_args="$2" want_reclaims="$3" want_rc="${4:-0}" image="${5:-/backingfiles/cam_disk.bin}" rc=0
  reclaim_calls=0
  unset EXFAT_FSCK_RESCUE
  rm -f "$workdir/fsck-args"
  fix_errors_in_image "$image" || rc=$?
  [ "$(cat "$workdir/fsck-args" 2>/dev/null)" = "$want_args" ] ||
    fail "$label: fsck args '$(cat "$workdir/fsck-args" 2>/dev/null)', want '$want_args'"
  [ "$reclaim_calls" -eq "$want_reclaims" ] ||
    fail "$label: $reclaim_calls reclaim(s), want $want_reclaims"
  [ "$rc" -eq "$want_rc" ] || fail "$label: returned $rc, want $want_rc"
}

# fsck.exfat -s exits 1 even on a clean volume, so exFAT always reclaims.
reset_mounts
fstype=exfat fsck_rc=1
run_fsck_case "exfat" "/dev/loop9p1 -- -p -s" 1
fstype=exfat fsck_rc=0
run_fsck_case "exfat, fsck exit 0" "/dev/loop9p1 -- -p -s" 1
fstype=vfat fsck_rc=1
run_fsck_case "vfat unchanged" "/dev/loop9p1 -- -p" 0
fstype=exfat fsck_rc=1 rescue_help='  -y | --repair-yes'
run_fsck_case "exfatprogs without --rescue" "/dev/loop9p1 -- -p" 0
grep -q 'fsck output' "$LOG_FILE" || fail "fsck output not logged"

# Only the cam drive is reclaimed; the car only reads the other drives.
rescue_help='  -s | --rescue        Assign orphaned clusters to files'
fstype=exfat fsck_rc=1
for img in music boombox lightshow wraps
do
  run_fsck_case "exfat $img drive" "/dev/loop9p1 -- -p" 0 0 "/backingfiles/${img}_disk.bin"
done

# Once reclaimed in a period without the car, later passes skip it.
touch "$EXFAT_RECLAIM_STAMP"
run_fsck_case "cam already reclaimed this period" "/dev/loop9p1 -- -p" 0
rm -f "$EXFAT_RECLAIM_STAMP"

# A reclaim that cannot release its mount fails the image as still mounted.
rescue_help='  -s | --rescue        Assign orphaned clusters to files'
fstype=exfat reclaim_rc=1
run_fsck_case "reclaim left the image mounted" "/dev/loop9p1 -- -p -s" 1 "$IMAGE_STILL_MOUNTED"
reclaim_rc=0

# No fsck at all while an interrupted reclaim's mount cannot be released.
reset_mounts; leftover=true umount_rc=32 losetup_calls=0
run_fsck_case "leftover mount stuck" "" 0 "$IMAGE_STILL_MOUNTED"
[ "$losetup_calls" -eq 0 ] || fail "image attached while a reclaim mount was stuck"
[[ $logs == *"not checking /backingfiles/cam_disk.bin"* ]] || fail "skipped fsck not logged"

# A releasable leftover is released, then the fsck runs normally.
reset_mounts; leftover=true
run_fsck_case "leftover mount released" "/dev/loop9p1 -- -p -s" 1
[[ $logs == *"left by an interrupted reclaim"* ]] || fail "leftover release before fsck not logged"

# The --rescue check runs once per process, not once per image.
reset_mounts
unset EXFAT_FSCK_RESCUE; : > "$workdir/help-calls"
for img in cam music boombox; do fix_errors_in_image "/backingfiles/${img}_disk.bin"; done
[ "$(wc -l < "$workdir/help-calls")" -eq 1 ] || fail "fsck.exfat --help ran $(wc -l < "$workdir/help-calls") times for 3 images"

# --- fix_errors_in_images -----------------------------------------------------
checked=""
image_rc_for() { case "$1" in *music*) echo "$music_rc" ;; *) echo 0 ;; esac; }
fix_errors_in_image() { checked+="${1##*/} "; return "$(image_rc_for "$1")"; }
images_dir="$workdir/backingfiles"; mkdir -p "$images_dir"
for i in cam music boombox; do : > "$images_dir/${i}_disk.bin"; done
fix_errors_in_images_source=$(extract fix_errors_in_images)
eval "${fix_errors_in_images_source//\/backingfiles/$images_dir}"

checked="" music_rc=1
fix_errors_in_images || fail "an ordinary fsck problem failed the image set"
[ "$checked" = "cam_disk.bin music_disk.bin boombox_disk.bin " ] || fail "images checked: '$checked'"
checked="" music_rc=$IMAGE_STILL_MOUNTED rc=0
fix_errors_in_images || rc=$?
[ "$rc" -eq "$IMAGE_STILL_MOUNTED" ] || fail "still-mounted image returned $rc"
[ "$checked" = "cam_disk.bin music_disk.bin " ] || fail "kept checking after a still-mounted image: '$checked'"

# --- callers fail closed ----------------------------------------------------
fix_errors_in_images() { return "$images_rc"; }
gadget_calls="" disable_rc=0
test_disable_gadget() { gadget_calls+="disable "; return "$disable_rc"; }
test_enable_gadget() {
  gadget_calls+="enable "
  [ ! -e "$EXFAT_RECLAIM_STAMP" ] || fail "drive presented while the reclaim stamp still claims a period without the car"
}
connect_source=$(extract connect_usb_drives_to_host_locked)
connect_source=${connect_source//\/root\/bin\/disable_gadget.sh/test_disable_gadget}
eval "${connect_source//\/root\/bin\/enable_gadget.sh/test_enable_gadget}"
disconnect_source=$(extract disconnect_usb_drives_from_host_locked)
eval "${disconnect_source//\/root\/bin\/disable_gadget.sh/test_disable_gadget}"
clear_archive_status() { :; }
usb_storage_ready() { return 0; }
usb_gadget_is_active() { return 0; }
sync_disk_media_to_host() { gadget_calls+="sync "; }
sync_user_media_to_cam() { return 0; }
record_cam_media_sync_success() { :; }
curl() { :; }

images_rc=$IMAGE_STILL_MOUNTED gadget_calls="" logs=""
if connect_usb_drives_to_host_locked; then fail "connect presented the drive with an image still mounted"; fi
[ "$gadget_calls" = "disable " ] || fail "connect went on after a still-mounted image: $gadget_calls"
[[ $logs == *"not presenting the drive"* ]] || fail "connect refusal not logged"
images_rc=0 gadget_calls=""
touch "$EXFAT_RECLAIM_STAMP"
connect_usb_drives_to_host_locked || fail "connect failed with images released"
[ ! -e "$EXFAT_RECLAIM_STAMP" ] || fail "connect left the reclaim stamp in place"
[[ $gadget_calls == *"enable"* ]] || fail "connect did not present the drive with images released"

images_rc=$IMAGE_STILL_MOUNTED logs=""
if disconnect_usb_drives_from_host_locked; then fail "disconnect reported success with an image still mounted"; fi
[[ $logs == *"not mounting it again"* ]] || fail "disconnect refusal not logged"
images_rc=0
disconnect_usb_drives_from_host_locked || fail "disconnect failed with images released"

mount_point_calls=0 copy_calls=0
ensure_mountpoint_is_mounted_with_retry() { mount_point_calls=$((mount_point_calls + 1)); }
archive_run_command() { copy_calls=$((copy_calls + 1)); }
disconnect_usb_drives_from_host() { return 1; }
if copy_music_files; then fail "music sync went on after a failed disconnect"; fi
[ "$mount_point_calls" -eq 0 ] && [ "$copy_calls" -eq 0 ] || fail "music image mounted or copied after a failed disconnect"

# --- reclaim_exfat_lost_found ----------------------------------------------
eval "$real_reclaim_source"
reset_mounts; populate=files
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin || fail "successful reclaim failed"
[[ $logs =~ Reclaimed\ [0-9]+\ MB\ of\ orphaned\ clusters\ \(1\ rescued\ file\(s\)\)\ on\ /backingfiles/cam_disk.bin ]] ||
  fail "reclaim not logged: $logs"
[ "$last_mnt" = "$EXFAT_RECLAIM_MOUNT" ] || fail "reclaim mounted at '$last_mnt', not the fixed path"
[ -e "$EXFAT_RECLAIM_STAMP" ] || fail "completed reclaim did not record itself"
rm -f "$EXFAT_RECLAIM_STAMP"
[ ! -e "$EXFAT_RECLAIM_MOUNT" ] || fail "reclaim left its mount point behind"
[ "$umount_calls" -eq 1 ] || fail "reclaim did not unmount"

# exfatprogs 1.2.0 leaves an empty LOST+FOUND after every -s run
reset_mounts; populate=empty
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin
[ -z "$logs" ] || fail "empty LOST+FOUND was logged: $logs"
[ "$umount_calls" -eq 1 ] || fail "empty reclaim did not unmount"

reset_mounts
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin
[ -z "$logs" ] || fail "nothing to reclaim but logged: $logs"

reset_mounts; mount_rc=32
reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin || fail "unmountable partition reported as still mounted"
[ ! -e "$EXFAT_RECLAIM_STAMP" ] || fail "failed reclaim recorded itself as done"
[[ $logs == *"could not mount"* ]] || fail "mount failure not logged"
[ "$umount_calls" -eq 0 ] || fail "unmounted after failed mount"

reset_mounts; mounted_src=/dev/loop9p1
if reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin; then fail "already-mounted partition reported as safe"; fi
[[ $logs == *"is already mounted"* ]] || fail "mounted partition was not reported"
[ "$mount_calls" -eq 0 ] || fail "mounted partition was mounted again"

# After the reclaim, an unmount that keeps failing is a failure, not a warning.
reset_mounts; populate=files
umount() { umount_calls=$((umount_calls + 1)); umount_args+="$* "; return 32; }
if reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin; then fail "stuck reclaim mount reported as released"; fi
[ ! -e "$EXFAT_RECLAIM_STAMP" ] || fail "stuck reclaim recorded itself as done"
[[ $umount_args != *"-l"* ]] || fail "stuck reclaim mount was unmounted lazily"
[[ $logs == *"is still mounted"* ]] || fail "stuck reclaim mount not reported"

# A leftover that cannot be released stops the reclaim before it mounts.
reset_mounts; leftover=true
if reclaim_exfat_lost_found /dev/loop9p1 /backingfiles/cam_disk.bin; then fail "unreleasable leftover reported as safe"; fi
[ "$mount_calls" -eq 0 ] || fail "mounted over an unreleasable leftover"

# A full cycle (disconnect pass, then connect pass, with nothing presented
# in between) reclaims the cam drive exactly once.
umount() { umount_calls=$((umount_calls + 1)); leftover=false; }
eval "${fix_errors_in_image_source//\/sbin\/fsck/test_fsck}"
reset_mounts; rm -f "$EXFAT_RECLAIM_STAMP"; fstype=exfat fsck_rc=1
: > "$workdir/fsck-log"
test_fsck() { echo "$*" >> "$workdir/fsck-log"; }
fix_errors_in_image /backingfiles/cam_disk.bin
fix_errors_in_image /backingfiles/cam_disk.bin
[ "$(grep -c -- '-p -s' "$workdir/fsck-log")" -eq 1 ] && [ "$mount_calls" -eq 1 ] ||
  fail "cycle reclaimed $(grep -c -- '-p -s' "$workdir/fsck-log") time(s) with $mount_calls mount(s), want 1"
rm -f "$EXFAT_RECLAIM_STAMP"

# The watchdog's re-present clears the stamp before enabling, like connect.
watchdog=$(extract gadget_stall_watchdog)
[[ $watchdog == *'rm -f "$EXFAT_RECLAIM_STAMP"'*'/root/bin/enable_gadget.sh'* ]] ||
  fail "gadget stall watchdog re-presents without clearing the reclaim stamp"

# Startup clears the stamp and releases a leftover before any worker can
# present the image.
stamp_line=$(grep -n '^rm -f "$EXFAT_RECLAIM_STAMP"' "$script" | cut -d: -f1)
release_line=$(grep -n '^release_exfat_reclaim_mount ' "$script" | cut -d: -f1)
workers_line=$(grep -n '^camdiskworkers &' "$script" | cut -d: -f1)
[ -n "$release_line" ] && [ "$release_line" -lt "$workers_line" ] || fail "startup does not release the reclaim mount before the workers start"
[ -n "$stamp_line" ] && [ "$stamp_line" -lt "$workers_line" ] || fail "startup does not clear the reclaim stamp before the workers start"

[ "$failures" -eq 0 ] || exit 1
echo "archiveloop exFAT reclaim tests passed"
