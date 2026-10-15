#!/bin/bash
# Run legacy setup with only the OS/network boundaries mocked in a temp tree.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
python3 - "$root/run/cifs_archive/verify-and-configure-archive.sh" "$work" <<'PY'
from pathlib import Path
import sys
source = Path(sys.argv[1]).read_text()
for directory in ['/tmp/', '/root/', '/mnt/', '/etc/', '/backingfiles/']:
    source = source.replace(directory, sys.argv[2] + directory)
Path(sys.argv[2], 'setup').write_text(source)
PY
mkdir -p "$work/"{tmp,root,mnt,etc,backingfiles}
touch "$work/backingfiles/cam_disk.bin"
mount() { printf '%s\n' "$*" >> "$work/mounts"; }
umount() { :; }
apt-get() { :; }
hping3() { :; }
nc() { :; }
route() { echo 'default 192.0.2.1 wlan0'; }
# macOS and Linux sed differ for -i; emulate only the fixture's fstab deletion.
sed() {
  if [[ ${1:-} == -i ]]; then
    python3 - "$3" <<'PY'
from pathlib import Path
import sys
p = Path(sys.argv[1])
p.write_text(''.join(l for l in p.read_text().splitlines(True) if '.teslaCamArchiveCredentials' not in l))
PY
  else
    command sed "$@"
  fi
}
export ARCHIVE_SERVER=nas.local SHARE_NAME='/Tesla Cam' SHARE_USER=test SHARE_PASSWORD=test
export MUSIC_SHARE_NAME=Music MUSIC_SIZE=1G CIFS_VERSION=3.0 CIFS_SEC=ntlmssp
printf '# original config\n/dev/root / ext4 ro 0 1\n' > "$work/etc/fstab"
for setting in unset true true false; do
  if [[ $setting == unset ]]; then unset CIFS_NOSERVERINO; else export CIFS_NOSERVERINO=$setting; fi
  : > "$work/mounts"
  source "$work/setup" > "$work/output"
  [[ $(grep -c ' cifs ' "$work/etc/fstab") == 2 ]]
  grep -q '^/dev/root / ext4 ro 0 1$' "$work/etc/fstab"
  grep -Fq '//nas.local/Tesla\040Cam ' "$work/etc/fstab"
  grep -Fq 'vers=3.0,sec=ntlmssp' "$work/etc/fstab"
  if [[ $setting == true ]]; then
    [[ $(grep -c noserverino "$work/etc/fstab") == 2 ]]
    [[ $(grep -c noserverino "$work/mounts") == 2 ]]
  else
    if grep -q noserverino "$work/etc/fstab" || grep -q noserverino "$work/mounts"; then
      echo 'disabled compatibility changed the SMB mount options' >&2
      exit 1
    fi
  fi
done
echo 'PASS: SMB compatibility is opt-in, applies to probes and both mounts, survives reruns, and can be disabled'
