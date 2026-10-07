#!/bin/bash

# Resolve the saved system-temperature preference without sourcing the config.
# Long-lived archive/monitor processes must observe UI changes and removal of an
# override, rather than retaining the environment they inherited at startup.
function system_temperature_unit () {
  local fallback="${SYSTEM_TEMPERATURE_UNIT:-${TEMPERATURE_UNIT:-C}}" unit
  if [ "$#" -eq 0 ]; then
    set -- /root/sentryusb.conf /boot/firmware/sentryusb.conf /boot/sentryusb.conf
  fi
  unit=$(python3 - "$fallback" "$@" <<'PY'
import re
import shlex
import sys

value = sys.argv[1]
for filename in sys.argv[2:]:
    try:
        with open(filename) as config:
            lines = config.readlines()
    except FileNotFoundError:
        continue
    except (OSError, UnicodeError):
        break
    units = {}
    for line in lines:
        match = re.match(r'^\s*(?:export\s+)?(SYSTEM_TEMPERATURE_UNIT|TEMPERATURE_UNIT)\s*=\s*(.*)$', line)
        if not match:
            continue
        try:
            tokens = shlex.split(match[2], comments=True)
        except ValueError:
            continue
        units[match[1]] = tokens[0] if len(tokens) == 1 else ''
    value = units.get('SYSTEM_TEMPERATURE_UNIT') or units.get('TEMPERATURE_UNIT') or 'C'
    break
print('F' if value.upper() == 'F' else 'C')
PY
  ) || unit="$fallback"
  case "$unit" in F|f) printf F ;; *) printf C ;; esac
}
