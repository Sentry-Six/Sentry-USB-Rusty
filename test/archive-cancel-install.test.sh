#!/bin/bash
# Run the legacy installer with only its download boundary redirected.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
eval "$(sed 's/\r$//' "$root/setup/pi/configure.sh" | awk '/^function install_archive_scripts / {keep=1} /^function install_python3_pip / {exit} keep')"
log_progress() { :; }
copy_script() {
  local dst="$work/bin"
  mkdir -p "$dst"
  cp "$root/$1" "$dst/"
  # A newly installed loop must never be visible before its dependency.
  if [[ $1 == run/archiveloop ]]; then
    [[ -s "$dst/archive-control.sh" ]] || { echo 'archiveloop installed without cancellation helper' >&2; exit 1; }
  fi
}
for backend in cifs nfs rsync rclone; do
  install_archive_scripts "$work/bin" "run/${backend}_archive"
  cmp "$root/run/archive-control.sh" "$work/bin/archive-control.sh"
  cmp "$root/run/archiveloop" "$work/bin/archiveloop"
  cmp "$root/run/post-archive-process.sh" "$work/bin/post-archive-process.sh"
done
echo 'archive installer tests passed'
