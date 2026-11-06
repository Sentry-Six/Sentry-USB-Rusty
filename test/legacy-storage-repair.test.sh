#!/bin/bash
# Execute the real legacy setup mount phase with device commands stubbed.
# No root, real mounts, repairs, or production paths are used.
set -euo pipefail
cd "$(dirname "$0")/.."
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# Stop extraction before image creation: the mount phase must either return
# safely or stop setup. Keep the actual branches and command ordering intact.
phase=$(awk '
  /^function create_usb_drive_backing_files \(\)/ { printing=1 }
  printing && /^  # Only clean up snapshots/ { exit }
  printing { print }
' setup/pi/setup-sentryusb)
[ -n "$phase" ]
phase=${phase//\/tmp\/create-backingfiles-partition.sh/$tmp\/partition-stub}
printf '#!/bin/sh\nexit 0\n' > "$tmp/partition-stub"
chmod +x "$tmp/partition-stub"

for scenario in success failed already-mounted; do
  mkdir -p "$tmp/$scenario/backingfiles" "$tmp/$scenario/mutable"
  : > "$tmp/$scenario/repair-calls"
  : > "$tmp/$scenario/mount-calls"
  set +e
  (
    set -eu
    BACKINGFILES_MOUNTPOINT="$tmp/$scenario/backingfiles"
    MUTABLE_MOUNTPOINT="$tmp/$scenario/mutable"
    copy_script() { :; }
    setup_progress() { printf '%s\n' "$*"; }
    findmnt() {
      [[ "$*" == *"$MUTABLE_MOUNTPOINT"* || "$scenario" == already-mounted ]]
    }
    losetup() { :; }
    findfs() { printf '/dev/disposable-fixture\n'; }
    timeout() { printf '%s\n' "$*" >> "$tmp/$scenario/repair-calls"; }
    xfs_repair() { printf '%s\n' "$*" >> "$tmp/$scenario/repair-calls"; }
    mount() {
      printf '%s\n' "$*" >> "$tmp/$scenario/mount-calls"
      [[ "$scenario" != failed ]]
    }
    eval "$phase"$'\n}'
    create_usb_drive_backing_files
    printf 'mount-phase-completed\n'
  ) > "$tmp/$scenario/output" 2>&1
  status=$?
  set -e
  if [ -s "$tmp/$scenario/repair-calls" ]; then
    echo "legacy setup invoked repair without confirmation ($scenario)" >&2
    cat "$tmp/$scenario/repair-calls" >&2
    exit 1
  fi
  if [ "$scenario" = failed ]; then
    [ "$status" -ne 0 ]
    grep -q 'Repair Storage' "$tmp/$scenario/output"
    ! grep -q 'mount-phase-completed\|Mounted the partition for the backing files' "$tmp/$scenario/output"
  else
    [ "$status" -eq 0 ]
    grep -q 'mount-phase-completed' "$tmp/$scenario/output"
  fi
  if [ "$scenario" = already-mounted ]; then
    [ ! -s "$tmp/$scenario/mount-calls" ]
  else
    [ "$(wc -l < "$tmp/$scenario/mount-calls")" -eq 1 ]
  fi
done
echo 'legacy storage repair safety tests passed'
