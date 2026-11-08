#!/usr/bin/env bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
eval "$(awk '/^function wifi_watchdog_init / {keep=1} /^function set_sys_param/ {exit} keep {print}' "$root/run/archiveloop")"

transport='[ 1234.029208] brcmfmac: mmc_submit_one: CMD53 sg block write failed -84'
supplicant='brcmfmac: failed to enable fw supplicant'
benign='brcmfmac: brcmf_c_process_clm_blob: no clm_blob available (err=-2)'
attempts=0
observe() {
  if wifi_watchdog_observe "$1" "$2"; then attempts=$((attempts + 1)); fi
}

wifi_watchdog_init
for ((i=0;i<162;i++)); do
  observe "$transport" 1234
  observe 'unrelated kernel message' 1234
done
[[ $attempts == 1 && $WIFI_WATCHDOG_REASON == transport ]]
# Continuous faults never re-arm the storm.
for ((i=1235;i<3000;i++)); do observe "$transport" "$i"; done
[[ $attempts == 1 ]]
# After a minute of quiet a new storm is reported again; the 10-minute
# cooldown is enforced by wifi_recovery_guarded (tested below), so a storm
# whose reset was skipped doesn't hide the next one.
for ((i=0;i<6;i++)); do observe "$transport" 3100; done
[[ $attempts == 2 ]]
for ((i=0;i<6;i++)); do observe "$transport" 3105; done
[[ $attempts == 2 ]]
for ((i=0;i<6;i++)); do observe "$transport" 3200; done
[[ $attempts == 3 ]]

wifi_watchdog_init
attempts=0
for ((i=0;i<162;i++)); do observe "$benign" "$i"; done
observe 'another driver: CMD53 sg block write failed -84' 200
[[ $attempts == 0 ]]
for ((i=0;i<20;i++)); do observe "$transport" "$((300 + i * 11))"; done
[[ $attempts == 0 ]]
observe "$supplicant" 600
observe 'unrelated kernel message' 601
observe "$supplicant" 603
[[ $attempts == 1 && $WIFI_WATCHDOG_REASON == supplicant ]]

for pattern in 'brcmf_sdio_txfail: sdio error' 'RXHEADER FAILED: -5' 'max tx seq number error'; do
  wifi_watchdog_init
  attempts=0
  for ((i=0;i<6;i++)); do observe "brcmfmac: $pattern" 1200; done
  [[ $attempts == 1 ]]
done

# A sliding window must retain late samples when its oldest event expires.
wifi_watchdog_init
attempts=0
for event in 1000 1009 1009 1011 1011 1011 1011; do observe "$transport" "$event"; done
[[ $attempts == 1 ]]

redacted=$(printf '%s\n' '2026-09-27T23:23:20-06:00' 'SSID: secret-name' 'Station aa:bb:cc:dd:ee:ff' 'nfs: server private-nas.local not responding, still trying' 'peer 192.168.1.39' | wifi_watchdog_sanitize)
[[ $redacted == *'2026-09-27T23:23:20-06:00'* ]]
[[ $redacted != *secret-name* && $redacted != *aa:bb* && $redacted != *private-nas* && $redacted != *192.168* ]]

# Exercise recovery orchestration against a fake sysfs/debugfs tree; no
# real hardware command runs.
messages=
log() { messages="$messages|$*"; }
wifi_watchdog_capture() { captured=true; }
away=false travel=false
away_mode_active() { [[ $away == true ]]; }
travel_mode_active() { [[ $travel == true ]]; }
tree=$(mktemp -d)
WIFI_SYSFS=$tree/sys WIFI_DEBUGFS=$tree/debug
calls=
mock_clock=5000
wifi_watchdog_uptime() { printf '%s\n' "$mock_clock"; }
sleep() { mock_clock=$((mock_clock + 1)); [[ $reprobe_at == never || $mock_clock -lt $reprobe_at ]] || new_phy; }
reset_tree() {
  rm -rf "$tree"; mkdir -p "$tree/sys/class/net/wlan0/phy80211" "$tree/debug/ieee80211/phy0"
  echo phy0 > "$tree/sys/class/net/wlan0/phy80211/name"
  echo 0 > "$tree/sys/class/net/wlan0/phy80211/index"
  : > "$tree/debug/ieee80211/phy0/reset"
  mkdir -p "$tree/sys/module/brcmfmac/holders" "$tree/sys/module/brcmfmac_cyw/holders"
  : > "$tree/sys/module/brcmfmac/holders/brcmfmac_cyw"
  calls= messages= captured=false reprobe_at=never tee_result=0 rmmod_fail= modprobe_fail=
}
new_phy() { echo 1 > "$tree/sys/class/net/wlan0/phy80211/index"; echo phy1 > "$tree/sys/class/net/wlan0/phy80211/name"; }
timeout() {
  while [[ $1 == -k ]]; do shift 2; done
  shift
  calls="$calls|$*"
  case $1 in
    tee) cat > /dev/null; return "$tee_result" ;;
    rmmod)
      [[ $2 == "$rmmod_fail" ]] && return 1
      [[ $2 == brcmfmac && -n $(ls -A "$tree/sys/module/brcmfmac/holders") ]] && return 1
      rm -rf "$tree/sys/module/$2"; rm -f "$tree/sys/module/brcmfmac/holders/$2" ;;
    modprobe)
      [[ $2 == "$modprobe_fail" ]] && return 1
      mkdir -p "$tree/sys/module/$2/holders"
      if [[ $2 == brcmfmac ]]; then mkdir -p "$tree/sys/module/brcmfmac_cyw/holders"; : > "$tree/sys/module/brcmfmac/holders/brcmfmac_cyw"; fi ;;
    *) echo "unexpected command in recovery: $*"; exit 1 ;;
  esac
}
verify_result=0
wifi_watchdog_verify_link() { return "$verify_result"; }
WIFI_WATCHDOG_REASON=transport

# 1. The chip reset is tried first and needs no module unload.
reset_tree; reprobe_at=5003
wifi_watchdog_recover
[[ $captured == true && $calls == '|tee '*'/phy0/reset' && $messages == *'chip reset and came back'* ]]
[[ $messages == *'throughput and archive recovery unverified'* ]]

# 2. Reset requested but no new phy: report it, never unload under the reset.
reset_tree
if wifi_watchdog_recover; then echo 'unconfirmed reset was reported as success'; exit 1; fi
[[ $calls != *rmmod* && $calls != *modprobe* && $messages == *'not unloading the driver while the reset may still be running'* ]]
# An unidentifiable wiphy (maybe mid-reset) stops everything: no reset
# request and no module unload.
for missing in index name; do
  reset_tree; rm "$tree/sys/class/net/wlan0/phy80211/$missing"
  if wifi_watchdog_recover; then echo "missing phy $missing was reported as success"; exit 1; fi
  [[ $calls == '' && $messages == *'no identifiable Wi-Fi device'* ]]
done

# 3. A failed or hung reset request: outcome unknown, no fallback either.
reset_tree; tee_result=124
if wifi_watchdog_recover; then echo 'failed reset request was reported as success'; exit 1; fi
[[ $calls != *rmmod* && $messages == *'outcome unknown'* ]]

# 4. No reset control: unload the vendor module first, then brcmfmac, then load.
reset_tree; rm "$tree/debug/ieee80211/phy0/reset"
wifi_watchdog_recover
[[ $calls == '|rmmod brcmfmac_cyw|rmmod brcmfmac|modprobe brcmfmac' ]]
[[ -d $tree/sys/module/brcmfmac && -d $tree/sys/module/brcmfmac_cyw && $messages == *'Wi-Fi driver reloaded'* ]]
# The vendor module is loaded again if brcmfmac didn't pull it in.
reset_tree; rm "$tree/debug/ieee80211/phy0/reset"
timeout_real=$(declare -f timeout)
timeout() {
  while [[ $1 == -k ]]; do shift 2; done; shift; calls="$calls|$*"
  case $1 in
    rmmod) rm -rf "$tree/sys/module/$2"; rm -f "$tree/sys/module/brcmfmac/holders/$2" ;;
    modprobe) mkdir -p "$tree/sys/module/$2/holders" ;;
  esac
}
wifi_watchdog_recover
[[ $calls == '|rmmod brcmfmac_cyw|rmmod brcmfmac|modprobe brcmfmac|modprobe brcmfmac_cyw' ]]
eval "$timeout_real"

# 5. An unknown holder stops the fallback before anything is unloaded.
reset_tree; rm "$tree/debug/ieee80211/phy0/reset"
mkdir -p "$tree/sys/module/other_mod/holders"; : > "$tree/sys/module/brcmfmac/holders/other_mod"
if wifi_watchdog_recover; then echo 'unknown holder was unloaded'; exit 1; fi
[[ $calls == '' && $messages == *'other_mod also uses brcmfmac'* ]]
# A vendor module that is itself in use also stops it.
reset_tree; rm "$tree/debug/ieee80211/phy0/reset"; : > "$tree/sys/module/brcmfmac_cyw/holders/x"
if wifi_watchdog_recover; then echo 'busy vendor module was unloaded'; exit 1; fi
[[ $calls == '' && $messages == *'brcmfmac_cyw is in use'* ]]

# 6. brcmfmac won't unload: the vendor module is put back.
reset_tree; rm "$tree/debug/ieee80211/phy0/reset"; rmmod_fail=brcmfmac
if wifi_watchdog_recover; then echo 'failed unload was reported as success'; exit 1; fi
[[ $calls == '|rmmod brcmfmac_cyw|rmmod brcmfmac|modprobe brcmfmac_cyw' && -d $tree/sys/module/brcmfmac_cyw ]]
[[ $messages == *'could not unload brcmfmac'* && $messages == *'no further attempt for this storm'* ]]
# brcmfmac won't load again.
reset_tree; rm "$tree/debug/ieee80211/phy0/reset"; modprobe_fail=brcmfmac
if wifi_watchdog_recover; then echo 'failed load was reported as success'; exit 1; fi
[[ $messages == *'did not load again'* ]]

# 7. A reset whose link can't be verified is still a failure.
reset_tree; reprobe_at=5002; verify_result=1
if wifi_watchdog_recover; then echo 'unverified link was reported as success'; exit 1; fi
[[ $messages == *'connection could not be verified'* ]]
verify_result=0

# 8. Away Mode's hotspot (same chip) blocks any reset; so does Travel Mode
#    for a supplicant-only error, but not for an SDIO transport burst.
reset_tree; mkdir -p "$tree/sys/class/net/ap0"
if wifi_watchdog_recover; then echo 'reset ran with the hotspot up'; exit 1; fi
[[ $calls == '' && $messages == *'Away Mode hotspot is active'* ]]
reset_tree; away=true
if wifi_watchdog_recover; then echo 'reset ran in Away Mode'; exit 1; fi
[[ $calls == '' ]]
away=false
reset_tree; travel=true; WIFI_WATCHDOG_REASON=supplicant
if wifi_watchdog_recover; then echo 'supplicant reset ran in Travel Mode'; exit 1; fi
[[ $calls == '' && $messages == *'Travel Mode is on'* ]]
reset_tree; WIFI_WATCHDOG_REASON=transport; reprobe_at=5002
wifi_watchdog_recover
[[ $calls == '|tee '* ]]
travel=false

# 9. The cooldown commit runs only after the last mode check: Away Mode
#    starting during the capture skips the reset without recording it.
committed=false
commit_cooldown() { committed=true; }
reset_tree; reprobe_at=5002
wifi_watchdog_capture() { mkdir -p "$tree/sys/class/net/ap0"; }
if wifi_watchdog_recover commit_cooldown; then echo 'reset ran after Away Mode started'; exit 1; fi
[[ $committed == false && $calls == '' ]]
wifi_watchdog_capture() { captured=true; }
reset_tree; reprobe_at=5002
wifi_watchdog_recover commit_cooldown
[[ $committed == true && $calls == '|tee '* ]]
commit_failed() { return 1; }
reset_tree
if wifi_watchdog_recover commit_failed; then echo 'reset ran without a cooldown record'; exit 1; fi
[[ $calls == '' ]]

# 10. Recovery never touches the USB drive the car is using.
recovery_code=$(declare -f wifi_watchdog_recover wifi_watchdog_chip_reset wifi_watchdog_module_reload wifi_watchdog_restore_modules wifi_watchdog_blocked)
[[ $recovery_code != *UDC* && $recovery_code != *lun.* && $recovery_code != *gadget* ]]
rm -rf "$tree"
unset WIFI_SYSFS WIFI_DEBUGFS
unset -f sleep timeout

# The boot-scoped timestamp survives monitor restarts and bounds actual reloads.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
WIFI_WATCHDOG_RUNTIME_DIR=$scratch
flock() { return 0; }
mock_clock=1000
wifi_watchdog_uptime() { printf '%s\n' "$mock_clock"; }
wifi_watchdog_recover() { "$1" || return 1; printf 'attempt\n' >> "$scratch/calls"; }
wifi_watchdog_attempt
mock_clock=1001
wifi_watchdog_init
wifi_watchdog_attempt
[[ $(wc -l < "$scratch/calls" | tr -d ' ') == 1 ]]
mock_clock=1600
wifi_watchdog_attempt
[[ $(wc -l < "$scratch/calls" | tr -d ' ') == 2 ]]

echo 'Wi-Fi watchdog burst, benign traffic, cooldown, redaction and recovery outcome checks passed'
