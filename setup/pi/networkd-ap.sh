#!/bin/bash
# Armbian Netplan/networkd only. Never rewrite the user's Netplan or station config.
set -euo pipefail
export LC_ALL=C
umask 077
CONFIG=/etc/sentryusb/networkd-ap
NETWORK=/etc/systemd/network/05-sentryusb-ap.network
UNIT=/etc/systemd/system/sentryusb-networkd-ap.service
SERVICE=sentryusb-networkd-ap.service
RUNTIME=/run/sentryusb-networkd-ap
OWNER=/run/sentryusb-networkd-ap-owner
RESURRECTOR=/etc/systemd/system/sentryusb-ap-resurrect.service.d/20-networkd-ap.conf

# Bound child commands as well as the API's overall operation deadline, so a
# killed helper cannot leave a late systemctl job racing the recovery helper.
systemctl() { command timeout 10 systemctl "$@"; }
networkctl() { command timeout 10 networkctl "$@"; }
iw() { command timeout 5 iw "$@"; }

fail() { echo "Armbian hotspot: $*" >&2; exit 2; }
valid_interface() { [[ "$1" =~ ^[a-zA-Z0-9_.-]{1,15}$ && "$1" != ap0 ]]; }

detect() {
  [ -f /etc/armbian-release ] || return 1
  if systemctl is-active --quiet NetworkManager.service || systemctl is-enabled --quiet NetworkManager.service; then
    return 1
  fi
  if ! systemctl is-active --quiet systemd-networkd.service && ! systemctl is-enabled --quiet systemd-networkd.service; then
    return 1
  fi
  local units candidate selected='' count=0 saved=''
  [ ! -f "$CONFIG/station" ] || saved=$(cat "$CONFIG/station")
  units=$(systemctl list-unit-files --no-legend --no-pager 'netplan-wpa-*.service') || fail 'could not identify the Netplan WiFi service'
  while read -r candidate _; do
    [[ "$candidate" == netplan-wpa-*.service ]] || continue
    candidate=${candidate#netplan-wpa-}; candidate=${candidate%.service}
    valid_interface "$candidate" || continue
    if [ "$candidate" = "$saved" ]; then printf '%s\n' "$candidate"; return 0; fi
    selected=$candidate
    count=$((count + 1))
  done <<< "$units"
  [ "$count" -ne 0 ] || return 1
  [ "$count" -eq 1 ] || fail 'multiple Netplan WiFi interfaces; cannot safely choose the hotspot radio'
  printf '%s\n' "$selected"
}

read_profile() {
  [ -f "$CONFIG/station" ] && [ -f "$CONFIG/ssid" ] && [ -f "$CONFIG/ip" ] &&
    [ -s "$CONFIG/hostapd.conf" ] && [ -s "$NETWORK" ] && [ -s "$UNIT" ] || fail 'AP setup is incomplete; apply the WiFi Access Point settings in Setup Wizard'
  station=$(cat "$CONFIG/station")
  valid_interface "$station" || fail 'invalid saved station interface'
  ssid=$(cat "$CONFIG/ssid")
  address=$(cat "$CONFIG/ip")
  validate_address "$address"
}

validate_address() {
  local a b c d extra
  [[ "$1" =~ ^[0-9]+\.[0-9]+\.[0-9]+\.[0-9]+$ ]] || fail 'AP IP must be an IPv4 address'
  IFS=. read -r a b c d extra <<< "$1"
  [ -z "${extra:-}" ] || fail 'AP IP must be an IPv4 address'
  for octet in "${a:-}" "${b:-}" "${c:-}" "${d:-}"; do
    [[ "$octet" =~ ^(0|[1-9][0-9]{0,2})$ ]] && [ "$octet" -le 255 ] || fail 'AP IP must be an IPv4 address'
  done
  [ "$a" -gt 0 ] && [ "$a" -lt 224 ] && [ "$a" -ne 127 ] && [ "$d" -gt 0 ] && [ "$d" -lt 255 ] || fail 'AP IP must be a usable unicast address in its /24 subnet'
}

configure() {
  local station=$1 ssid=${AP_SSID:-} pass=${AP_PASS:-} address=${AP_IP:-192.168.66.1} hex hostapd_bin security
  [ ! -e "$OWNER" ] || fail 'disable Away Mode before changing its hotspot settings'
  [ "${#ssid}" -ge 1 ] && [ "${#ssid}" -le 32 ] && [[ "$ssid" != *$'\n'* && "$ssid" != *$'\r'* ]] || fail 'AP SSID must be 1–32 bytes without line breaks'
  if [[ "$pass" =~ ^[[:xdigit:]]{64}$ ]]; then
    security="wpa_psk=$pass"
  else
    [ "${#pass}" -ge 8 ] && [ "${#pass}" -le 63 ] && [[ "$pass" != *$'\n'* && "$pass" != *$'\r'* ]] || fail 'AP password must be 8–63 bytes without line breaks, or a 64-digit hexadecimal key'
    security="wpa_passphrase=$pass"
  fi
  validate_address "$address"
  hostapd_bin=$(command -v hostapd) || fail 'hostapd is not installed'
  command -v hostapd_cli >/dev/null || fail 'hostapd_cli is not installed'
  command -v iw >/dev/null || fail 'iw is not installed'
  mkdir -p "$CONFIG" /etc/systemd/network /etc/systemd/system
  rm -f "$CONFIG/station"
  hex=$(printf '%s' "$ssid" | od -An -v -tx1 | tr -d ' \n')
  cat > "$CONFIG/hostapd.conf" <<EOF
interface=ap0
driver=nl80211
ctrl_interface=$RUNTIME
ssid2=$hex
hw_mode=g
channel=1
ieee80211n=1
wmm_enabled=1
auth_algs=1
wpa=2
wpa_key_mgmt=WPA-PSK
rsn_pairwise=CCMP
$security
EOF
  cat > "$NETWORK" <<EOF
# Managed by SentryUSB for Armbian Netplan Away Mode; matches only ap0.
[Match]
Name=ap0
[Link]
RequiredForOnline=no
[Network]
Address=$address/24
DHCP=no
DHCPServer=yes
LinkLocalAddressing=no
IPv6AcceptRA=no
ConfigureWithoutCarrier=yes
[DHCPServer]
PoolOffset=10
PoolSize=200
EmitDNS=no
EmitRouter=no
EOF
  cat > "$UNIT" <<EOF
[Unit]
Description=SentryUSB Armbian hotspot
Requires=systemd-networkd.service
After=systemd-networkd.service sys-subsystem-net-devices-ap0.device
BindsTo=sys-subsystem-net-devices-ap0.device
[Service]
Type=simple
RuntimeDirectory=sentryusb-networkd-ap
RuntimeDirectoryMode=0700
ExecStart=$hostapd_bin $CONFIG/hostapd.conf
Restart=no
EOF
  mkdir -p "$(dirname "$RESURRECTOR")"
  printf '[Unit]\nConditionPathExists=!%s/station\n' "$CONFIG" > "$RESURRECTOR"
  printf '%s\n' "$ssid" > "$CONFIG/ssid"
  printf '%s\n' "$address" > "$CONFIG/ip"
  chmod 0600 "$CONFIG/hostapd.conf" "$CONFIG/ssid" "$CONFIG/ip"
  chmod 0644 "$NETWORK" "$UNIT"
  systemctl daemon-reload
  networkctl reload
  # Commit the backend marker last; incomplete installation cannot look ready.
  printf '%s\n' "$station" > "$CONFIG/station.tmp"
  mv "$CONFIG/station.tmp" "$CONFIG/station"
  # The old ifupdown watchdog must never touch this backend's interface.
  systemctl stop sentryusb-ap-resurrect.service 2>/dev/null || true
}

stop_ap() {
  [ -f "$OWNER" ] || return 0
  local station resume rc=0
  read -r station resume < "$OWNER"
  valid_interface "$station" && [[ "$resume" == 0 || "$resume" == 1 ]] || fail 'invalid hotspot recovery state'
  systemctl stop "$SERVICE" || rc=1
  if iw dev ap0 info >/dev/null 2>&1; then iw dev ap0 del || rc=1; fi
  networkctl up "$station" || rc=1
  if [ "$resume" = 1 ]; then systemctl start "netplan-wpa-$station.service" || rc=1; fi
  networkctl reconfigure "$station" || rc=1
  if [ "$rc" = 0 ]; then rm -f "$OWNER"; else echo 'Armbian hotspot: could not fully restore the Netplan WiFi connection' >&2; fi
  return "$rc"
}

ready() {
  local status
  status=$(timeout 3 hostapd_cli -p "$RUNTIME" -i ap0 status 2>/dev/null) || return 1
  grep -qx 'state=ENABLED' <<< "$status" || return 1
  ip -4 -o address show dev ap0 | awk -v address="$address/24" '$4 == address { found=1 } END { exit !found }'
}

start_ap() {
  read_profile
  if [ -f "$OWNER" ]; then
    if ready; then return 0; fi
    stop_ap || fail 'previous hotspot cleanup must finish before retrying'
  fi
  # Do not commandeer an interface created by another service.
  if iw dev ap0 info >/dev/null 2>&1; then fail 'ap0 already exists outside this hotspot session'; fi
  iw dev "$station" info >/dev/null || fail "WiFi interface $station is unavailable"
  local resume=0
  if systemctl is-active --quiet "netplan-wpa-$station.service" || systemctl is-enabled --quiet "netplan-wpa-$station.service"; then resume=1; fi
  printf '%s %s\n' "$station" "$resume" > "$OWNER.tmp"
  mv "$OWNER.tmp" "$OWNER"
  # Free the shared radio and Netplan's P2P device before creating the AP.
  # The existing station config stays untouched and is restored on every exit.
  trap 'rc=$?; if [ "$rc" -ne 0 ]; then stop_ap || true; fi; exit "$rc"' EXIT
  systemctl stop "netplan-wpa-$station.service"
  networkctl down "$station"
  iw dev "$station" interface add ap0 type __ap
  networkctl reload
  # networkd may still be discovering the newly created virtual interface.
  local attempt configured=0
  for attempt in {1..20}; do
    if networkctl reconfigure ap0; then configured=1; break; fi
    sleep 0.25
  done
  [ "$configured" = 1 ] || fail 'networkd could not configure ap0'
  if ! systemctl start "$SERVICE"; then
    journalctl -u "$SERVICE" -n 8 --no-pager >&2 || true
    fail 'hostapd service failed to start'
  fi
  for attempt in {1..40}; do
    if ready; then trap - EXIT; return 0; fi
    sleep 0.25
  done
  journalctl -u "$SERVICE" -n 8 --no-pager >&2 || true
  fail 'hotspot failed to become ready (hostapd ENABLED and AP address required)'
}

action=${1:-detect}
# Even a leftover profile never opts non-Armbian or NM-managed machines in.
station=$(detect) || exit $?
if [[ "$action" != detect && "$action" != info && "$action" != check ]]; then
  exec 9>/run/sentryusb-networkd-ap.lock
  flock -w 30 9 || fail 'another hotspot operation is still running'
fi
case "$action" in
  detect) printf '%s\n' "$station" ;;
  configure) configure "$station" ;;
  info) read_profile; printf '%s\n%s\n' "$ssid" "$address" ;;
  check) read_profile; [ -f "$OWNER" ] && ready || fail 'the active hotspot is no longer ready' ;;
  start) start_ap ;;
  stop) stop_ap ;;
  remove)
    stop_ap
    rm -f "$CONFIG/station" "$CONFIG/ssid" "$CONFIG/ip" "$CONFIG/hostapd.conf" "$NETWORK" "$UNIT" "$RESURRECTOR"
    rmdir "$CONFIG" 2>/dev/null || true
    systemctl daemon-reload
    networkctl reload
    ;;
  *) fail "unknown operation: $action" ;;
esac
