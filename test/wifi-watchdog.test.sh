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
# Continuous faults never re-arm the storm, even after the cooldown.
for ((i=1235;i<3000;i++)); do observe "$transport" "$i"; done
[[ $attempts == 1 ]]
# Quiet time and the cooldown must both elapse before another attempt.
for ((i=0;i<6;i++)); do observe "$transport" 3100; done
[[ $attempts == 2 ]]
for ((i=0;i<6;i++)); do observe "$transport" 3200; done
[[ $attempts == 2 ]]
for ((i=0;i<6;i++)); do observe "$transport" 3700; done
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

# Exercise recovery orchestration without running real hardware commands.
messages=
log() { messages="$messages|$*"; }
wifi_watchdog_capture() { captured=true; }
modprobe_calls=0
mock_unload_result=124
mock_load_result=0
timeout() {
  [[ $* == '-k 2 8 modprobe'* ]]
  modprobe_calls=$((modprobe_calls + 1))
  if [[ $* == *'modprobe -r'* ]]; then return "$mock_unload_result"; fi
  return "$mock_load_result"
}
verify_result=0
wifi_watchdog_verify_link() { return "$verify_result"; }
captured=false
if wifi_watchdog_recover; then echo 'failed unload was reported as success'; exit 1; fi
[[ $captured == true && $modprobe_calls == 2 && $messages == *'unload=124, load=0'* ]]
[[ $messages != *'associated and gateway reachable'* ]]
mock_unload_result=0
mock_load_result=1
messages=
if wifi_watchdog_recover; then echo 'failed load was reported as success'; exit 1; fi
[[ $messages == *'unload=0, load=1'* ]]
mock_load_result=0
verify_result=1
messages=
if wifi_watchdog_recover; then echo 'unverified link was reported as success'; exit 1; fi
[[ $messages == *'connection could not be verified'* ]]
verify_result=0
messages=
wifi_watchdog_recover
[[ $messages == *'throughput and archive recovery unverified'* ]]

# The boot-scoped timestamp survives monitor restarts and bounds actual reloads.
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
WIFI_WATCHDOG_RUNTIME_DIR=$scratch
flock() { return 0; }
mock_clock=1000
wifi_watchdog_uptime() { printf '%s\n' "$mock_clock"; }
wifi_watchdog_recover() { printf 'attempt\n' >> "$scratch/calls"; }
wifi_watchdog_attempt
mock_clock=1001
wifi_watchdog_init
wifi_watchdog_attempt
[[ $(wc -l < "$scratch/calls" | tr -d ' ') == 1 ]]
mock_clock=1600
wifi_watchdog_attempt
[[ $(wc -l < "$scratch/calls" | tr -d ' ') == 2 ]]

echo 'Wi-Fi watchdog burst, benign traffic, cooldown, redaction and recovery outcome checks passed'
