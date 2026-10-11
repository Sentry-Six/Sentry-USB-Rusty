#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
python3 - "$root" "$scratch" <<'PY'
from pathlib import Path
import re, sys
root, scratch = map(Path, sys.argv[1:])
archive = (root/'run/archiveloop').read_text()
helpers = '\n'.join(re.search(r'^function '+name+r' \(\) \{.*?^\}', archive, re.M|re.S)[0]
                    for name in ['usb_storage_ready', 'wait_for_usb_storage', 'connect_usb_at_startup'])
(scratch/'helpers.sh').write_text(helpers.replace('/backingfiles', str(scratch/'backingfiles')))
enable = (root/'run/enable_gadget.sh').read_text()
for prefix in ['/root/bin', '/backingfiles', '/sys', '/etc/machine-id']:
    enable = enable.replace(prefix, str(scratch)+prefix)
(scratch/'enable.sh').write_text(enable)
PY
source "$scratch/helpers.sh"
mkdir -p "$scratch/backingfiles" "$scratch/root/bin" "$scratch/etc"
printf 'fixture-machine\n' > "$scratch/etc/machine-id"
cat > "$scratch/root/bin/envsetup.sh" <<'SH'
isPi5() { return 1; }
isPi4() { return 0; }
SH
export CAM_SIZE=32G
mounted=false
findmnt() {
  if [[ $* == *configfs* ]]; then printf '%s\n' "$scratch/configfs"
  else [[ $mounted == true ]]; fi
}
logs=""
log() { logs+="$*"$'\n'; }
if usb_storage_ready; then echo 'unmounted storage accepted'; exit 1; fi
printf image > "$scratch/backingfiles/cam_disk.bin"
if usb_storage_ready; then echo 'stray image under unmounted directory accepted'; exit 1; fi
mounted=true
usb_storage_ready
: > "$scratch/backingfiles/cam_disk.bin"
if usb_storage_ready; then echo 'empty image accepted'; exit 1; fi
rm "$scratch/backingfiles/cam_disk.bin"
printf music > "$scratch/backingfiles/music_disk.bin"
if usb_storage_ready; then echo 'missing configured camera accepted'; exit 1; fi
export CAM_SIZE=0
usb_storage_ready
export CAM_SIZE=32G

# Late readiness resumes startup without a reboot or archive connection.
sleeps=0
sleep() {
  sleeps=$((sleeps + 1))
  if [[ $sleeps == 2 ]]; then printf image > "$scratch/backingfiles/cam_disk.bin"; fi
}
wait_for_usb_storage
[[ $sleeps == 2 && $logs == *'USB storage ready after 20s'* ]]

# The USB API may start after the archive service, or time out after binding.
attempts=0
api_bound=false
connect_usb_drives_to_host() { attempts=$((attempts + 1)); [[ $attempts -ge 2 ]]; }
usb_gadget_is_active() { [[ $api_bound == true ]]; }
connect_usb_at_startup
[[ $attempts == 2 ]]
attempts=0
api_bound=true
connect_usb_at_startup
[[ $attempts == 1 && $logs == *'binding verified after retry'* ]]

# Run the legacy enabler on fake configfs. Kernel-created LUN attributes
# and controller discovery are mocked; real hardware tools are never run.
modprobe() { printf 'module\n' >> "$scratch/modules"; }
sha256sum() { echo 'fixture-sha'; }
mkdir() {
  command mkdir "$@"
  case "${*: -1}" in
    */functions/mass_storage.0)
      command mkdir -p "${*: -1}/lun.0"
      printf '0\n' > "${*: -1}/lun.0/ro"
      ;;
  esac
}
find() { echo mock-controller; }
enable() { (set -eu; source "$scratch/enable.sh") > "$scratch/result" 2>&1; }
mounted=false
if enable; then echo 'legacy enabler accepted failed mount'; exit 1; fi
[[ ! -e "$scratch/modules" ]]
mounted=true
rm "$scratch/backingfiles/cam_disk.bin"
if enable; then echo 'legacy enabler accepted missing camera with music present'; exit 1; fi
[[ ! -e "$scratch/modules" ]]
printf image > "$scratch/backingfiles/cam_disk.bin"
enable
gadget="$scratch/configfs/usb_gadget/sentryusb"
[[ $(cat "$gadget/UDC") == mock-controller ]]
[[ $(cat "$gadget/functions/mass_storage.0/lun.0/file") == "$scratch/backingfiles/cam_disk.bin" ]]
enable
[[ $(cat "$scratch/result") == 'already prepared' ]]
: > "$gadget/UDC"
if enable; then echo 'legacy enabler accepted unbound gadget'; exit 1; fi
echo 'USB startup tests passed'
