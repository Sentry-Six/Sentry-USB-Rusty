#!/bin/bash -eu

LOG_FILE=${LOG_FILE:-/mutable/archiveloop.log}
function log () {
  echo "$( date ):" "$@" >> "$LOG_FILE" 2>/dev/null || true
}

NAME=$(basename "$1")

if [[ "$NAME" != snap-* ]]
then
  log "invalid snapshot name"
  exit
fi

# The car's USB reads and writes share this SSD and must always win: run the
# whole release (and its children) at the lowest best-effort I/O and CPU
# priority. Applied in place so a caller's snapshot flock stays held.
ionice -c2 -n7 -p $$ >/dev/null 2>&1 || true
renice -n 19 -p $$ >/dev/null 2>&1 || true

log "releasing snapshot $1"
IMAGE="/backingfiles/snapshots/$NAME/snap.bin"

# Remember the links into this snapshot (path<TAB>target) before deleting it,
# so clips that another snapshot still holds can be re-pointed there instead
# of being orphaned (and then never archived).
ORPHANS=$(find /mutable/TeslaCam/ -lname "*/${NAME}/*" -printf '%p\t%l\n' 2>/dev/null || true)

# Plan each re-point before deleting anything, from the surviving snapshots'
# TOCs ("<size> <path>" per file) in one pass. Never look inside a survivor's
# mnt: every lookup there makes autofs loop-mount a full cam image, and doing
# that across hundreds of snapshots starves the car's USB I/O until it drops
# the drive. The largest copy wins, the highest slot breaks ties. A survivor
# whose TOC can't be read is skipped rather than holding up space reclaim.
PLAN=""
if [ -n "$ORPHANS" ]
then
  # Stream the survivor list to awk: an environment variable or argument
  # would hit the kernel's per-string size limit with thousands of snapshots.
  PLAN=$(printf '%s\n' "$ORPHANS" | awk -v name="$NAME" \
      -v snaps=/backingfiles/snapshots -v autofs=/tmp/snapshots \
      -v survivors=<(
        for snapdir in $(ls -d /backingfiles/snapshots/snap-* 2>/dev/null | LC_ALL=C sort -r)
        do
          [ "${snapdir##*/}" != "$NAME" ] || continue
          [ -e "$snapdir/snap.bin" ] || continue
          printf '%s\n' "$snapdir"
        done) '
    BEGIN { FS = "\t"; stable_prefix = snaps "/" name "/mnt/"; autofs_prefix = autofs "/" name "/" }
    {
      if ($1 == "") next
      if (index($2, stable_prefix) == 1) { rel = substr($2, length(stable_prefix) + 1); stable = 1 }
      else if (index($2, autofs_prefix) == 1) { rel = substr($2, length(autofs_prefix) + 1); stable = 0 }
      else next
      if (rel == "" || rel ~ /^\// || rel ~ /\/\// || rel ~ /(^|\/)\.\.?(\/|$)/) next
      n++; link[n] = $1; relof[n] = rel; stab[n] = stable; need[rel] = 1
    }
    END {
      if (n == 0) exit
      while ((getline dir < survivors) > 0) {
        if (dir == "") continue
        toc = dir "/snap.bin.toc"
        hits = 0
        while ((r = (getline line < toc)) > 0) {
          sp = index(line, " ")
          if (sp < 2) continue
          size = substr(line, 1, sp - 1)
          path = substr(line, sp + 1)
          if (size !~ /^[0-9]+$/ || !(path in need)) continue
          hits++; hitpath[hits] = path; hitsize[hits] = size + 0
        }
        close(toc)
        if (r < 0) { skipped++; continue }
        snap = dir; sub(/.*\//, "", snap)
        for (j = 1; j <= hits; j++) {
          p = hitpath[j]
          if (!(p in best) || hitsize[j] > bestsize[p]) { best[p] = snap; bestsize[p] = hitsize[j] }
        }
      }
      for (k = 1; k <= n; k++) {
        rel = relof[k]
        if (!(rel in best)) { unresolved++; continue }
        if (stab[k]) target = snaps "/" best[rel] "/mnt/" rel
        else target = autofs "/" best[rel] "/" rel
        print "L\t" link[k] "\t" target
      }
      print "S\t" skipped + 0 "\t" unresolved + 0
    }') || { log "could not plan clip re-points for $NAME"; PLAN=""; }
fi

umount "$IMAGE" || true

# delete the snapshot folders
rm -rf "/backingfiles/snapshots/$NAME"

# delete all obsolete links
find /mutable/TeslaCam/ -lname "*/${NAME}/*" -delete || true

relinked=0
skipped=0
unresolved=0
while IFS=$'\t' read -r kind link target
do
  case "$kind" in
    L)
      # Someone (a new snapshot) already re-created it.
      [ -L "$link" ] && continue
      mkdir -p "$(dirname "$link")"
      ln -s "$target" "$link" && relinked=$((relinked + 1))
      ;;
    S)
      skipped=$link
      unresolved=$target
      ;;
  esac
done <<< "$PLAN"
if [ "$relinked" -gt 0 ]
then
  log "re-pointed $relinked clip link(s) from $NAME at surviving snapshots"
fi
if [ "$skipped" -gt 0 ]
then
  log "skipped $skipped surviving snapshot(s) with an unreadable TOC while releasing $NAME; $unresolved clip link(s) found no matching copy in readable TOCs"
fi

# delete all Sentry, saved and recent folders that are now empty
find /mutable/TeslaCam/ -mindepth 2 -depth -type d -empty -exec rmdir "{}" \; || true
