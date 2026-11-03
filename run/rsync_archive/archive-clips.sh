#!/bin/bash -eu

# Connection monitor: poll the archive server every ~10s. Five
# consecutive misses kill rsync (and this script) so archiveloop can
# reach `connect_usb_drives_to_host` and put the gadget back online
# instead of hanging on a dropped SSH socket while the user drives away.
# rsync's `--timeout=600` only fires on socket-idle, not on a quietly-
# dropping link, so a bash-level monitor is the only way to bound the
# hang from outside the rsync process.
#
# Travel Mode (passed fresh by archiveloop as TRAVEL_MODE_ACTIVE) relaxes the
# thresholds below for slow, high-latency VPN links so a still-progressing
# transfer isn't killed by a brief mobile-link hiccup. Normal mode keeps the
# original snappy values byte-for-byte so "drive away from home" recovery is
# unchanged.
if [ "${TRAVEL_MODE_ACTIVE:-0}" = "1" ]; then
  MONITOR_MISSES=20            # ~minutes of sustained loss before giving up
  MONITOR_TIMEOUT=20           # must exceed the patient probe (ping 4 + ssh 8 ~= 12s)
  export ARCHIVE_PING_TIMEOUT=4 ARCHIVE_SSH_TIMEOUT=8
  RSYNC_EXTRA=(--partial)      # resume an interrupted large clip next cycle
else
  MONITOR_MISSES=5             # unchanged
  MONITOR_TIMEOUT=6            # unchanged
  # --partial in normal mode too: with the archive reads now running at low
  # I/O priority behind the car's writes, rsync's own --timeout=600 can
  # abort a slowed-but-wanted transfer; without --partial the in-flight
  # clip would restart from byte 0 next cycle, re-reading gigabytes against
  # the same contended disk.
  RSYNC_EXTRA=(--partial)
fi

# Optional non-standard SSH port (RSYNC_SSH_PORT in sentryusb.conf). It has
# to go through rsync's remote-shell override: `rsync -p` means "preserve
# permissions", not "port". Unset adds nothing, so the command line is
# unchanged for servers on the default port.
RSYNC_SSH_ARGS=()
if [ "${TRAVEL_MODE_ACTIVE:-0}" != "1" ] && [ -z "${RSYNC_RSH:-}" ]; then
  # Home mode: SSH keepalives notice a session that stopped answering in
  # ~45 s. A Wi-Fi link can stay associated and pass pings while one traffic
  # class is stuck, so the reachability monitor above never fires and only
  # rsync's 10-minute --timeout would end the hang. Travel Mode keeps its
  # patient settings for slow VPN links, and a custom RSYNC_RSH is left
  # exactly as it was.
  RSYNC_SSH_ARGS=(-e "ssh${RSYNC_SSH_PORT:+ -p ${RSYNC_SSH_PORT}} -o ServerAliveInterval=15 -o ServerAliveCountMax=3 -o ConnectTimeout=10")
elif [ -n "${RSYNC_SSH_PORT:-}" ]; then
  RSYNC_SSH_ARGS=(-e "ssh -p ${RSYNC_SSH_PORT}")
fi
RSYNC_STDERR=/tmp/archive-rsync-stderr.log

# Exit 75 tells archiveloop the transfer died on a stalled connection
# (rsync's own I/O timeout, or SSH reporting a timeout in this run's
# stderr), which is worth a transport check. Auth, permission and disk
# errors stay exit 1.
function rsync_transport_timeout {
  local rc=$1 errfile=$2
  [ "$rc" = 30 ] && return 0
  case "$rc" in 12|255) ;; *) return 1 ;; esac
  # Keep in step with archive_ssh_probe in archiveloop.
  LC_ALL=C grep -Eq '^(Timeout, server [^ ]+ not responding\.?|Read from remote host [^ ]+: Connection timed out|ssh: connect to host [^ ]+ port [0-9]+: Connection timed out|Connection timed out during banner exchange|Connection to [^ ]+ port [0-9]+ timed out)'$'\r''?$' "$errfile" 2>/dev/null
}

function connectionmonitor {
  while true
  do
    for (( i = 1; i <= MONITOR_MISSES; i++ ))
    do
      if timeout "$MONITOR_TIMEOUT" /root/bin/archive-is-reachable.sh "$ARCHIVE_SERVER"
      then
        sleep 5
        continue 2
      fi
      sleep 1
    done
    log "connection dead, killing archive-clips"
    # Give rsync a chance to delete the source files it already copied
    # before we kill it hard.
    killall rsync || true
    sleep 2
    killall -9 rsync || true
    kill -9 "$1" || true
    return
  done
}

connectionmonitor $$ &

while [ -n "${1+x}" ]
do
  # Low I/O + CPU priority: the archive reads tens of GB from the same disk
  # the car is writing dashcam footage to through the USB gadget. At default
  # priority those reads compete head-to-head with the car's writes and can
  # stall them past the car's SCSI timeout (it drops the drive with an X).
  # Best-effort lowest (-c2 -n7), NOT idle (-c3): under bfq the idle class is
  # only serviced when the disk is otherwise quiet, so continuous sentry
  # writes could stall the archive indefinitely and freespacemanager would
  # end up purging footage that was never archived. -c2 -n7 keeps the car's
  # default-priority writes winning while guaranteeing the archive makes
  # progress. Needs the bfq scheduler to have effect (udev rule ships it).
  # stderr is kept apart so the timeout check only sees this run's SSH and
  # rsync messages, never clip names from the transfer listing.
  rsync_rc=0
  ionice -c2 -n7 nice -n19 rsync -avhRL --timeout=600 --remove-source-files --no-perms --omit-dir-times \
        ${RSYNC_EXTRA[@]+"${RSYNC_EXTRA[@]}"} ${RSYNC_SSH_ARGS[@]+"${RSYNC_SSH_ARGS[@]}"} \
        --stats --log-file=/tmp/archive-rsync-cmd.log --ignore-missing-args \
        --files-from="$2" "$1" "$RSYNC_USER@$RSYNC_SERVER:$RSYNC_PATH" > /tmp/rsynclog 2> "$RSYNC_STDERR" || rsync_rc=$?
  cat "$RSYNC_STDERR" >> /tmp/rsynclog 2>/dev/null || true
  if [ "$rsync_rc" != 0 ] && [ "$rsync_rc" != 24 ]
  then
    cat /tmp/archive-rsync-cmd.log /tmp/rsynclog > /tmp/archive-error.log
    kill %1 || true
    if rsync_transport_timeout "$rsync_rc" "$RSYNC_STDERR"
    then
      log "rsync stopped on a connection timeout (rsync exit $rsync_rc)"
      exit 75
    fi
    exit 1
  fi
  shift 2
done

# Stop the monitor.
kill %1 || true
