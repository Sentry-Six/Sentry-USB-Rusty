#!/bin/bash
# Real rsync interruption through the supervisor, with immutable footage
# behind the same symlink shape used by the snapshot farm. No Pi mounts.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export ARCHIVE_CONTROL_DIR="$work"
source "$root/run/archive-control.sh"
mkdir "$work/originals" "$work/farm" "$work/archive"
dd if=/dev/urandom of="$work/originals/first.mp4" bs=1024 count=32 status=none
dd if=/dev/urandom of="$work/originals/second.mp4" bs=1024 count=1024 status=none
ln -s "$work/originals/first.mp4" "$work/farm/first.mp4"
ln -s "$work/originals/second.mp4" "$work/farm/second.mp4"
sha256sum "$work/originals/"* > "$work/before"
printf 'first.mp4\nsecond.mp4\n' > "$work/files"
archive_cycle_begin
(
  # Request cancellation once the receiver has committed the first file.
  # Sender removal can lag its acknowledgement; retrying that file is safe.
  for _ in {1..200}; do
    if [[ -f "$work/archive/first.mp4" ]]; then
      touch "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$ARCHIVE_CYCLE_ID"
      exit 0
    fi
    sleep .05
  done
  echo 'transfer did not commit its first file before timeout' >&2
  exit 1
) &
request=$!
rc=0
archive_run_command rsync -aRL --remove-source-files --bwlimit=64 --files-from="$work/files" "$work/farm/" "$work/archive/" > "$work/rsync.log" 2>&1 || rc=$?
wait "$request" || { cat "$work/rsync.log"; exit 1; }
[[ $rc == 125 ]]
sha256sum -c "$work/before"
cmp "$work/originals/first.mp4" "$work/archive/first.mp4"
[[ -L "$work/farm/second.mp4" ]] || { echo 'incomplete footage lost its retry link'; exit 1; }
archive_cycle_end
archive_cycle_begin
archive_run_command rsync -aRL --remove-source-files --ignore-missing-args --files-from="$work/files" "$work/farm/" "$work/archive/"
cmp "$work/originals/second.mp4" "$work/archive/second.mp4"
sha256sum -c "$work/before"
archive_cycle_end
echo 'interrupted transfer preserves footage and later retry completes'
