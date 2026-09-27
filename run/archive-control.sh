#!/bin/bash
# Shared cancellation contract with crates/drives/src/archive_control.rs.
# A request belongs to ONE cycle; it is not a pause/disable setting.
ARCHIVE_CONTROL_DIR=${ARCHIVE_CONTROL_DIR:-/tmp}

archive_cycle_begin() {
  export ARCHIVE_CYCLE_ID="$$:$(cat /proc/sys/kernel/random/uuid)"
  printf '%s\n' "$ARCHIVE_CYCLE_ID" > "$ARCHIVE_CONTROL_DIR/archive-cycle.new"
  mv "$ARCHIVE_CONTROL_DIR/archive-cycle.new" "$ARCHIVE_CONTROL_DIR/archive-cycle"
}

archive_cancel_requested() {
  [ -n "${ARCHIVE_CYCLE_ID:-}" ] &&
    [ -e "$ARCHIVE_CONTROL_DIR/archive-cycle-cancel-$ARCHIVE_CYCLE_ID" ]
}

archive_cycle_end() {
  local rc=0
  (
    flock -x 9 || exit 1
    if [ -n "${ARCHIVE_CYCLE_ID:-}" ] &&
       [ "$(cat "$ARCHIVE_CONTROL_DIR/archive-cycle" 2>/dev/null)" = "$ARCHIVE_CYCLE_ID" ]; then
      rm -f "$ARCHIVE_CONTROL_DIR/archive-cycle"
      # Capture every accepted request, including requests during teardown.
      archive_cancel_requested && exit 125
    fi
    exit 0
  ) 9>"$ARCHIVE_CONTROL_DIR/archive-cycle.lock" || rc=$?
  [ "$rc" != 125 ] || ARCHIVE_CYCLE_CANCELLED=1
  # Keep the per-cycle receipt until reboot for late reserved workers.
  unset ARCHIVE_CYCLE_ID
  [ "$rc" = 0 ] || [ "$rc" = 125 ]
}

archive_allow_cleanup_after_success() {
  (
    flock -x 9 || exit 1
    archive_cancel_requested && exit 125
    rm -f "$CAM_CLEANUP_DEFERRED" || exit 1
    sync -f "${CAM_CLEANUP_DEFERRED%/*}" >> "$LOG_FILE" 2>&1
  ) 9>"$ARCHIVE_CONTROL_DIR/archive-cycle.lock"
}

# Only external commands run here, in their own process group. Never killall:
# archive backends may have SSH, watchdog and transfer children, while other
# services can be running unrelated rsync/rclone commands at the same time.
# 125 is a deliberate skip, distinct from backend transfer failures.
archive_run_command() {
  archive_cancel_requested && return 125
  setsid --wait "$@" &
  local child=$! rc=0
  while kill -0 "$child" 2>/dev/null; do
    if archive_cancel_requested; then
      kill -TERM -- "-$child" 2>/dev/null || true
      for _ in 1 2 3 4 5; do
        kill -0 -- "-$child" 2>/dev/null || break
        sleep 1
      done
      kill -KILL -- "-$child" 2>/dev/null || true
      wait "$child" 2>/dev/null || true
      return 125
    fi
    sleep 0.2
  done
  wait "$child" || rc=$?
  # Reap stray watchdogs even when the backend exited first.
  kill -TERM -- "-$child" 2>/dev/null || true
  archive_cancel_requested && return 125
  return "$rc"
}
