#!/usr/bin/env bash
# Exercise the production collector against a stalled gadget and missing
# evidence without touching hardware, mounting images, or making requests.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
python3 - "$root/crates/api/src/healthcheck.rs" "$scratch" <<'PY'
from pathlib import Path
import sys
source = Path(sys.argv[1]).read_text()
script = source.split('const DIAGNOSTICS_SCRIPT: &str = r#"', 1)[1].split('"#;', 1)[0]
for prefix in ['/backingfiles', '/mutable', '/sys', '/proc', '/opt', '/run']:
    script = script.replace(prefix, sys.argv[2] + prefix)
Path(sys.argv[2], 'collector.sh').write_text(script)
PY
bash -n "$scratch/collector.sh"
gadget="$scratch/sys/kernel/config/usb_gadget/sentryusb"
lun="$gadget/functions/mass_storage.0/lun.0"
mkdir -p "$lun" "$gadget/configs/c.1" "$scratch/sys/class/udc/mock" \
  "$scratch/proc/123" "$scratch/backingfiles" "$scratch/mutable" \
  "$scratch/opt/sentryusb" "$scratch/run" "$scratch/sys/firmware/devicetree/base"
printf 'mock-controller\n' > "$gadget/UDC"
printf '0x0200\n' > "$gadget/bcdUSB"
printf '500\n' > "$gadget/configs/c.1/MaxPower"
printf '/backingfiles/cam_disk.bin\n' > "$lun/file"
printf '0\n' > "$lun/ro"
printf '1\n' > "$lun/nofua"
printf '0\n' > "$lun/removable"
printf 'configured\n' > "$scratch/sys/class/udc/mock/state"
printf 'high-speed\n' > "$scratch/sys/class/udc/mock/current_speed"
printf 'file-storage\n' > "$scratch/proc/123/comm"
printf 'write_bytes: 4096\n' > "$scratch/proc/123/io"
printf 'io_schedule\n' > "$scratch/proc/123/wchan"
printf 'disk /backingfiles ext4 rw 0 0\n' > "$scratch/proc/mounts"
printf 'mock disk counters\n' > "$scratch/proc/diskstats"
printf 'Raspberry Pi 4 Model B\n' > "$scratch/sys/firmware/devicetree/base/model"
printf 'test-version\n' > "$scratch/opt/sentryusb/version"
touch "$scratch/backingfiles/cam_disk.bin"
printf 'state_age=12s cam_age=3600s\n' > "$scratch/mutable/sentryusb-ble.log"
printf 'dwc2: mock stalled endpoint\n' > "$scratch/mutable/kernel.log"
for i in 1 2 3 4; do printf 'STALL_EVIDENCE_%s\n' "$i" > "$scratch/mutable/gadget_stall_20261009_00000$i.log"; done
for i in $(seq 1 1100); do printf 'ARCHIVE_LINE_%s\n' "$i"; done > "$scratch/mutable/archiveloop.log"
mtime=$(($(date +%s) - 3600))
timeout() { shift; "$@"; }
stat() { printf 'size_bytes=1000000 mtime_epoch=%s modified=one-hour-ago\n' "$mtime"; }
sleep() { :; }
hostname() { echo mock-pi; }
uptime() { echo mock-uptime; }
uname() { echo mock-kernel; }
df() { echo 'mock capacity and inode headroom'; }
du() { echo '1G cam_disk.bin'; }
ip() { echo 'inet 192.0.2.1'; }
systemctl() { echo active; }
journalctl() { echo 'mock drive cache: journal'; }
curl() { echo 'mock BLE and import status'; }
dmesg() { echo 'dwc2: live mock evidence'; }
vcgencmd() {
  case "$1" in
    get_throttled) echo 'throttled=0x50000' ;;
    measure_temp) echo "temp=45.0'C" ;;
    *) return 1 ;;
  esac
}
# Any recovery or mount command would fail the test, even if swallowed.
mount() { touch "$scratch/mutation"; return 1; }
modprobe() { touch "$scratch/mutation"; return 1; }
reboot() { touch "$scratch/mutation"; return 1; }
capture() { (set +e; source "$scratch/collector.sh") > "$scratch/report"; }
capture
report=$(cat "$scratch/report")
[[ $report == *'test-version'* && $report == *'mock-kernel'* ]]
[[ $report == *'state: configured'* && $report == *'current_speed: high-speed'* ]]
[[ $report == *'/nofua: 1'* && $report == *'/ro: 0'* ]]
[[ $report == *'cam_last_write_secs=36'* && $report == *'write_bytes: 4096'* ]]
[[ $(printf '%s\n' "$report" | awk '/Sample UTC:/ {n++} END {print n}') == 2 ]]
[[ $report == *'throttled=0x50000'* && $report == *'PMIC rail measurements unavailable'* ]]
[[ $report == *'dwc2: mock stalled endpoint'* && $report == *'state_age=12s'* ]]
[[ $report == *'STALL_EVIDENCE_4'* && $report == *'STALL_EVIDENCE_2'* && $report != *'STALL_EVIDENCE_1'* ]]
[[ $report == *'ARCHIVE_LINE_1100'* && $report != *$'ARCHIVE_LINE_100\n'* ]]
[[ ! -e "$scratch/mutation" ]]
rm -rf "$gadget" "$scratch/mutable" "$scratch/proc/123"
capture
report=$(cat "$scratch/report")
[[ $report == *'Gadget configuration absent'* ]]
[[ $report == *'no persistent kernel history available'* && $report == *'no BLE heartbeat history available'* ]]
[[ $report == *'end of diagnostics'* && ! -e "$scratch/mutation" ]]
echo 'diagnostics capture tests passed'
