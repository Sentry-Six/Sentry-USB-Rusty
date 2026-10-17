#!/usr/bin/env bash
# Exercise the production collector against a stalled gadget, failed DHCP,
# and missing evidence without touching hardware or changing services.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
python3 - "$root/crates/api/src/healthcheck.rs" "$scratch" <<'PY'
from pathlib import Path
import sys
source = Path(sys.argv[1]).read_text()
script = source.split('const DIAGNOSTICS_SCRIPT: &str = r#"', 1)[1].split('"#;', 1)[0]
for prefix in ['/backingfiles', '/mutable', '/sys', '/proc', '/opt', '/run', '/etc']:
    script = script.replace(prefix + '/', sys.argv[2] + prefix + '/')
Path(sys.argv[2], 'collector.sh').write_text(script)
PY
bash -n "$scratch/collector.sh"
gadget="$scratch/sys/kernel/config/usb_gadget/sentryusb"
lun="$gadget/functions/mass_storage.0/lun.0"
mkdir -p "$lun" "$gadget/configs/c.1" "$scratch/sys/class/udc/mock" \
  "$scratch/proc/123" "$scratch/backingfiles" "$scratch/mutable" \
  "$scratch/opt/sentryusb" "$scratch/run" "$scratch/sys/firmware/devicetree/base" \
  "$scratch/etc/systemd/network" "$scratch/etc/sentryusb/networkd-ap" "$scratch/etc/netplan"
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
printf '[Match]\nName=ap0\n[Network]\nAddress=10.3.0.159/24\nDHCPServer=yes\n' \
  > "$scratch/etc/systemd/network/05-sentryusb-ap.network"
printf 'wpa_passphrase=SECRET_HOTSPOT_PASSWORD\n' > "$scratch/etc/sentryusb/networkd-ap/hostapd.conf"
printf 'password: SECRET_HOME_WIFI_PASSWORD\n' > "$scratch/etc/netplan/10-wifi.yaml"
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
lsblk() { echo 'sda /dev/sda disk 512G usb SanDisk Ultra_Flair xfs /backingfiles'; }
findmnt() { echo 'recording mount probe'; }
ip() { echo 'inet 192.0.2.1'; }
network_probe_mode=blocked
network_probe_available() {
  case "$network_probe_mode" in
    missing) echo 'mock network tool not installed' >&2; return 127 ;;
    timeout) echo 'mock network probe timed out' >&2; return 124 ;;
  esac
}
networkctl() {
  network_probe_available || return $?
  case "$*" in
    --version) echo 'systemd 257 (mock)' ;;
    'status ap0 --no-pager') echo 'ap0: routable; Network File: /etc/systemd/network/05-sentryusb-ap.network' ;;
    *) touch "$scratch/mutation"; return 1 ;;
  esac
}
systemctl() {
  case " $* " in
    *' status '*)
      if [[ " $* " == *' systemd-networkd.service '* ]]; then
        network_probe_available || return $?
        echo 'sentryusb-networkd-ap.service: active'
        if [[ $network_probe_mode == blocked ]]; then
          echo 'systemd-networkd-persistent-storage.service: failed (exit-code)'
          return 3
        fi
      fi
      echo active
      ;;
    ' is-active '*) echo active ;;
    *) touch "$scratch/mutation"; return 1 ;;
  esac
}
journalctl() {
  if [[ " $* " == *' systemd-networkd.service '* ]]; then
    network_probe_available || return $?
    [[ " $* " == *' -b '* && " $* " == *' -n 150 '* && " $* " == *' --no-pager '* ]] || return 1
    echo 'mock networkd journal: persistent storage unavailable'
  else
    echo 'mock drive cache: journal'
  fi
}
ss() {
  network_probe_available || return $?
  [[ $* == '-4 -H -u -l -n -p sport = :67' ]] || { touch "$scratch/mutation"; return 1; }
  if [[ $network_probe_mode == listening ]]; then
    echo 'UNCONN 0 0 0.0.0.0%ap0:67 0.0.0.0:* users:(("systemd-network",pid=123,fd=45))'
  fi
}
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
[[ $report == *'SanDisk Ultra_Flair'* && $report == *'recording mount probe'* ]]
[[ $report == *'state: configured'* && $report == *'current_speed: high-speed'* ]]
[[ $report == *'/nofua: 1'* && $report == *'/ro: 0'* ]]
[[ $report == *'cam_last_write_secs=36'* && $report == *'write_bytes: 4096'* ]]
[[ $(printf '%s\n' "$report" | awk '/Sample UTC:/ {n++} END {print n}') == 2 ]]
[[ $report == *'throttled=0x50000'* && $report == *'PMIC rail measurements unavailable'* ]]
[[ $report == *'dwc2: mock stalled endpoint'* && $report == *'state_age=12s'* ]]
[[ $report == *'STALL_EVIDENCE_4'* && $report == *'STALL_EVIDENCE_2'* && $report != *'STALL_EVIDENCE_1'* ]]
[[ $report == *'ARCHIVE_LINE_1100'* && $report != *$'ARCHIVE_LINE_100\n'* ]]
[[ $report == *'systemd 257 (mock)'* && $report == *'ap0: routable'* ]]
[[ $report == *'Address=10.3.0.159/24'* && $report == *'DHCPServer=yes'* ]]
[[ $report == *'persistent-storage.service: failed (exit-code)'* ]]
[[ $report == *'mock networkd journal: persistent storage unavailable'* ]]
[[ $report == *'No IPv4 UDP port 67 listeners found'* ]]
[[ $report == *'SentryUSB runtime DHCP override is not installed'* ]]
[[ $report != *'SECRET_HOTSPOT_PASSWORD'* && $report != *'SECRET_HOME_WIFI_PASSWORD'* ]]
[[ ! -e "$scratch/mutation" ]]
network_probe_mode=listening
mkdir -p "$scratch/run/systemd/network/05-sentryusb-ap.network.d"
printf '[DHCPServer]\nPersistLeases=no\nBindToInterface=yes\n' \
  > "$scratch/run/systemd/network/05-sentryusb-ap.network.d/90-sentryusb-dhcp.conf"
capture
report=$(cat "$scratch/report")
[[ $report == *'0.0.0.0%ap0:67'* && $report != *'No IPv4 UDP port 67 listeners found'* ]]
[[ $report == *'--- SentryUSB runtime DHCP override ---'* && $report == *'PersistLeases=no'* && $report == *'BindToInterface=yes'* ]]
rm -rf "$gadget" "$scratch/mutable" "$scratch/proc/123"
rm "$scratch/etc/systemd/network/05-sentryusb-ap.network"
network_probe_mode=missing
capture
report=$(cat "$scratch/report")
[[ $report == *'Gadget configuration absent'* ]]
[[ $report == *'no persistent kernel history available'* && $report == *'no BLE heartbeat history available'* ]]
[[ $report == *'SentryUSB networkd AP configuration is not installed'* ]]
[[ $report == *'networkd version unavailable'* && $report == *'ap0 networkd status unavailable or timed out'* ]]
[[ $report == *'DHCP listener probe unavailable or timed out'* && $report != *'No IPv4 UDP port 67 listeners found'* ]]
[[ $report == *'networkd/hotspot journal unavailable or timed out'* ]]
[[ $report == *'end of diagnostics'* && ! -e "$scratch/mutation" ]]
network_probe_mode=timeout
capture
report=$(cat "$scratch/report")
[[ $report == *'mock network probe timed out'* && $report == *'ap0 networkd status unavailable or timed out'* ]]
[[ $report == *'DHCP listener probe unavailable or timed out'* && $report == *'networkd/hotspot journal unavailable or timed out'* ]]
[[ $report == *'end of diagnostics'* && ! -e "$scratch/mutation" ]]
echo 'diagnostics capture tests passed'
