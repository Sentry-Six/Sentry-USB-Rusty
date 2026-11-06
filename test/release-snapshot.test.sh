#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)

# Run release_snapshot.sh against a fixture tree. Survivors' `mnt` dirs hold
# decoy files of the wrong size: any lookup through mnt (which on a Pi makes
# autofs mount the image) picks the wrong snapshot, so only a TOC-driven plan
# produces the expected links.
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
snaps=$work/snapshots
farm=$work/TeslaCam
mkdir -p "$snaps" "$farm/RecentClips/2026-10-30" "$farm/SavedClips/evt" "$work/bin"
printf '#!/bin/sh\nexit 0\n' > "$work/bin/umount"
chmod +x "$work/bin/umount"
sed -e "s@/backingfiles/snapshots@$snaps@g" -e "s@/mutable/TeslaCam@$farm@g" \
    -e "s@/tmp/snapshots@$work/autofs@g" "$root/run/release_snapshot.sh" > "$work/release.sh"

snap () {  # snap NAME [SIZE PATH]...
  local dir=$snaps/$1
  shift
  mkdir -p "$dir/mnt"
  : > "$dir/snap.bin"
  : > "$dir/snap.bin.toc"
  while [ $# -gt 0 ]
  do
    printf '%s %s\n' "$1" "$2" >> "$dir/snap.bin.toc"
    mkdir -p "$(dirname "$dir/mnt/$2")"
    head -c $((1000 - $1)) /dev/zero > "$dir/mnt/$2"
    shift 2
  done
}

full=TeslaCam/RecentClips/2026-10-30_20-50-00-front.mp4
tie="TeslaCam/SavedClips/evt/2026-10-30_20-40-00 front.mp4"
gone=TeslaCam/RecentClips/2026-10-30_20-51-00-front.mp4
kept=TeslaCam/RecentClips/2026-10-30_20-52-00-front.mp4
snap snap-000035 40 "$full" 30 "$tie"
snap snap-000036 40 "$full" 30 "$tie" 50 "$gone" 10 "$kept"
snap snap-000037 10 "$full" 30 "$tie" 10 "$kept"
snap snap-000038
rm "$snaps/snap-000038/snap.bin.toc"
printf '99 %s\n' "$full" > "$snaps/snap-000037/snap.bin.toc_"

link () { ln -s "$snaps/snap-000036/mnt/$2" "$farm/$1"; }
link RecentClips/2026-10-30/full.mp4 "$full"
link SavedClips/evt/tie.mp4 "$tie"
link RecentClips/2026-10-30/gone.mp4 "$gone"
link RecentClips/2026-10-30/kept.mp4 "$kept"
# Already re-pointed at a survivor: not owned by 36, so left as is.
rm "$farm/RecentClips/2026-10-30/kept.mp4"
ln -s "$snaps/snap-000037/mnt/$kept" "$farm/RecentClips/2026-10-30/kept.mp4"
ln -s "$snaps/snap-000036/mnt/$kept" "$farm/RecentClips/2026-10-30/kept2.mp4"

PATH=$work/bin:$PATH LOG_FILE=$work/log bash -eu "$work/release.sh" "$snaps/snap-000036"

[[ ! -e $snaps/snap-000036 ]]
[[ $(readlink "$farm/RecentClips/2026-10-30/full.mp4") == "$snaps/snap-000035/mnt/$full" ]]
[[ $(readlink "$farm/SavedClips/evt/tie.mp4") == "$snaps/snap-000037/mnt/$tie" ]]
[[ ! -L $farm/RecentClips/2026-10-30/gone.mp4 ]]
[[ $(readlink "$farm/RecentClips/2026-10-30/kept.mp4") == "$snaps/snap-000037/mnt/$kept" ]]
[[ $(readlink "$farm/RecentClips/2026-10-30/kept2.mp4") == "$snaps/snap-000037/mnt/$kept" ]]
grep -q 're-pointed 3 clip link(s) from snap-000036' "$work/log"
grep -q 'skipped 1 surviving snapshot(s) with an unreadable TOC while releasing snap-000036; 1 clip link(s)' "$work/log"

# Releasing a snapshot nothing links into reads no TOC and logs no re-point.
: > "$work/log"
snap snap-000040
PATH=$work/bin:$PATH LOG_FILE=$work/log bash -eu "$work/release.sh" snap-000040
[[ ! -e $snaps/snap-000040 ]]
! grep -q -e 're-pointed' -e 'skipped' "$work/log"

# Thousands of survivors must not hit the kernel's per-string size limit.
: > "$work/log"
for ((i = 1000; i < 5000; i++))
do
  mkdir "$snaps/snap-00$i"
  : > "$snaps/snap-00$i/snap.bin"
  : > "$snaps/snap-00$i/snap.bin.toc"
done
printf '90 %s\n' "$full" > "$snaps/snap-004999/snap.bin.toc"
snap snap-000041 10 "$full"
ln -s "$snaps/snap-000041/mnt/$full" "$farm/RecentClips/2026-10-30/many.mp4"
PATH=$work/bin:$PATH LOG_FILE=$work/log bash -eu "$work/release.sh" snap-000041
[[ $(readlink "$farm/RecentClips/2026-10-30/many.mp4") == "$snaps/snap-004999/mnt/$full" ]]
grep -q 're-pointed 1 clip link(s) from snap-000041' "$work/log"

echo "release-snapshot tests passed"
