#!/usr/bin/env bash
# The runtime patch that keeps Wi-Fi power saving off, run against a fake
# NetworkManager directory with stubbed tools; never touches the real system.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT

python3 - "$root/setup/pi/apply-runtime-patches.sh" "$scratch/patch.sh" <<'PY'
import re, sys
source = open(sys.argv[1]).read()
match = re.search(r'^apply_wifi_powersave_off\(\) \{.*?^\}', source, re.M | re.S)
assert match, 'apply_wifi_powersave_off is missing'
assert 'run_patch apply_wifi_powersave_off' in source, 'the patch is not in the run list'
open(sys.argv[2], 'w').write(match[0] + '\n')
PY

# Stub tools: record every call, pretend root is read-only, list two radios.
bin=$scratch/bin; mkdir -p "$bin"; calls=$scratch/calls; : > "$calls"
cat > "$bin/findmnt" <<'EOF'
#!/bin/sh
echo "ro,noatime"
EOF
cat > "$bin/mount" <<EOF
#!/bin/sh
echo "mount \$*" >> "$calls"
EOF
cat > "$bin/nmcli" <<EOF
#!/bin/sh
echo "nmcli \$*" >> "$calls"
EOF
cat > "$bin/sync" <<'EOF'
#!/bin/sh
exit 0
EOF
cat > "$bin/iw" <<EOF
#!/bin/sh
if [ "\$1" = dev ] && [ \$# -eq 1 ]; then
  printf 'phy#0\n\tUnnamed/non-netdev interface\n\t\ttype P2P-device\n\tInterface wlan0\n\t\ttype managed\nphy#1\n\tInterface wlan1\n\t\ttype managed\n'
else
  echo "iw \$*" >> "$calls"
fi
EOF
chmod +x "$bin"/*
export PATH="$bin:$PATH"

log() { echo "[patches] $1"; }
err() { echo "[patches] ERROR $1" >&2; }
source "$scratch/patch.sh"

# 1. A fresh install writes the setting, reloads NetworkManager, relocks root
#    and switches power saving off on every radio right away.
export SENTRYUSB_NM_DIR=$scratch/NetworkManager
mkdir -p "$SENTRYUSB_NM_DIR"
apply_wifi_powersave_off
dropin=$SENTRYUSB_NM_DIR/conf.d/sentryusb-wifi-powersave.conf
grep -qx 'wifi.powersave = 2' "$dropin"
grep -qx '\[connection\]' "$dropin"
grep -qx 'mount -o remount,rw /' "$calls"
grep -qx 'mount -o remount,ro /' "$calls"
grep -qx 'nmcli general reload conf' "$calls"
grep -qx 'iw dev wlan0 set power_save off' "$calls"
grep -qx 'iw dev wlan1 set power_save off' "$calls"

# 2. Running again changes nothing on disk and leaves root alone.
: > "$calls"
before=$(cat "$dropin")
apply_wifi_powersave_off | grep -q 'already current'
[ "$(cat "$dropin")" = "$before" ]
! grep -q '^mount' "$calls"
! grep -q '^nmcli' "$calls"
grep -qx 'iw dev wlan0 set power_save off' "$calls"

# 3. A hand-edited or older file is replaced.
echo 'wifi.powersave = 3' > "$dropin"
apply_wifi_powersave_off >/dev/null
grep -qx 'wifi.powersave = 2' "$dropin"

# 4. Without NetworkManager nothing is written, but the live switch still runs.
: > "$calls"
export SENTRYUSB_NM_DIR=$scratch/absent
apply_wifi_powersave_off | grep -q 'NetworkManager not in use'
[ ! -e "$scratch/absent" ]
grep -qx 'iw dev wlan0 set power_save off' "$calls"

echo 'Wi-Fi power saving patch checks passed'
