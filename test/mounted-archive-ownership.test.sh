#!/bin/bash
# Mounted (CIFS/NFS) archive scripts and their runtime patch, in a temp tree.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir -p "$work/bin" "$work/tmp" "$work/fixtures"

# The patcher heredocs must match the shipped scripts byte for byte. Also
# emit the patch function (paths moved into the temp tree) and fixtures for
# the script shapes found on installs today.
python3 - "$root" "$work" <<'PY'
from pathlib import Path
import re, sys
root, work = map(Path, sys.argv[1:])
patcher = (root/'setup/pi/apply-runtime-patches.sh').read_text()
for tag, rel in [('MOUNTEDMON_EOF', 'run/mounted-archive-monitor.sh'),
                 ('NFSCLIPS_EOF', 'run/nfs_archive/archive-clips.sh'),
                 ('CIFSCLIPS_EOF', 'run/cifs_archive/archive-clips.sh')]:
    match = re.search(r"<<'" + tag + r"'\n(.*?\n)" + tag + r"\n", patcher, re.S)
    assert match, f'{tag} heredoc missing'
    assert match[1] == (root/rel).read_text(), f'{tag} heredoc differs from {rel}'

for variant in ['cifs', 'nfs']:
    text = (root/f'run/{variant}_archive/archive-clips.sh').read_text()
    assert f'\n# MOUNTED_ARCHIVE_VARIANT={variant}\n' in text, f'{variant} variant marker missing'
    assert 'MOUNTED_ARCHIVE_CLIPS_V2' in text
    rsync = [l for l in text.splitlines() if ' rsync -avhRL ' in l]
    assert len(rsync) == 1 and all(f in rsync[0] for f in ['--no-o', '--no-g', '--no-perms']), f'{variant} rsync must skip owner, group and permissions'

start = patcher.index('apply_mounted_archive_watchdog() {')
end = patcher.index('\n}\n', patcher.index('archive-clips.sh refreshed', start)) + 3
function = patcher[start:end]
# Move only the patch's own paths; heredoc bodies stay as shipped.
pieces, pos = [], 0
for heredoc in re.finditer(r"<<'(\w+)'\n.*?\n\1\n", function, re.S):
    pieces += [function[pos:heredoc.start()].replace('/root/bin/', str(work/'root/bin') + '/'), heredoc[0]]
    pos = heredoc.end()
pieces.append(function[pos:].replace('/root/bin/', str(work/'root/bin') + '/'))
(work/'patch.sh').write_text(''.join(pieces))

# MOUNTED_ARCHIVE_MONITOR_V1 installs: the current scripts without the
# variant/V2 header, CIFS without the ownership flags.
fixtures = work/'fixtures'
for variant in ['cifs', 'nfs']:
    text = (root/f'run/{variant}_archive/archive-clips.sh').read_text()
    text = re.sub(r'# MOUNTED_ARCHIVE_VARIANT=\w+\n# MOUNTED_ARCHIVE_CLIPS_V2:.*?\n\n', '', text, flags=re.S)
    if variant == 'cifs':
        text = text.replace('rsync -avhRL --no-o --no-g ', 'rsync -avhRL ')
    assert 'MOUNTED_ARCHIVE_VARIANT' not in text and 'MOUNTED_ARCHIVE_MONITOR_V1' in text
    (fixtures/f'v1-{variant}').write_text(text)
# Installs from before the dual-signal monitor.
legacy = '''#!/bin/bash -eu
function connectionmonitor {
  killall rsync || true
}
connectionmonitor $$ &
rsync -avhRL FLAGS--remove-source-files --no-perms --omit-dir-times --files-from="$2" "$1/" "$ARCHIVE_MOUNT" &> /tmp/rsynclog
'''
(fixtures/'legacy-cifs').write_text(legacy.replace('FLAGS', ''))
(fixtures/'legacy-nfs').write_text(legacy.replace('FLAGS', '--no-o --no-g '))
for name in ['rsync', 'rclone', 'none']:
    (fixtures/f'other-{name}').write_text((root/f'run/{name}_archive/archive-clips.sh').read_text())
PY
cifs_new="$root/run/cifs_archive/archive-clips.sh"
nfs_new="$root/run/nfs_archive/archive-clips.sh"
monitor_new="$root/run/mounted-archive-monitor.sh"

# ── Runtime patch ───────────────────────────────────────────────────────
log() { printf '%s\n' "$1" >> "$work/patch.log"; }
err() { printf 'ERR %s\n' "$1" >> "$work/patch.log"; }
findmnt() { printf '%s\n' "$FINDMNT_OPTIONS"; }
mount() { printf 'mount %s\n' "$*" >> "$work/calls"; }
sync() { :; }
source "$work/patch.sh"
bin="$work/root/bin"

reset_install() {
  rm -rf "$work/root" "$work/calls" "$work/patch.log"
  mkdir -p "$bin"
  printf '#!/bin/bash\necho remountfs_rw >> "%s"\n' "$work/calls" > "$bin/remountfs_rw"
  chmod +x "$bin/remountfs_rw"
  cp "$1" "$bin/archive-clips.sh"
  if [ -n "${2:-}" ]; then cp "$2" "$bin/mounted-archive-monitor.sh"; fi
  export FINDMNT_OPTIONS=rw,noatime
}

expect_upgrade() { # fixture monitor expected-script variant
  reset_install "$1" "$2"
  apply_mounted_archive_watchdog
  cmp -s "$bin/archive-clips.sh" "$3" || { echo "$1 did not become $4" >&2; exit 1; }
  cmp -s "$bin/mounted-archive-monitor.sh" "$monitor_new"
  grep -q "mounted-watchdog: $4 archive-clips.sh refreshed" "$work/patch.log"
  [ -x "$bin/archive-clips.sh" ]
  [ -z "$(find "$bin" -name '*.new')" ]
}

expect_upgrade "$work/fixtures/v1-cifs" "$monitor_new" "$cifs_new" cifs
expect_upgrade "$work/fixtures/v1-nfs" "$monitor_new" "$nfs_new" nfs
expect_upgrade "$work/fixtures/legacy-cifs" "" "$cifs_new" cifs
expect_upgrade "$work/fixtures/legacy-nfs" "" "$nfs_new" nfs
# The variant marker wins over the ownership flags both variants now carry.
expect_upgrade "$cifs_new" "" "$cifs_new" cifs
expect_upgrade "$nfs_new" "" "$nfs_new" nfs
echo 'PASS: MOUNTED_ARCHIVE_MONITOR_V1 and older installs upgrade to their own variant'

for variant in cifs nfs; do
  script="$root/run/${variant}_archive/archive-clips.sh"
  reset_install "$script" "$monitor_new"
  echo '# local copy' >> "$bin/archive-clips.sh"
  apply_mounted_archive_watchdog
  grep -q 'mounted-watchdog: already patched' "$work/patch.log"
  [ "$(tail -n 1 "$bin/archive-clips.sh")" = '# local copy' ]
  [ ! -e "$work/calls" ]
done
for name in rsync rclone none; do
  reset_install "$work/fixtures/other-$name"
  apply_mounted_archive_watchdog
  cmp -s "$bin/archive-clips.sh" "$work/fixtures/other-$name"
  [ ! -e "$bin/mounted-archive-monitor.sh" ]
  grep -q 'mounted-watchdog: not applicable' "$work/patch.log"
done
echo 'PASS: current installs and rsync-ssh, rclone and none variants are left alone'

reset_install "$work/fixtures/v1-cifs" "$monitor_new"
export FINDMNT_OPTIONS=ro,noatime
apply_mounted_archive_watchdog
cmp -s "$bin/archive-clips.sh" "$cifs_new"
[ "$(cat "$work/calls")" = "$(printf 'remountfs_rw\nmount -o remount,ro /')" ]
echo 'PASS: a read-only root is remounted for the write and restored afterwards'

# ── rsync exit handling against a share that rejects chown ──────────────
cat > "$work/bin/ionice" <<'STUB'
#!/bin/bash
shift 2
exec "$@"
STUB
cat > "$work/bin/rsync" <<'STUB'
#!/bin/bash
printf '%s\n' "$*" >> "$RSYNC_STUB_CALLS"
case "$RSYNC_STUB_MODE" in
  share)
    if [[ " $* " != *' --no-o '* || " $* " != *' --no-g '* ]]; then
      echo 'rsync: [receiver] chown "/mnt/archive/.sentryusbtmp/clip.mp4.x" failed: Operation not permitted (1)'
      exit 23
    fi ;;
  rename)
    echo 'rsync: [receiver] rename "/mnt/archive/.sentryusbtmp/clip.mp4.x" -> "clip.mp4" failed: Permission denied (13)'
    exit 23 ;;
  vanished) exit 24 ;;
esac
exit 0
STUB
chmod +x "$work/bin/ionice" "$work/bin/rsync"
printf 'connectionmonitor() { :; }\n' > "$work/monitor-stub.sh"
mkdir -p "$work/src" "$work/archive"
touch "$work/filelist"
export RSYNC_STUB_CALLS="$work/rsync-calls" ARCHIVE_MOUNT="$work/archive"

run_clips() { # script mode
  python3 - "$1" "$work" > "$work/clips.sh" <<'PY'
from pathlib import Path
import sys
work = sys.argv[2]
text = Path(sys.argv[1]).read_text()
text = text.replace('/root/bin/mounted-archive-monitor.sh', work + '/monitor-stub.sh').replace('/tmp/', work + '/tmp/')
sys.stdout.write(text)
PY
  rm -f "$work/tmp/archive-error.log"
  RSYNC_STUB_MODE="$2" PATH="$work/bin:$PATH" bash "$work/clips.sh" "$work/src" "$work/filelist" > /dev/null 2>&1
}

run_clips "$cifs_new" share
run_clips "$nfs_new" share
run_clips "$cifs_new" vanished
if run_clips "$work/fixtures/v1-cifs" share; then
  echo 'fixture should reproduce the chown failure' >&2
  exit 1
fi
grep -q 'chown .* failed: Operation not permitted' "$work/tmp/archive-error.log"
for script in "$cifs_new" "$nfs_new"; do
  if run_clips "$script" rename; then
    echo "$script hid a rename failure" >&2
    exit 1
  fi
  grep -q 'rename .* failed: Permission denied' "$work/tmp/archive-error.log"
done
echo 'PASS: chown rejection no longer fails the archive; real write failures still do'
