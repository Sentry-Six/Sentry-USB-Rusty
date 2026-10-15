#!/usr/bin/env bash
# Exercise the real shell setup function without touching host keys or packages.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export BLE_SETUP_TEST_DIR="$work"

python3 - "$root/setup/pi/configure.sh" "$work" <<'PY'
import pathlib, re, sys
source = pathlib.Path(sys.argv[1]).read_text()
work = pathlib.Path(sys.argv[2])
function = re.search(r'^function check_and_configure_tesla_ble \(\) \{.*?^}', source, re.M | re.S)
assert function
isolated = function[0].replace('/root/bin/sentryusb-ble-action', str(work/'ble-action'))
isolated = isolated.replace('/root/.ble', str(work/'keys'))
(work/'setup-function.sh').write_text(isolated)
PY
# shellcheck disable=SC1091
source "$work/setup-function.sh"

# Called by the extracted setup function.
# shellcheck disable=SC2329
check_variable() { :; }
# shellcheck disable=SC2329
log_progress() { printf '%s\n' "$*"; }
# shellcheck disable=SC2329
dpkg-query() { printf 'installed\n'; }
# shellcheck disable=SC2329
apt-cache() { :; }
# shellcheck disable=SC2329
timeout() {
  shift
  if [[ $BLE_SETUP_TEST_MODE == unreachable ]]; then return 124; fi
  "$@"
}

cat > "$work/ble-action" <<'SH'
#!/usr/bin/env bash
set -euo pipefail
printf '%s\n' "$1" >> "$BLE_SETUP_TEST_DIR/calls"
case "$1" in
  keygen)
    [[ $BLE_SETUP_TEST_MODE != keygen-failure ]] || exit 2
    if [[ $BLE_SETUP_TEST_MODE == empty || $BLE_SETUP_TEST_MODE == corrupt-private ]]; then
      printf 'valid-private' > "$BLE_SETUP_TEST_DIR/keys/key_private.pem"
      touch "$BLE_SETUP_TEST_DIR/keys/key_pending_pairing"
    fi
    printf 'matching-public' > "$BLE_SETUP_TEST_DIR/keys/key_public.pem"
    ;;
  session-info)
    [[ $(head -1 "$BLE_SETUP_TEST_DIR/calls") == keygen ]]
    [[ $(cat "$BLE_SETUP_TEST_DIR/keys/key_private.pem") == valid-private ]]
    [[ $(cat "$BLE_SETUP_TEST_DIR/keys/key_public.pem") == matching-public ]]
    printf 'PAIRED\n'
    ;;
  *) exit 3 ;;
esac
SH
chmod +x "$work/ble-action"
export TESLA_BLE_VIN=test-vin SENTRY_CASE=1

for mode in empty corrupt-private corrupt-public mismatched-public paired keygen-failure unreachable pending; do
  export BLE_SETUP_TEST_MODE="$mode"
  rm -rf "$work/keys"
  mkdir -p "$work/keys"
  : > "$work/calls"
  printf 'valid-private' > "$work/keys/key_private.pem"
  printf 'matching-public' > "$work/keys/key_public.pem"
  case "$mode" in
    empty) : > "$work/keys/key_private.pem"; : > "$work/keys/key_public.pem" ;;
    corrupt-private) printf 'truncated-private' > "$work/keys/key_private.pem" ;;
    corrupt-public) printf 'truncated-public' > "$work/keys/key_public.pem" ;;
    mismatched-public) printf 'another-keys-public-half' > "$work/keys/key_public.pem" ;;
    pending) touch "$work/keys/key_pending_pairing" ;;
  esac

  check_and_configure_tesla_ble "$work" > "$work/$mode.out"
  [[ $(head -1 "$work/calls") == keygen ]]
  case "$mode" in
    empty|corrupt-private|pending)
      [[ $(cat "$work/calls") == keygen ]]
      grep -q 'Please visit the web UI' "$work/$mode.out"
      [[ -f "$work/keys/key_pending_pairing" ]]
      ;;
    keygen-failure)
      [[ $(cat "$work/calls") == keygen ]]
      grep -q 'keygen failed (exit 2)' "$work/$mode.out"
      ;;
    unreachable)
      grep -q 'car is not reachable' "$work/$mode.out"
      ;;
    *)
      [[ $(cat "$work/calls") == $'keygen\nsession-info' ]]
      grep -q 'keys exist and are paired' "$work/$mode.out"
      [[ $(cat "$work/keys/key_private.pem") == valid-private ]]
      ;;
  esac
done

rm "$work/ble-action"
: > "$work/calls"
check_and_configure_tesla_ble "$work" > "$work/missing.out"
[[ ! -s "$work/calls" ]]
grep -q 'not found' "$work/missing.out"
echo 'BLE setup recovery tests passed'
