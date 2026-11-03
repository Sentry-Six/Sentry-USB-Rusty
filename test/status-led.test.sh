#!/usr/bin/env bash
# Synthetic lifecycle hints and install wiring; never touches real sysfs or images.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
scratch=$(mktemp -d)
trap 'rm -rf "$scratch"' EXIT
python3 - "$root" "$scratch" <<'PY'
from pathlib import Path
import re, sys
root, scratch = map(Path, sys.argv[1:])
source = (root/'run/archiveloop').read_text()
assert '# sentryusb-status-led: cooperative-v1' in source, 'new archive workers must advertise the LED handoff protocol'
match = re.search(r'^function status_led_archive \(\) \{.*?^\}', source, re.M|re.S)
assert match, 'archive must publish process-bound status without writing LEDs'
(scratch/'hint.sh').write_text(match[0].replace('/run/sentryusb-led', str(scratch/'run')))
assert '$STATUSLED/' not in source, 'archive still owns hardware LEDs'
for name in ['setup/generic/install.sh', 'setup/pi/setup-sentryusb']:
    text = (root/name).read_text()
    assert not re.search(r'>\s*"?\$(?:led|STATUSLED)/', text), f'{name} still writes LEDs'
for name in ['install-pi.sh', 'setup/pi/setup-sentryusb']:
    assert re.search(r'ExecStopPost=-.*status-led-restore', (root/name).read_text()), f'{name} misses crash cleanup'
assert 'apply_status_led_cleanup' in (root/'setup/pi/apply-runtime-patches.sh').read_text()
PY
source "$scratch/hint.sh"
status_led_archive starting
read -r pid start phase < "$scratch/run/archive"
[[ $pid == $$ && $phase == starting && $start =~ ^[0-9]+$ ]]
status_led_archive ready
read -r ready_pid ready_start phase < "$scratch/run/archive"
[[ $ready_pid == "$pid" && $ready_start == "$start" && $phase == ready ]]
status_led_archive working
[[ $(awk '{print $3}' "$scratch/run/archive") == working ]]
# Unwritable/missing runtime storage must not kill bash -eu archive processing.
rm -rf "$scratch/run"
printf blocked > "$scratch/run"
status_led_archive ready
echo 'status LED lifecycle and install checks passed'
