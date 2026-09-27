#!/bin/bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
export TEST_WORK="$work"
export LOG_FILE="$work/log"
export PATH="$work:$PATH"
tr -d '\r' < "$root/run/send-push-message" > "$work/send"
cat > "$work/curl" <<'SH'
#!/bin/bash
if [[ "$*" == *settings/check* ]]; then
  printf '{"enabled":%s}\n' "${TEST_GATE:-true}"
else
  while (($#)); do
    if [[ "$1" == -d ]]; then printf '%s' "$2" > "$TEST_WORK/payload"; break; fi
    shift
  done
  printf '{"attempted":1,"providers":["webhook"],"failed":[]}\n200\n'
fi
SH
chmod +x "$work/curl"
export NOTIFICATION_COMMAND_ENABLED=true
export NOTIFICATION_COMMAND_START='printf "%s" "$CURRENT_ARCHIVE_FILE" > "$TEST_WORK/hook"'
export NOTIFICATION_COMMAND_FINISH='printf "%s" "$CURRENT_ARCHIVE_FILE" > "$TEST_WORK/finish-hook"'
export CURRENT_ARCHIVE_FILE='clip with spaces.mp4'
export ARCHIVE_TOTAL_COUNT=13
detail=$'Full "diagnostic"\nexit code 23; $(touch should-not-exist)'
short='Archiving 13 files · test'
export EXPECT_DETAIL="$detail" EXPECT_SHORT="$short"
bash -eu "$work/send" SentryUSB "$detail" start archive_start "$short"
python3 - <<'PY'
import json, os, time
from pathlib import Path
p = Path(os.environ["TEST_WORK"])
body = json.loads((p / "payload").read_text())
assert body["message"] == os.environ["EXPECT_DETAIL"]
assert body.get("summary") == os.environ["EXPECT_SHORT"], body
assert body["archive_total_count"] == 13 and body["type"] == "start"
assert body["notification_type"] == "archive_start"
for _ in range(100):
    if (p / "hook").exists(): break
    time.sleep(.01)
assert (p / "hook").read_text() == "clip with spaces.mp4"
assert "exit code 23" in (p / "log").read_text()
PY
rm "$work/payload" "$work/hook"
bash -eu "$work/send" SentryUSB "$detail" finish archive_error
python3 - <<'PY'
import json, os, time
from pathlib import Path
p = Path(os.environ["TEST_WORK"])
body = json.loads((p / "payload").read_text())
assert "summary" not in body
assert body["message"] == os.environ["EXPECT_DETAIL"]
for _ in range(100):
    if (p / "finish-hook").exists(): break
    time.sleep(.01)
assert (p / "finish-hook").read_text() == "clip with spaces.mp4"
PY
rm "$work/payload" "$work/finish-hook"
TEST_GATE=false bash -eu "$work/send" SentryUSB "$detail" start archive_start "$short"
[[ ! -e "$work/payload" && ! -e "$work/hook" ]]
TEST_GATE=false bash -eu "$work/send" SentryUSB "$detail" finish archive_complete "$short"
[[ ! -e "$work/payload" && ! -e "$work/finish-hook" ]]
echo 'PASS: optional summary, legacy payload, literal text, metadata, hooks and disabled gate'
