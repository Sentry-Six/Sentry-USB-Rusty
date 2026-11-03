#!/usr/bin/env bash
# A Wi-Fi link can stay associated and pass pings while one traffic class is
# stuck (2026-11-02: rsync and non-interactive SSH hung, ping and interactive
# SSH worked, a Wi-Fi re-association fixed it). Exercise the pieces that
# detect that, reconnect Wi-Fi only for it, and keep the car's drive alone.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT

fail() { echo "FAIL: $*"; exit 1; }

# --- 1. Timeout classification in the rsync backend -------------------------
eval "$(awk '/^function rsync_transport_timeout/ {keep=1} keep {print} keep && /^}/ {exit}' \
  "$root/run/rsync_archive/archive-clips.sh")"
classify() {  # classify RC STDERR-TEXT -> prints yes/no
  printf '%s\n' "$2" > "$work/err"
  if rsync_transport_timeout "$1" "$work/err"; then echo yes; else echo no; fi
}
[[ $(classify 30 '') == yes ]] || fail 'rsync io timeout (30)'
[[ $(classify 255 'Timeout, server nas.local not responding.') == yes ]] || fail 'ssh keepalive timeout'
[[ $(classify 12 'Read from remote host 192.168.1.175: Connection timed out') == yes ]] || fail 'read timeout'
[[ $(classify 255 'ssh: connect to host nas port 22: Connection timed out') == yes ]] || fail 'connect timeout'
[[ $(classify 255 $'Timeout, server nas not responding.\r') == yes ]] || fail 'CRLF stderr'
[[ $(classify 255 'Connection timed out during banner exchange') == yes ]] || fail 'banner timeout'
[[ $(classify 255 'Connection to 192.168.1.175 port 22 timed out') == yes ]] || fail 'connection timed out form'
[[ $(classify 255 'root@nas: Permission denied (publickey).') == no ]] || fail 'auth failure'
[[ $(classify 255 'ssh: connect to host nas port 22: Connection refused') == no ]] || fail 'refused'
[[ $(classify 1 'Timeout, server nas not responding.') == no ]] || fail 'unrelated exit code'
[[ $(classify 23 'rsync: link_stat "/x/Read from remote host a: Connection timed out" failed') == no ]] \
  || fail 'clip name containing the phrase'
[[ $(classify 255 'rsync: link_stat "/Timeout, server x not responding" failed') == no ]] \
  || fail 'unanchored match'

# --- 2. The backend's exit codes and SSH options -----------------------------
mkdir -p "$work/bin" "$work/src"
cat > "$work/bin/rsync" <<'EOF'
#!/bin/bash
printf '%s\n' "$@" > "$FAKE_RSYNC_ARGS"
printf 'RecentClips/clip.mp4\n'
[ -z "${FAKE_RSYNC_STDERR:-}" ] || printf '%s\n' "$FAKE_RSYNC_STDERR" >&2
exit "${FAKE_RSYNC_RC:-0}"
EOF
chmod +x "$work/bin/rsync"
sed -e "s@/tmp/@$work/@g" -e "s@/root/bin/@$work/bin/@g" \
  "$root/run/rsync_archive/archive-clips.sh" | tr -d '\r' > "$work/archive-clips.sh"
: > "$work/files"
log() { printf '%s\n' "$*" >> "$work/backend.log"; }
export -f log
export work
run_backend() {  # run_backend RC STDERR TRAVEL [PORT] [RSYNC_RSH] -> sets backend_rc
  backend_rc=0
  env PATH="$work/bin:$PATH" FAKE_RSYNC_ARGS="$work/args" FAKE_RSYNC_RC="$1" FAKE_RSYNC_STDERR="$2" \
    TRAVEL_MODE_ACTIVE="$3" RSYNC_SSH_PORT="${4:-}" RSYNC_RSH="${5:-}" RSYNC_USER=u RSYNC_SERVER=nas RSYNC_PATH=/dst \
    ARCHIVE_SERVER=nas bash "$work/archive-clips.sh" "$work/src" "$work/files" >/dev/null 2>&1 || backend_rc=$?
}
run_backend 0 '' 0
[[ $backend_rc == 0 ]] || fail "success exited $backend_rc"
grep -qx -- '-e' "$work/args" || fail 'home mode passes a remote shell'
grep -qx -- 'ssh -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ConnectTimeout=10' "$work/args" \
  || fail 'home mode keepalive options'
grep -qx -- '--timeout=600' "$work/args" || fail 'clip transfer keeps its 600 s idle timeout'
run_backend 24 'file has vanished' 0
[[ $backend_rc == 0 ]] || fail 'vanished files (24) still count as success'
run_backend 30 '[sender] io timeout after 601 seconds -- exiting' 0
[[ $backend_rc == 75 ]] || fail "io timeout exited $backend_rc, want 75"
grep -q 'connection timeout (rsync exit 30)' "$work/backend.log" || fail 'timeout is logged with the native exit'
grep -q 'io timeout after 601' "$work/rsynclog" || fail 'stderr still reaches /tmp/rsynclog for the failure report'
run_backend 255 'Timeout, server nas not responding.' 0
[[ $backend_rc == 75 ]] || fail 'keepalive timeout exited non-75'
run_backend 255 'u@nas: Permission denied (publickey).' 0
[[ $backend_rc == 1 ]] || fail 'auth failure must stay exit 1'
run_backend 0 '' 0 2222
grep -qx -- 'ssh -p 2222 -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ConnectTimeout=10' "$work/args" \
  || fail 'custom SSH port kept in home mode'
run_backend 0 '' 0 '' 'ssh -i /custom/key'
! grep -qx -- '-e' "$work/args" || fail 'a custom RSYNC_RSH is left alone (no -e override)'
run_backend 0 '' 0 2222 'ssh -i /custom/key'
grep -qx -- 'ssh -p 2222' "$work/args" || fail 'custom RSYNC_RSH plus port keeps the old -e'
run_backend 0 '' 1
! grep -qx -- '-e' "$work/args" || fail 'Travel Mode on the default port passes no remote shell (unchanged)'
run_backend 0 '' 1 2222
grep -qx -- 'ssh -p 2222' "$work/args" || fail 'Travel Mode custom port unchanged'

grep -q -- '--timeout=60' "$root/run/rsync_archive/copy-music.sh" || fail 'music has an idle timeout'
grep -q 'ServerAliveInterval=15' "$root/run/rsync_archive/copy-music.sh" || fail 'music has SSH keepalives'

# --- 3. Recovery decisions ----------------------------------------------------
eval "$(awk '/^function wifi_watchdog_init / {keep=1} /^function set_sys_param/ {exit} keep {print}' \
  "$root/run/archiveloop" | tr -d '\r')"
# Recovery runs in a subshell (it holds the shared lock), so counters live in files.
log() { printf '%s\n' "$*" >> "$work/msgs"; }
msgs() { cat "$work/msgs" 2>/dev/null || true; }
count() { if [ -f "$work/$1" ]; then wc -l < "$work/$1" | tr -d ' '; else echo 0; fi; }
LOG_FILE=$work/loop.log
WIFI_WATCHDOG_RUNTIME_DIR=$work/run
mkdir -p "$WIFI_WATCHDOG_RUNTIME_DIR"
ARCHIVE_SSH_PROBE_ERR=$work/probe.err
RSYNC_USER=u RSYNC_SERVER=nas
flock() { return 0; }
wifi_watchdog_capture() {
  echo x >> "$work/captures"
  # change_during_capture: state flips while diagnostics run.
  case "$change_during_capture" in away) away=1 ;; travel) travel=1 ;; esac
}
ping() {
  # change_on_ping=N:STATE flips STATE on the Nth ping (the last check before the reconnect).
  echo x >> "$work/pings"
  if [[ -n $change_on_ping && $(count pings) == "${change_on_ping%%:*}" ]]; then
    case "${change_on_ping#*:}" in
      cancel) cancel=1 ;; travel) travel=1 ;; away) away=1 ;; usb) usb=0 ;;
    esac
  fi
  [[ $reachable == 1 ]]
}
archive_is_reachable() { fail 'recovery must not use the SSH-fallback reachability check'; }
wifi_watchdog_uptime() { printf '%s\n' "$clock"; }
archive_cancel_requested() { [[ $cancel == 1 ]]; }
travel_mode_active() { [[ $travel == 1 ]]; }
usb_gadget_is_active() { [[ $usb == 1 ]]; }
away_mode_active() { [[ $away == 1 ]]; }
iw() { [[ $ap0 == 1 ]]; }
getent() { printf '192.168.1.175 STREAM nas\n'; }
ip() { printf '192.168.1.175 dev %s src 192.168.1.234 uid 0\n' "$route_dev"; }
timeout() {
  while [[ $1 == -* ]]; do [[ $1 == -k ]] && shift; shift; done
  shift
  "$@"
}
ssh() {
  probes=$((probes + 1))
  [[ $cancel_during_probe != 1 ]] || cancel=1
  local rc=$ssh_rc
  if [[ $probes -gt 1 && -n $ssh_rc_after ]]; then rc=$ssh_rc_after; fi
  [ -z "$ssh_err" ] || printf '%s\n' "$ssh_err" >&2
  return "$rc"
}
nmcli() {
  case "$*" in
    '-t -f DEVICE,TYPE,STATE device status') printf 'wlan0:wifi:connected\neth0:ethernet:unavailable\n' ;;
    '-g GENERAL.CON-UUID device show wlan0')
      # uuid_later: the station switches connections after the route lookup.
      if [[ -n $uuid_later && -e $work/uuid-read ]]; then printf '%s\n' "$uuid_later"; else printf '%s\n' "$U"; fi
      : > "$work/uuid-read" ;;
    *'connection up'*) printf '%s\n' "$*" >> "$work/nmcli-up" ;;
    *disconnect*) echo x >> "$work/disconnects" ;;
  esac
}
U=0b9d1d4e-6c55-4b6a-9b2e-3a1f6a2c7d11
setup() {
  ARCHIVE_RSYNC_TIMEOUT=1 ARCHIVE_SYSTEM=rsync
  cancel=0 travel=0 usb=1 reachable=1 away=0 ap0=0 route_dev=wlan0 uuid_later=''
  rm -f "$work/uuid-read"
  ssh_rc=124 ssh_rc_after='' ssh_err='' probes=0 change_during_capture='' cancel_during_probe=0 change_on_ping=''
  unset RSYNC_RSH
  clock=$((clock + 1000))
  rm -f "$work/nmcli-up" "$work/disconnects" "$work/captures" "$work/msgs" "$work/pings"
}
clock=0

setup
archive_transport_recover
[[ $(count nmcli-up) == 1 && $(count disconnects) == 0 && $(count captures) == 1 ]] \
  || fail "stalled SSH must reconnect once (ups=$(count nmcli-up))"
grep -qx -- "--wait 30 connection up uuid $U ifname wlan0" "$work/nmcli-up" || fail 'same connection, same interface'
[[ $(msgs) == *'SSH still not answering'* ]] || fail 'post-reconnect probe result logged'
setup; ssh_rc_after=0
archive_transport_recover
[[ $(count nmcli-up) == 1 && $(msgs) == *'SSH answers again'* ]] || fail 'recovered probe logged'
# The shared cooldown blocks a second reset inside ten minutes.
clock=$((clock + 300)) probes=0
rm -f "$work/nmcli-up"
archive_transport_recover
[[ $(count nmcli-up) == 0 && $(msgs) == *'less than 10 minutes'* ]] || fail 'cooldown'

for err in 'ssh: connect to host nas port 22: Connection timed out' \
           'Connection timed out during banner exchange' \
           'Connection to 192.168.1.175 port 22 timed out' \
           'Read from remote host nas: Connection timed out'; do
  setup; ssh_rc=255 ssh_err=$err
  archive_transport_recover
  [[ $(count nmcli-up) == 1 ]] || fail "SSH timeout form is eligible: $err"
done

# A reconnect that is skipped under the lock must not spend the cooldown.
setup; ap0=1
archive_transport_recover
[[ $(count nmcli-up) == 0 ]] || fail 'skipped while the hotspot is up'
ap0=0 clock=$((clock + 5))
archive_transport_recover
[[ $(count nmcli-up) == 1 ]] || fail 'a skipped attempt consumed the cooldown'

expect_no_reset() {  # expect_no_reset LABEL EXPECTED-PROBES
  archive_transport_recover
  [[ $(count nmcli-up) == 0 && $(count disconnects) == 0 ]] || fail "$1: must not reset Wi-Fi"
  [[ $probes == "$2" ]] || fail "$1: probes=$probes, want $2"
}
setup; ssh_rc=0;                                        expect_no_reset 'server answers SSH' 1
[[ $(msgs) == *'server side'* ]] || fail 'server-side stall is named'
setup; ssh_rc=255 ssh_err='u@nas: Permission denied (publickey).'; expect_no_reset 'auth failure' 1
setup; ARCHIVE_RSYNC_TIMEOUT=0;                         expect_no_reset 'non-timeout failure' 0
setup; ARCHIVE_SYSTEM=cifs;                             expect_no_reset 'other backends' 0
setup; travel=1;                                        expect_no_reset 'Travel Mode' 0
setup; cancel=1;                                        expect_no_reset 'cancellation' 0
setup; usb=0;                                           expect_no_reset 'drive not presented' 0
setup; reachable=0;                                     expect_no_reset 'server unreachable' 0
setup; route_dev=eth0;                                  expect_no_reset 'Ethernet route' 1
setup; ap0=1;                                           expect_no_reset 'Away Mode hotspot' 1
setup; away=1;                                          expect_no_reset 'Away Mode flag' 1
setup; uuid_later=11111111-2222-3333-4444-555555555555; expect_no_reset 'connection changed' 1
setup; cancel_during_probe=1;                           expect_no_reset 'cancelled during the probe' 1
setup; change_during_capture=away;                      expect_no_reset 'Away Mode during diagnostics' 1
setup; change_during_capture=travel;                    expect_no_reset 'Travel Mode during diagnostics' 1
# Pings: 1 = gate before the probe, 2 = first allowed check, 3 = final check.
for state in cancel travel away usb; do
  setup; change_on_ping=3:$state;                       expect_no_reset "$state during the final ping" 1
done
setup; export RSYNC_RSH='ssh -i /custom/key';           expect_no_reset 'custom RSYNC_RSH' 0
unset RSYNC_RSH
# A firmware operation in progress blocks recovery.
setup
mkdir -p "$work/fw"
printf '{"state":"running"}\n' > "$work/fw/state.json"
grep() { if [[ $* == *wifi-firmware/state.json* ]]; then command grep "${@/\/mutable\/wifi-firmware/$work/fw}"; else command grep "$@"; fi; }
stat() { command stat "${@/\/mutable\/wifi-firmware/$work/fw}"; }
expect_no_reset 'firmware operation' 1
unset -f grep stat

# --- 4. A failed archive leaves the car's drive alone ------------------------
eval "$(awk '/^function ensure_usb_drives_connected_locked/ {keep=1} keep {print} keep && /^}/ {exit}' \
  "$root/run/archiveloop" | tr -d '\r')"
connect_usb_drives_to_host_locked() { connects=$((connects + 1)); }
cam_user_media_may_need_sync() { return 0; }
cancel=0
for case in '1 1 0' '1 0 1' '0 1 1'; do
  read -r failed usb want <<< "$case"
  ARCHIVE_CYCLE_FAILED=$failed connects=0
  ensure_usb_drives_connected_locked
  [[ $connects == "$want" ]] || fail "failed=$failed active=$usb: connects=$connects, want $want"
done

eval "$(awk '/^function archive_clips \(\)/ {keep=1} keep {print} keep && /^}/ {exit}' \
  "$root/run/archiveloop" | tr -d '\r' | sed "s@/root/bin/@$work/fakebin/@g")"
mkdir -p "$work/fakebin"
for s in send-live-activity disconnect-archive.sh; do printf '#!/bin/sh\nexit 0\n' > "$work/fakebin/$s"; chmod +x "$work/fakebin/$s"; done
archive_run_command() { ran="$ran|${1##*/}"; }
archive_is_reachable() { [[ $reachable == 1 ]]; }
has_cam_disk() { return 0; }
archive_teslacam_clips() { return "$clips_rc"; }
clear_archive_status() { :; }
copy_music_files() { music=$((music + 1)); }
ensure_usb_drives_connected() { ensures=$((ensures + 1)); }
cloud_upload_step() { cloud=$((cloud + 1)); }
archive_transport_recover() { checks=$((checks + 1)); }
archive_cancel_cleanup() { :; }
ARCHIVE_SYSTEM=rsync MUSIC_SHARE_NAME=Music MUSIC_MOUNT=$work/music
mkdir -p "$work/music"
touch "$work/music_disk.bin"
flow() {  # flow CLIPS-RC USB-ACTIVE
  clips_rc=$1 usb=$2 travel=0 cancel=0 reachable=1
  music=0 ensures=0 cloud=0 checks=0 ran=''
  rm -f "$work/msgs"
  MUSIC_ARCHIVE_MOUNT=$work/music
  flow_rc=0
  archive_clips || flow_rc=$?
}
flow 1 1
[[ $flow_rc == 1 && $music == 0 && $ensures == 0 && $checks == 1 ]] \
  || fail "failed clips, active drive: rc=$flow_rc music=$music ensures=$ensures checks=$checks"
[[ $ran == *post-archive-process.sh* && $cloud == 1 ]] || fail 'maps and cloud upload still run after a failed archive'
[[ $(msgs) == *'Skipping music sync after failed clip archive'* ]] || fail 'music skip is logged'
flow 1 0
[[ $ensures == 1 && $checks == 1 && $music == 0 ]] || fail 'failed clips restore an inactive drive once'
flow 0 1
[[ $flow_rc == 0 && $music == 1 && $ensures == 1 && $checks == 0 ]] || fail 'successful archive keeps its old flow'

echo 'PASS: transport timeouts are classified, Wi-Fi reconnects only for a stalled link, and a failed archive keeps the car drive'
