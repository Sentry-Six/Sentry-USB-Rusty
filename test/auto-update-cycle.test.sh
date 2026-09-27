#!/bin/bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
python3 - "$root/run/archiveloop" "$work/functions" <<'PY'
import re,sys
from pathlib import Path
source=Path(sys.argv[1]).read_text()
names=['auto_update_cycle_eligible']
found=[]
for name in names:
    match=re.search(r'^function '+name+r'\b[^\n]*\n.*?^}',source,re.M|re.S)
    assert match, 'missing automatic update eligibility gate'
    found.append(match[0])
Path(sys.argv[2]).write_text('\n'.join(found))
PY
source "$work/functions"
archive_cancel_requested() { [[ ${cancel:-0} = 1 ]]; }
archive_is_reachable() { [[ ${reachable:-1} = 1 ]]; }
travel_mode_active() { [[ ${travel:-0} = 1 ]]; }
ARCHIVE_NEW_FILES=1 ARCHIVE_ALL_STAGES_OK=1 ARCHIVE_CYCLE_FAILED=0 ARCHIVE_CYCLE_CANCELLED=0
cancel=0 reachable=1 travel=0
auto_update_cycle_eligible
ARCHIVE_NEW_FILES=0
if auto_update_cycle_eligible; then echo 'zero files admitted'; exit 1; fi
ARCHIVE_NEW_FILES=1
for variable in cancel travel ARCHIVE_CYCLE_FAILED ARCHIVE_CYCLE_CANCELLED; do
  printf -v "$variable" 1
  if auto_update_cycle_eligible; then echo "$variable admitted"; exit 1; fi
  printf -v "$variable" 0
done
reachable=0
if auto_update_cycle_eligible; then echo 'departure admitted'; exit 1; fi
reachable=1
for stage in clips cleanup music maps export cloud teardown; do
  ARCHIVE_ALL_STAGES_OK=0
  if auto_update_cycle_eligible; then echo "$stage failure admitted"; exit 1; fi
done
unset ARCHIVE_ALL_STAGES_OK
if auto_update_cycle_eligible; then echo 'unknown status admitted'; exit 1; fi
echo 'PASS: successful nonempty cycle only; failed, skipped, cancelled, away and unknown cycles excluded'

python3 - "$root/run/archiveloop" "$work/cleanup" <<'PYTEST'
import re,sys
from pathlib import Path
s=Path(sys.argv[1]).read_text()
m=re.search(r'^function clean_empty_cam_directories[^\n]*\n.*?^}',s,re.M|re.S)
assert m, 'missing optional-directory-safe cleanup helper'
Path(sys.argv[2]).write_text(m[0])
PYTEST
source "$work/cleanup"
CAM_MOUNT="$work/cam"
mkdir -p "$CAM_MOUNT/TeslaCam/SavedClips/empty" "$CAM_MOUNT/TeslaCam/SavedClips/event"
printf original > "$CAM_MOUNT/TeslaCam/SavedClips/event/clip.mp4"
archive_run_command() { "$@"; }
clean_empty_cam_directories
[[ ! -e "$CAM_MOUNT/TeslaCam/SavedClips/empty" ]]
[[ "$(cat "$CAM_MOUNT/TeslaCam/SavedClips/event/clip.mp4")" = original ]]
archive_run_command() { return 1; }
if clean_empty_cam_directories; then echo 'real cleanup error ignored'; exit 1; fi
echo 'PASS: absent optional categories are harmless; real cleanup errors propagate; footage preserved'

# Exercise the production logger and processing poller, including swallowed
# child errors. All commands below are fixtures; no API/device is contacted.
python3 - "$root/run/archiveloop" "$root/run/post-archive-process.sh" "$work" <<'PYTEST'
import re,sys
from pathlib import Path
archive,post,work=map(Path,sys.argv[1:])
for source,names,dest in [(archive,['log'],'archive-log'),(post,['log','process_clips_dir'],'post-functions')]:
 functions=[]
 for name in names:
  m=re.search(r'^function '+name+r'\s*\([^\n]*\n.*?^}',source.read_text(),re.M|re.S)
  assert m, name
  functions.append(m[0].replace('/tmp/drive_process_response.json',str(work/'response.json')))
 (work/dest).write_text('\n'.join(functions))
PYTEST
source "$work/archive-log"
LOG_FILE="$work/archive.log" ARCHIVE_STAGE_FAILURE_FILE="$work/stage-failed"
ARCHIVE_ALL_STAGES_OK=1
( log 'ERROR: cleanup failed in child worker' )
[[ -f "$ARCHIVE_STAGE_FAILURE_FILE" ]]
if auto_update_cycle_eligible; then echo 'child failure lost'; exit 1; fi
rm "$ARCHIVE_STAGE_FAILURE_FILE"
source "$work/post-functions"
API_URL=fixture ARCHIVE_CYCLE_QUERY='' POST_ARCHIVE_FAILED=0
sleep() { :; }
curl() {
  if [[ "$*" == *'/api/drives/process?'* ]]; then
    printf '{"total":1}' > "$work/response.json"
    printf 202
  else
    printf '%s' "$fixture_status"
  fi
}
fixture_status='{"running":false,"archive_work_running":false,"error":null,"routes_count":1,"processed_count":1}'
process_clips_dir fixture
[[ "$POST_ARCHIVE_FAILED" = 0 ]]
fixture_status='{"running":false,"archive_work_running":true,"routes_count":1,"processed_count":1}'
if process_clips_dir fixture; then echo 'active background export admitted'; exit 1; fi
[[ "$POST_ARCHIVE_FAILED" = 1 && -f "$ARCHIVE_STAGE_FAILURE_FILE" ]]
POST_ARCHIVE_FAILED=0
fixture_status='{"routes_count":1,"processed_count":1}'
if process_clips_dir fixture; then echo 'unknown completion admitted'; exit 1; fi
[[ "$POST_ARCHIVE_FAILED" = 1 ]]
echo 'PASS: child failures persist; actual processing poller rejects active work, timeout and unknown completion'

# New scripts with an older API must retain availability-only notifications.
python3 - "$root/run/archiveloop" "$work" <<'PYTEST'
import re,sys
from pathlib import Path
s=Path(sys.argv[1]).read_text()
m=re.search(r'^function legacy_available_update_notification[^\n]*\n.*?^}',s,re.M|re.S)
assert m, 'missing mixed-version notification fallback'
Path(sys.argv[2],'legacy-notify').write_text(m[0].replace('/root/bin/send-push-message','send_notice').replace('/tmp/sentryusb-update-notified-',sys.argv[2]+'/notified-'))
PYTEST
source "$work/legacy-notify"
notices=0
send_notice() { notices=$((notices+1)); [[ "$2" == *v4.0.0* ]]; }
curl() {
  case "$*" in
    *auto_update_check*) printf '{"value":"enabled"}' ;;
    *update_channel*) printf '{"value":"stable"}' ;;
    *check-update*) printf '{"update_available":true,"latest_version":"v4.0.0"}' ;;
    *) return 1 ;;
  esac
}
legacy_available_update_notification fixture
legacy_available_update_notification fixture
[[ "$notices" = 1 ]]
echo 'PASS: old API availability notification remains available and deduplicated'

# Exercise the actual response discriminator against old and new API replies.
python3 - "$root/run/archiveloop" "$work" <<'PYTEST'
import re,sys
from pathlib import Path
s=Path(sys.argv[1]).read_text()
m=re.search(r'^function after_archive_needs_legacy_notification[^\n]*\n.*?^}',s,re.M|re.S)
assert m
Path(sys.argv[2],'legacy-response').write_text(m[0])
PYTEST
source "$work/legacy-response"
printf '<html>old embedded settings page</html>' > "$work/response"
after_archive_needs_legacy_notification 404 "$work/response"
after_archive_needs_legacy_notification 200 "$work/response"
if after_archive_needs_legacy_notification 409 "$work/response"; then exit 1; fi
printf '{"success":true}' > "$work/response"
if after_archive_needs_legacy_notification 200 "$work/response"; then exit 1; fi
echo 'PASS: mixed-version HTML and 404 fallback, without replay for new API or conflicts'
