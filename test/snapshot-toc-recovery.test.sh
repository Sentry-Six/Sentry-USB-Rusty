#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)

# Snapshot TOC safety, run against the real make_snapshot.sh and archiveloop
# functions with paths rewritten into a fixture tree and loop devices and
# mounts stubbed. Covers: an image whose TOC never landed is kept, a failed
# scan commits no TOC, links a run still owes get made after the capture,
# the newest snapshot is re-linked once per boot, and the boot-time rebuild
# recovers missing or empty TOCs within its deadline.
work=$(mktemp -d)
trap 'chmod -R u+w "$work" 2>/dev/null; rm -rf "$work"' EXIT
snaps=$work/snapshots
autofs=$work/autofs
farm=$work/mutable/TeslaCam
mkdir -p "$snaps" "$autofs" "$work/bin" "$farm"
: > "$work/mutable/.recentclips_events_purged"
: > "$work/cam_disk.bin"
paths () {
  tr -d '\r' | sed -e "s@/backingfiles/snapshots@$snaps@g" -e "s@/backingfiles@$work@g" \
      -e "s@/tmp/snapshots@$autofs@g" -e "s@/mutable@$work/mutable@g" \
      -e "s@/root/bin@$work/bin@g" -e "s@/tmp/\.sentryusb-@$work/.sentryusb-@g" \
      -e "s@/tmp/toc-recover@$work/toc-recover@g"
}
stub () { printf '#!/bin/bash\n%s\n' "$2" > "$work/bin/$1"; chmod +x "$work/bin/$1"; }
stub ionice 'while [[ ${1:-} == -* ]]; do [[ $1 == -p ]] && exit 0; shift 2; done; exec "$@"'
stub cp 'args=(); for a; do [[ $a == --reflink=* ]] || args+=("$a"); done; exec /bin/cp "${args[@]}"'
stub losetup 'case "$1" in -d) exit 0 ;; *) echo /dev/fakeloop ;; esac'
stub fsck 'exit 0'
stub systemctl 'exit 0'
stub release_snapshot.sh 'rm -rf "$1"'
export PATH=$work/bin:$PATH LOG=$work/log
log () { echo "$*" >> "$LOG"; }
# archiveloop exports these to make_snapshot.sh.
losetup_find_show () { echo /dev/fakeloop; }
export -f log losetup_find_show

paths < "$root/run/make_snapshot.sh" > "$work/make_snapshot.sh"
run_snapshot () {
  FLOCKED=$work/make_snapshot.sh bash -eu "$work/make_snapshot.sh" nofsck
}
clip () {  # clip SNAP SIZE NAME : a RecentClips file inside a snapshot's mount
  mkdir -p "$autofs/$1/TeslaCam/RecentClips"
  head -c "$2" /dev/zero > "$autofs/$1/TeslaCam/RecentClips/$3"
}
committed () {  # committed SNAP : image + TOC listing what its mount holds
  mkdir -p "$snaps/$1"
  : > "$snaps/$1/snap.bin"
  (cd "$autofs/$1" && find . -type f -printf '%s %P\n') > "$snaps/$1/snap.bin.toc"
}
recent () { readlink "$farm/RecentClips/2026-10-30/$1"; }

a=2026-10-30_20-00-00-front.mp4
b=2026-10-30_21-00-00-front.mp4

# 1. The newest image has no TOC: keep it and append after it. The newest
#    committed snapshot is marked as owing links once per boot and re-linked
#    after the new capture.
clip snap-000010 5 "$a"; committed snap-000010
mkdir -p "$snaps/snap-000011"; : > "$snaps/snap-000011/snap.bin"
clip snap-000012 7 "$b"
run_snapshot
[[ -e $snaps/snap-000011/snap.bin && ! -e $snaps/snap-000011/snap.bin.toc ]]
grep -q 'snap-000011 has no TOC — keeping its image for recovery' "$LOG"
[[ $(cat "$snaps/snap-000012/snap.bin.toc") == "7 TeslaCam/RecentClips/$b" ]]
[[ ! -e $snaps/snap-000012/snap.bin.relink_pending ]]
[[ $(recent "$b") == "$snaps/snap-000012/mnt/TeslaCam/RecentClips/$b" ]]
[[ $(recent "$a") == "$snaps/snap-000010/mnt/TeslaCam/RecentClips/$a" ]]
grep -q 're-making links for snap-000010' "$LOG"
[[ $(grep -n 'took snapshot' "$LOG" | cut -d: -f1) -lt $(grep -n 're-making links' "$LOG" | cut -d: -f1) ]]
[[ -e $work/.sentryusb-relink-boot-marked && ! -e $snaps/snap-000010/snap.bin.relink_pending ]]
# Links just made are checked once more after the next boot.
[[ -e $snaps/snap-000010/snap.bin.relink_verify ]]

# 2. Next boot, after a power cut lost a link snapshot 10's re-link made:
#    snapshot 12 is newer now, but 10's re-check still restores it. This
#    run's scan fails: no TOC is committed, the image is kept, and the
#    script fails.
rm "$farm/RecentClips/2026-10-30/$a" "$work/.sentryusb-relink-boot-marked"
: > "$LOG"
if run_snapshot; then echo "expected a failed scan to fail the snapshot"; exit 1; fi
grep -q 're-making links for snap-000012' "$LOG"
grep -q 'filling in missing links for snap-000010' "$LOG"
[[ $(recent "$a") == "$snaps/snap-000010/mnt/TeslaCam/RecentClips/$a" ]]
[[ $(recent "$b") == "$snaps/snap-000012/mnt/TeslaCam/RecentClips/$b" ]]
[[ -e $snaps/snap-000013/snap.bin && ! -e $snaps/snap-000013/snap.bin.toc && ! -e $snaps/snap-000013/snap.bin.toc_ ]]
grep -q 'could not list files' "$LOG"

# 3. Links that could not all be made stay owed and are retried.
: > "$LOG"
clip snap-000014 9 "$b"
chmod a-w "$farm/RecentClips/2026-10-30"
run_snapshot
[[ -e $snaps/snap-000014/snap.bin.relink_pending ]]
grep -q 'link(s) for .*snap-000014 could not be made' "$LOG"
chmod u+w "$farm/RecentClips/2026-10-30"
: > "$LOG"
clip snap-000015 9 "$b"   # same files: a duplicate, discarded
run_snapshot
grep -q 're-making links for snap-000014' "$LOG"
[[ ! -e $snaps/snap-000014/snap.bin.relink_pending && ! -e $snaps/snap-000015 ]]
[[ $(recent "$b") == "$snaps/snap-000014/mnt/TeslaCam/RecentClips/$b" ]]

# 4. Boot-time rebuild: newest missing TOC first, once per boot.
eval "$(awk '/^TOC_RECOVERY_DONE=/ {keep=1} /^function connect_usb_drives_to_host_locked/ {exit} keep {print}' \
  "$root/run/archiveloop" | paths)"
stub mount '[ $# -gt 0 ] || exit 0; cp -r "$FIXTURE/." "${@: -1}" && echo "${@: -1}" >> "$LOG.mounts"'
stub mountpoint 'grep -qxF -- "${@: -1}" "$LOG.mounts" 2>/dev/null'
stub umount 'sed -i "\\#^${1}\$#d" "$LOG.mounts"; rm -rf "${1:?}"/* 2>/dev/null; exit 0'
c=2026-10-30_19-00-00-front.mp4
mkdir -p "$work/fixture/TeslaCam/RecentClips"
head -c 11 /dev/zero > "$work/fixture/TeslaCam/RecentClips/$c"
export FIXTURE=$work/fixture
: > "$LOG"
recover_one_snapshot_toc
[[ $(cat "$snaps/snap-000013/snap.bin.toc") == "11 TeslaCam/RecentClips/$c" ]]
[[ -e $snaps/snap-000013/snap.bin.relink_pending && ! -e $snaps/snap-000011/snap.bin.toc ]]
grep -q 'rebuilt snap-000013 (1 files)' "$LOG"
[[ ! -s $LOG.mounts ]]
recover_one_snapshot_toc   # same boot: nothing more
[[ ! -e $snaps/snap-000011/snap.bin.toc ]]

# The recovered snapshot is older than snap-000014, so the next run only
# fills in its missing links; snap-000014's newer link is kept.
# Its mount also holds an older copy of $b, whose link must not move back.
mkdir -p "$autofs/snap-000013"; cp -r "$FIXTURE/." "$autofs/snap-000013/"
head -c 9 /dev/zero > "$autofs/snap-000013/TeslaCam/RecentClips/$b"
: > "$LOG"
clip snap-000016 9 "$b"   # duplicate again
run_snapshot
grep -q 'filling in missing links for snap-000013' "$LOG"
[[ $(recent "$c") == "$snaps/snap-000013/mnt/TeslaCam/RecentClips/$c" ]]
[[ $(recent "$b") == "$snaps/snap-000014/mnt/TeslaCam/RecentClips/$b" ]]
[[ ! -e $snaps/snap-000013/snap.bin.relink_pending ]]

# The fill-in created a link, so that repair is checked once more next boot.
[[ -e $snaps/snap-000013/snap.bin.relink_verify ]]

# Each run serves the newest and the oldest debt, so an old one isn't
# starved; a snapshot whose links keep failing is skipped for the rest of
# the boot.
: > "$snaps/snap-000014/snap.bin.relink_pending"
: > "$snaps/snap-000013/snap.bin.relink_pending"
mv "$autofs/snap-000014" "$work/hidden-14"
: > "$LOG"
clip snap-000017 9 "$b"; run_snapshot   # duplicates: no new snapshot
grep -q 'links for snap-000014 are incomplete' "$LOG"
grep -q 'filling in missing links for snap-000013' "$LOG"
[[ -e $snaps/snap-000014/snap.bin.relink_pending && ! -e $snaps/snap-000013/snap.bin.relink_pending ]]
[[ $(wc -l < "$snaps/snap-000014/snap.bin.relink_pending") == 1 ]]   # the failure is counted
: > "$LOG"
clip snap-000018 9 "$b"; run_snapshot
! grep -q 'snap-000014' "$LOG"
mv "$work/hidden-14" "$autofs/snap-000014"

# Next boot: the failed one is tried again, and the earlier repair is
# checked. A link lost since (a power cut) is re-created, so it stays on the
# re-check list for one more boot.
rm "$work/.sentryusb-relink-boot-marked" "$work/.sentryusb-relink-failed"
rm "$farm/RecentClips/2026-10-30/$c"
# Only snapshot 13's re-check matters here; drop earlier steps' ones.
find "$snaps" -name snap.bin.relink_verify ! -path '*/snap-000013/*' -delete
: > "$LOG"
clip snap-000019 9 "$b"; run_snapshot
grep -q 're-making links for snap-000014' "$LOG"
grep -q 'filling in missing links for snap-000013' "$LOG"
[[ $(recent "$c") == "$snaps/snap-000013/mnt/TeslaCam/RecentClips/$c" ]]
[[ ! -e $snaps/snap-000013/snap.bin.relink_pending && -e $snaps/snap-000013/snap.bin.relink_verify ]]
# The boot after, nothing is missing: the re-check passes and is done.
rm "$work/.sentryusb-relink-boot-marked"
: > "$LOG"
clip snap-000020 9 "$b"; run_snapshot
grep -q 'filling in missing links for snap-000013' "$LOG"
[[ ! -e $snaps/snap-000013/snap.bin.relink_pending && ! -e $snaps/snap-000013/snap.bin.relink_verify ]]

# Next boot picks the older one; a genuinely empty image is remembered.
rm "$work/.sentryusb-toc-recovery-done"
export FIXTURE=$work/empty; mkdir -p "$FIXTURE"
recover_one_snapshot_toc
[[ -e $snaps/snap-000011/snap.bin.toc && ! -s $snaps/snap-000011/snap.bin.toc ]]
[[ -e $snaps/snap-000011/snap.bin.toc_checked && ! -e $snaps/snap-000011/snap.bin.relink_pending ]]

# A mount that keeps failing is retried for three boots, then left alone.
: > "$snaps/snap-000010/snap.bin.toc"
stub mount 'exit 32'
for _ in 1 2 3 4
do
  rm "$work/.sentryusb-toc-recovery-done"
  recover_one_snapshot_toc
done
[[ $(wc -l < "$snaps/snap-000010/snap.bin.recover_failed") == 3 ]]

# A scan that hangs is stopped at the deadline and the drive is presented.
rm -f "$snaps/snap-000010/snap.bin.recover_failed" "$work/.sentryusb-toc-recovery-done"
stub mount 'echo "${@: -1}" >> "$LOG.mounts"'
stub timeout 'shift; while [[ $1 == -* || $1 =~ ^[0-9]+$ ]]; do shift; done; [[ $1 == find ]] && exec sleep 60.123; exec "$@"'
: > "$LOG"
start=$SECONDS
recover_one_snapshot_toc
(( SECONDS - start < 20 ))
grep -q 'snap-000010 took over 15 s' "$LOG"
[[ ! -e $snaps/snap-000010/snap.bin.toc_ && $(wc -l < "$snaps/snap-000010/snap.bin.recover_failed") == 1 ]]
# Nothing it started survives, its mount is gone, and the lock is free.
! pgrep -f 'sleep 60.123' > /dev/null
[[ ! -s $LOG.mounts ]]
flock -n "$snaps" true

# Cleanup that hangs is bounded too.
rm -f "$snaps/snap-000010/snap.bin.recover_failed" "$work/.sentryusb-toc-recovery-done"
stub timeout 'shift; while [[ $1 == -* || $1 =~ ^[0-9]+$ ]]; do shift; done; [[ $1 == find ]] && exec sleep 60.123; [[ $1 == umount ]] && exec sleep 60.456; exec "$@"'
: > "$LOG"
start=$SECONDS
recover_one_snapshot_toc
(( SECONDS - start < 24 ))
grep -q 'cleanup of .* did not finish' "$LOG"
! pgrep -f 'sleep 60.(123|456)' > /dev/null
flock -n "$snaps" true

echo "snapshot-toc-recovery tests passed"
