#!/bin/bash
# Snapshot eviction must order by the car's clip timestamps, not the Pi's
# clock. Field report: on a drive without Wi-Fi the Pi (no RTC, no NTP)
# stamped that day's snapshots OLDER than ones from two days earlier, so
# low-space cleanup deleted today's footage first.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
fail() { echo "FAIL: $*" >&2; exit 1; }

# 1. The clip-time block is identical in the repo script, the OTA
#    whole-file body and the OTA in-place block.
python3 - "$root" "$work" <<'PY'
import pathlib, sys
root, work = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
start = '    # Candidates = snapshot dirs that actually hold a snap.bin, oldest\n'
end = "      LC_ALL=C sort -t $'\\t' -k1,1n -k2,2\n    )\n"
def block(text, after=0):
    a = text.index(start, after)
    return text[a:text.index(end, a) + len(end)]
repo = (root / 'run/manage_free_space.sh').read_text()
patcher = (root / 'setup/pi/apply-runtime-patches.sh').read_text()
mfs = block(patcher, patcher.index("<<'MFS_EOF'"))
cta = block(patcher, patcher.index("<<'CTA_EOF'"))
want = block(repo)
assert mfs == want, 'MFS_EOF body differs from run/manage_free_space.sh'
assert cta == want, 'CTA_EOF block differs from run/manage_free_space.sh'
(work / 'block.sh').write_text(want.replace('/backingfiles/snapshots', str(work / 'snaps')))
# The in-place patch function, pointed at test paths.
a = patcher.index('apply_clip_time_age_block() {')
(work / 'patchfn.sh').write_text(patcher[a:patcher.index('\n}\n', a) + 3])
PY

# 2. Ordering: real-clock snapshots from home (31, 32) and stale-clock
#    snapshots from today's drive (62-64), plus a TOC-less fallback.
mk() { # name mtime clip...
  local d="$work/snaps/$1" m=$2; shift 2
  mkdir -p "$d"; : > "$d/snap.bin"; touch -d "@$m" "$d/snap.bin"
  if [ $# -gt 0 ]; then printf '1048576 %s\n' "$@" > "$d/snap.bin.toc"; fi
}
mk snap-000031 1793000000 TeslaCam/RecentClips/2026-11-08_08-00-00-front.mp4
mk snap-000032 1793000600 TeslaCam/RecentClips/2026-11-08_08-10-00-front.mp4
mk snap-000062 1792000000 TeslaCam/RecentClips/2026-11-10_07-00-00-front.mp4 \
  TeslaCam/SentryClips/2026-11-08_20-00-00/2026-11-08_19-59-00-back.mp4
mk snap-000063 1792000600 TeslaCam/RecentClips/2026-11-10_07-10-00-front.mp4
mk snap-000064 1792001200 TeslaCam/RecentClips/2026-11-10_07-20-00-left_repeater.mp4
mk snap-000007 1700000000            # no TOC: falls back to mtime (oldest)
mkdir -p "$work/snaps/snap-backup"; : > "$work/snaps/snap-backup/snap.bin"

order() { local candidates; source "$work/block.sh"; printf '%s\n' "$candidates" | cut -f2- | sed 's#.*/##' | tr '\n' ' '; }
got=$(order)
[ "$got" = "snap-000007 snap-000031 snap-000032 snap-000062 snap-000063 snap-000064 " ] ||
  fail "eviction order: $got"

# 3. The OTA in-place swap turns the previous bodies (repo script with the
#    inode policy, and the old MFS_EOF body) into the current ones.
log() { :; }; warn() { echo "warn: $*" >&2; }; err() { echo "err: $*" >&2; }
source "$work/patchfn.sh"
# Rebuild the previous shipped body: the current script with the old
# mtime-ordered block and log line put back.
cat > "$work/old-block.txt" <<'OLD_EOF'
    # Candidates = snapshot dirs that actually hold a snap.bin, ordered by
    # the bin's mtime (TRUE age), name as tie-break. Slot numbers are not
    # time-monotonic in the field — a reflash can leave a stale
    # high-numbered snapshot above a restarted sequence — and the old
    # name-order pick (`sort | head -1`) deleted newer footage while
    # sparing the genuinely oldest snapshot. The same list feeds the
    # "fewer than two" guard so the count and the pick can't disagree.
    local candidates
    # `-regex` (not just snap-*) so a scratch dir like snap-backup/ is
    # never a deletion candidate — matches the numeric-only rule the
    # Rust scanners use.
    candidates=$(
      LC_ALL=C find /backingfiles/snapshots \
        -mindepth 2 -maxdepth 2 -type f \
        -regextype posix-extended -regex '/backingfiles/snapshots/snap-[0-9]+/snap\.bin' \
        -printf '%T@\t%h\n' 2>/dev/null |
      LC_ALL=C sort -t $'\t' -k1,1n -k2,2
    )
OLD_EOF
python3 - "$root/run/manage_free_space.sh" "$work/old-block.txt" "$work/old-repo.sh" <<'PY'
import sys
cur, old = open(sys.argv[1]).read(), open(sys.argv[2]).read()
start = '    # Candidates = snapshot dirs that actually hold a snap.bin, oldest\n'
end = "      LC_ALL=C sort -t $'\\t' -k1,1n -k2,2\n    )\n"
a = cur.index(start); b = cur.index(end, a) + len(end)
out = (cur[:a] + old + cur[b:]).replace('(oldest by footage time)"', '(oldest by snap.bin mtime)"')
assert 'CLIP_TIME_AGE' not in out and 'HIGHEST-numbered snapshot' in out
open(sys.argv[3], 'w').write(out)
PY
apply_clip_time_age_block "$work/old-repo.sh"
cmp -s "$work/old-repo.sh" "$root/run/manage_free_space.sh" ||
  fail "in-place swap of the previous repo body does not match run/manage_free_space.sh"

# Unrecognised variants are left alone.
printf '#!/bin/bash\necho local fork\n' > "$work/fork.sh"
cp "$work/fork.sh" "$work/fork.orig"
apply_clip_time_age_block "$work/fork.sh" 2>/dev/null
cmp -s "$work/fork.sh" "$work/fork.orig" || fail "unknown variant was modified"

echo "manage-free-space-age: ok"
