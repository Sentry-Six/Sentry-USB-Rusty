#!/bin/bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export ARCHIVE_RUNTIME_TEST_CALLS="$work/calls"
export ARCHIVE_RUNTIME_TEST_MODE=current
touch "$work/archiveloop"
cat > "$work/sentryusb" <<'STUB'
#!/bin/bash
if [ "$1" = --help ]; then
  [ "$ARCHIVE_RUNTIME_TEST_MODE" != legacy ] || exit 0
  echo 'refresh-archive-scripts'
  exit 0
fi
printf '%s\n' "$*" >> "$ARCHIVE_RUNTIME_TEST_CALLS"
[ "$ARCHIVE_RUNTIME_TEST_MODE" != failure ]
STUB
chmod +x "$work/sentryusb"
python3 - "$root/setup/pi/apply-runtime-patches.sh" "$work" <<'PY'
import pathlib,sys
source=pathlib.Path(sys.argv[1]).read_text()
start=source.index('apply_archive_runtime() {')
end=source.index('\n}\n',start)+3
work=pathlib.Path(sys.argv[2])
(work/'function.sh').write_text(source[start:end].replace('local binary=/opt/sentryusb/sentryusb', 'local binary="'+str(work/'sentryusb')+'"').replace('/root/bin/archiveloop', str(work/'archiveloop')))
assert source.index('run_patch apply_archive_runtime') < source.index('run_patch apply_inode_reserve_cap')
PY
source "$work/function.sh"
log() { :; }
err() { :; }
apply_archive_runtime
[ "$(cat "$work/calls")" = refresh-archive-scripts ]
export ARCHIVE_RUNTIME_TEST_MODE=legacy
apply_archive_runtime
[ "$(wc -l < "$work/calls" | tr -d ' ')" = 1 ]
export ARCHIVE_RUNTIME_TEST_MODE=failure
if apply_archive_runtime; then
  echo 'runtime refresh failure was hidden' >&2
  exit 1
fi
[ "$(wc -l < "$work/calls" | tr -d ' ')" = 2 ]
rm "$work/archiveloop"
apply_archive_runtime
[ "$(wc -l < "$work/calls" | tr -d ' ')" = 2 ]
echo 'offline archive runtime update hook tests passed'

# The legacy shell installer must install the shared formatter before either
# consumer, independently of the Rust install/update paths.
python3 - "$root/setup/pi/configure.sh" "$work/installer-functions" <<'PY'
import pathlib, re, sys
source = pathlib.Path(sys.argv[1]).read_text()
functions = []
for name in ['check_and_install_temperature_monitor', 'install_archive_scripts']:
    match = re.search(r'^function ' + name + r'\b[^\n]*\n.*?^}', source, re.M | re.S)
    assert match
    functions.append(match[0])
pathlib.Path(sys.argv[2]).write_text('\n'.join(functions))
PY
source "$work/installer-functions"
log_progress() { :; }
copy_script() {
  local dest="$2"
  [ "$dest" != /tmp ] || dest="$work/auxiliary"
  mkdir -p "$dest"
  case "$1" in
    run/archiveloop|run/temperature_monitor) [ -f "$dest/system-temperature.sh" ] ;;
  esac
  cp "$root/$1" "$dest/"
}
unset TEMPERATURE_WARNING TEMPERATURE_CAUTION TEMPERATURE_INTERVAL MUSIC_SHARE_NAME
check_and_install_temperature_monitor "$work/monitor-install"
install_archive_scripts "$work/archive-install" run/none_archive
[ -s "$work/monitor-install/temperature_monitor" ]
[ -s "$work/archive-install/archiveloop" ]
echo 'legacy installers publish the temperature helper before its consumers'
