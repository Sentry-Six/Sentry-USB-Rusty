#!/bin/bash
# Regression coverage for reinstalling onto an already read-only SentryUSB.
set -euo pipefail

script=${1:-install-pi.sh}
script=$(cd "$(dirname "$script")" && pwd)/$(basename "$script")
tmp=$(mktemp -d)
trap 'rm -rf "$tmp"' EXIT

# curl | bash ignores the shebang, so strict mode must be enabled by the body.
grep -q '^set -Eeuo pipefail$' "$script"

# Install the EXIT warning before the first remount attempt: root may become RW
# and a later boot remount can still fail inside the same function.
trap_line=$(grep -n "^trap 'readonly_reinstall_exit_notice" "$script" | cut -d: -f1)
remount_line=$(grep -n '^ensure_install_filesystems_writable$' "$script" | cut -d: -f1)
[ -n "$trap_line" ] && [ -n "$remount_line" ] && [ "$trap_line" -lt "$remount_line" ]

# Functions and variables below are consumed by functions loaded via eval.
# shellcheck disable=SC2034,SC2329
load_installer_functions() {
  REPO=Sentry-Six/Sentry-USB-Rusty
  INSTALL_DIR="${SENTRYUSB_INSTALL_DIR:-/opt/sentryusb}"
  REMOUNT_HELPER="${SENTRYUSB_REMOUNT_HELPER:-/root/bin/remountfs_rw}"
  BINARY_NAME=sentryusb
  RELEASE_TAG=v-test
  RED='' GREEN='' BLUE='' YELLOW='' NC=''
  info() { echo "[INFO] $1"; }
  ok() { echo "[OK] $1"; }
  warn() { echo "[WARN] $1"; }
  error_exit() { echo "[ERROR] $1"; exit 1; }
  eval "$(awk '/^mount_is_writable\(\)/,/^}/' "$script")"
  eval "$(awk '/^ensure_install_filesystems_writable\(\)/,/^}/' "$script")"
  eval "$(awk '/^install_release_asset\(\)/,/^}/' "$script")"
  eval "$(awk '/^install_release_bundle\(\)/,/^}/' "$script")"
  eval "$(awk '/^readonly_reinstall_exit_notice\(\)/,/^}/' "$script")"
}

# A failed final move must never print OK or return success.
set +e
# shellcheck disable=SC2030
(
  export SENTRYUSB_INSTALL_DIR="$tmp/install-fail"
  load_installer_functions
  mkdir -p "$INSTALL_DIR"
  # Invoked indirectly by install_release_asset loaded via eval.
  # shellcheck disable=SC2329
  curl() {
    local output=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = -o ]; then output="$2"; shift 2; else shift; fi
    done
    printf 'downloaded payload\n' > "$output"
  }
  # shellcheck disable=SC2329
  mv() { printf 'attempt\n' >> "$tmp/mv-attempts"; return 1; }
  # shellcheck disable=SC2329
  sleep() { :; }
  install_release_asset sentryusb linux-arm64-a72
) >"$tmp/fail.out" 2>&1
status=$?
set -e
[ "$status" -ne 0 ] || { echo "failed move returned success" >&2; exit 1; }
[ "$(wc -l < "$tmp/mv-attempts")" -eq 5 ] || { echo "failed move was not retried" >&2; exit 1; }
! grep -q 'Downloaded sentryusb-linux-arm64-a72' "$tmp/fail.out" \
  || { echo "failed move printed a false OK" >&2; exit 1; }

# The happy path publishes an executable from a same-filesystem staging file.
# shellcheck disable=SC2031
(
  export SENTRYUSB_INSTALL_DIR="$tmp/install-ok"
  load_installer_functions
  mkdir -p "$INSTALL_DIR"
  # Invoked indirectly by install_release_asset loaded via eval.
  # shellcheck disable=SC2329
  curl() {
    local output=""
    while [ "$#" -gt 0 ]; do
      if [ "$1" = -o ]; then output="$2"; shift 2; else shift; fi
    done
    printf '#!/bin/sh\nexit 0\n' > "$output"
  }
  # shellcheck disable=SC2329
  sleep() { :; }
  install_release_asset sentryusb linux-arm64-a72
  [ -x "$INSTALL_DIR/sentryusb-linux-arm64-a72" ]
  [ ! -e "$INSTALL_DIR/.sentryusb-linux-arm64-a72.new" ]
) >"$tmp/success.out" 2>&1
grep -q 'Downloaded sentryusb-linux-arm64-a72' "$tmp/success.out"

# If remounting fails, stop before claiming that installation can proceed.
set +e
(
  export SENTRYUSB_REMOUNT_HELPER="$tmp/missing-remount-helper"
  load_installer_functions
  # Invoked indirectly by ensure_install_filesystems_writable loaded via eval.
  # shellcheck disable=SC2329
  findmnt() {
    if [ "${2:-}" = TARGET ]; then printf '%s\n' "${3:-/}"; else printf 'ro,noatime\n'; fi
  }
  # shellcheck disable=SC2329
  mount() { return 1; }
  ensure_install_filesystems_writable
) >"$tmp/remount.out" 2>&1
status=$?
set -e
[ "$status" -ne 0 ] || { echo "read-only remount failure returned success" >&2; exit 1; }
grep -q 'could not be remounted read-write' "$tmp/remount.out"

# Release installs must not mix latest binaries with scripts from moving main.
# shellcheck disable=SC2016
grep -q 'SOURCE_REF="${RELEASE_TAG:-main}"' "$script"
# shellcheck disable=SC2016
if grep -q 'raw.githubusercontent.com/${REPO}/main/' "$script"; then
  echo "release-managed scripts still bypass SOURCE_REF" >&2
  exit 1
fi

# A release is a protocol-compatible bundle: every CPU variant gets the main
# daemon and both native BLE helpers from the same pinned tag.
(
  load_installer_functions
  # Consumed by functions loaded dynamically above.
  # shellcheck disable=SC2034
  SUFFIXES='linux-arm64-a53 linux-arm64-a72 linux-arm64-a76'
  RELEASE_TAG=v9.9.9
  # Invoked indirectly by install_release_bundle loaded via eval.
  # shellcheck disable=SC2329
  install_release_asset() { printf '%s %s %s\n' "$RELEASE_TAG" "$1" "$2"; }
  install_release_bundle
) >"$tmp/bundle.out"
[ "$(wc -l < "$tmp/bundle.out")" -eq 9 ]
for suffix in linux-arm64-a53 linux-arm64-a72 linux-arm64-a76; do
  grep -qx "v9.9.9 sentryusb $suffix" "$tmp/bundle.out"
  grep -qx "v9.9.9 sentryusb-tesla-telemetry $suffix" "$tmp/bundle.out"
  grep -qx "v9.9.9 sentryusb-ble-action $suffix" "$tmp/bundle.out"
done

# Failure after remounting a read-only install must leave an explicit warning;
# otherwise the appliance silently remains writable until reboot.
(
  load_installer_functions
  # Consumed by readonly_reinstall_exit_notice loaded via eval.
  # shellcheck disable=SC2034
  ROOT_WAS_READONLY=1
  # shellcheck disable=SC2034
  BOOT_WAS_READONLY=0
  readonly_reinstall_exit_notice 1
) >"$tmp/exit-notice.out" 2>&1
grep -q 'installation failed after remounting' "$tmp/exit-notice.out"

# Execute the real optional-advertising section under errexit, with filesystem
# and service commands stubbed so failures cannot alter the host installation.
awk '/^info "Installing BLE advertising selector/{keep=1} /^#.*Step 4: Sample Config/{exit} keep' \
  "$script" > "$tmp/advertising-section.sh"
for failed in select-ble-adv-mode.sh sentryusb-ble-adv.sh ble-native-manifest \
    sentryusb-ble-mode.service sentryusb-ble-adv.service 99-sentryusb-ble-hci.rules \
    sentryusb-ble-wants-bluetooth.conf local-install mkdir chmod none; do
  mkdir -p "$tmp/local/setup/pi"
  rm -f "$tmp/local/setup/pi/ble-native-manifest"
  if [ "$failed" = local-install ]; then
    printf manifest > "$tmp/local/setup/pi/ble-native-manifest"
  fi
  : > "$tmp/service-calls"
  set +e
  (
    set -e
    load_installer_functions
    # Used by the sourced installer section below.
    # shellcheck disable=SC2034
    SOURCE_REF=v-test
    set -- "$tmp/local/sentryusb"
    # These commands are invoked by the sourced installer section.
    # shellcheck disable=SC2329
    mkdir() { [ "$failed" != mkdir ]; }
    # shellcheck disable=SC2329
    chmod() { [ "$failed" != chmod ]; }
    # shellcheck disable=SC2329
    rm() { :; }
    # shellcheck disable=SC2329
    install() { return 1; }
    # shellcheck disable=SC2329
    systemctl() { printf '%s\n' "$*" >> "$tmp/service-calls"; }
    # shellcheck disable=SC2329
    udevadm() { :; }
    # shellcheck disable=SC2329
    curl() {
      local arg
      for arg in "$@"; do
        if [[ "$arg" == https://* && "${arg##*/}" == "$failed" ]]; then return 22; fi
      done
      return 0
    }
    # shellcheck disable=SC1091
    source "$tmp/advertising-section.sh"
    echo 'installer continued'
  ) > "$tmp/advertising-$failed.out" 2>&1
  status=$?
  set -e
  [ "$status" -eq 0 ] || { cat "$tmp/advertising-$failed.out"; exit 1; }
  grep -q 'installer continued' "$tmp/advertising-$failed.out"
  if [ "$failed" = none ]; then
    grep -q 'selector + helper installed' "$tmp/advertising-$failed.out"
    grep -q '^enable sentryusb-ble-adv.service$' "$tmp/service-calls"
  else
    grep -q 'installation incomplete' "$tmp/advertising-$failed.out"
    if grep -q 'selector + helper installed' "$tmp/advertising-$failed.out" || [ -s "$tmp/service-calls" ]; then
      echo "incomplete advertising install reported success: $failed" >&2
      exit 1
    fi
  fi
done

# Generic firmware discovered by find must still produce valid relative links.
awk '/^    HCD=""/{keep=1} /^    # No WiFi NVRAM relink/{exit} keep' \
  "$script" > "$tmp/firmware-section.sh"
mkdir -p "$tmp/firmware"
printf firmware > "$tmp/firmware/BCM4345C0.vendor_generic.hcd"
(
  load_installer_functions
  # Used by the sourced firmware-selection section.
  # shellcheck disable=SC2034
  BRCM="$tmp/firmware"
  # shellcheck disable=SC1091
  source "$tmp/firmware-section.sh"
) > "$tmp/firmware.out"
for link in BCM4345C0.radxa,rock-4c-plus.hcd BCM4345C0.hcd; do
  [ -f "$tmp/firmware/$link" ]
  [ "$(readlink "$tmp/firmware/$link")" = BCM4345C0.vendor_generic.hcd ]
done

echo "install-pi safety tests passed"
