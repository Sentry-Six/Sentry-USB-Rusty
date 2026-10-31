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

log "releasing snapshot $1"
IMAGE="/backingfiles/snapshots/$NAME/snap.bin"

# Remember the links into this snapshot (path<TAB>target) before deleting it,
# so clips that another snapshot still holds can be re-pointed there instead
# of being orphaned (and then never archived).
ORPHANS=$(find /mutable/TeslaCam/ -lname "*/${NAME}/*" -printf '%p\t%l\n' 2>/dev/null || true)

umount "$IMAGE" || true

# delete the snapshot folders
rm -rf "/backingfiles/snapshots/$NAME"

# delete all obsolete links
find /mutable/TeslaCam/ -lname "*/${NAME}/*" -delete || true

# Re-point each removed link at the live snapshot holding the largest copy of
# the same clip (highest slot wins ties). A snapshot taken before the clock
# synced has a past mtime and is evicted early although it is recent; its
# clips may still exist in an older snapshot that survives.
relinked=0
# Listed once: one stat per link over all of them replaces a stat per (link, snapshot).
snapdirs=()
for snapdir in $(ls -d /backingfiles/snapshots/snap-* 2>/dev/null | LC_ALL=C sort -r)
do
  [ -e "$snapdir/snap.bin" ] && snapdirs+=("$snapdir")
done
while IFS=$'\t' read -r link target
do
  [ -n "$link" ] || continue
  [ -L "$link" ] && continue
  case "$target" in
    /backingfiles/snapshots/"$NAME"/mnt/*) rel=${target#/backingfiles/snapshots/"$NAME"/mnt/}; stable=1 ;;
    /tmp/snapshots/"$NAME"/*) rel=${target#/tmp/snapshots/"$NAME"/}; stable=0 ;;
    *) continue ;;
  esac
  [ "${#snapdirs[@]}" -gt 0 ] || continue
  candidates=()
  for snapdir in "${snapdirs[@]}"
  do
    candidates+=("$snapdir/mnt/$rel")
  done
  # xargs keeps argument order (highest slot first), so a strict > keeps the highest slot on ties.
  best=$(printf '%s\0' "${candidates[@]}" | xargs -0 stat -L -c '%s %n' 2>/dev/null |
    awk '{ size = $1 + 0; if (!found || size > max) { found = 1; max = size; path = substr($0, length($1) + 2) } } END { if (found) print path }')
  best=${best#/backingfiles/snapshots/}
  best=${best%%/*}
  [ -n "$best" ] || continue
  mkdir -p "$(dirname "$link")"
  if [ "$stable" = 1 ]
  then
    ln -s "/backingfiles/snapshots/$best/mnt/$rel" "$link" && relinked=$((relinked + 1))
  else
    ln -s "/tmp/snapshots/$best/$rel" "$link" && relinked=$((relinked + 1))
  fi
done <<< "$ORPHANS"
if [ "$relinked" -gt 0 ]
then
  log "re-pointed $relinked clip link(s) from $NAME at surviving snapshots"
fi

# delete all Sentry, saved and recent folders that are now empty
find /mutable/TeslaCam/ -mindepth 2 -depth -type d -empty -exec rmdir "{}" \; || true
