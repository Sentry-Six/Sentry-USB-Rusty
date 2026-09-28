#!/bin/bash
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd)
work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
# Execute pure formatting functions from production without starting the daemon.
python3 - "$root/run/archiveloop" "$work/functions" <<'PY'
import re, sys
from pathlib import Path
source = Path(sys.argv[1]).read_text()
names = ("archive_start_summary", "archive_finish_summary", "notification_duration",
         "archive_resume_context", "write_archive_checkpoint", "finish_archive_checkpoint")
functions = []
for name in names:
    match = re.search(r"^function " + name + r"\b[^\n]*\n.*?^}", source, re.M | re.S)
    assert match, f"missing production formatter {name}"
    functions.append(match[0])
Path(sys.argv[2]).write_text("\n".join(functions))
PY
source "$work/functions"
[[ "$(archive_start_summary 13)" == 'Archiving 13 files.' ]]
[[ "$(archive_start_summary 1)" == 'Archiving 1 file.' ]]
[[ "$(archive_start_summary 2012 682 2694)" == '682/2694 Archived, resuming archive after reconnect.' ]]
[[ "$(archive_finish_summary true 704 704 5342)" == 'Archived 704 files in 1h 29m.' ]]
[[ "$(archive_finish_summary true 1 1 4)" == 'Archived 1 file in 4s.' ]]
[[ "$(archive_finish_summary false 704 691 4)" == 'Archive interrupted — 691 files archived, 13 remaining.' ]]
[[ "$(archive_finish_summary false 1 1 4)" == 'Archive interrupted — 1 file archived, 0 remaining.' ]]
[[ "$(notification_duration 62)" == '1m 2s' ]]
[[ "$(notification_duration 0)" == '0s' ]]
[[ "$(notification_duration 3600)" == '1h' ]]
echo 'PASS: current-batch counts, partial failure, singular and duration'

checkpoint="$work/progress.json"
python3 - "$work" <<'PYFIXTURE'
import sys
from pathlib import Path
root = Path(sys.argv[1])
files = [f"SavedClips/event {n}/clip {n}.mp4" for n in range(2694)]
files[0] = f'SavedClips/$(touch {root}/injected)/"quoted"; clip.mp4'
(root / "full").write_text("\n".join(files) + "\n")
(root / "reordered").write_text("\n".join(reversed(files)) + "\n")
(root / "remaining").write_text("\n".join(files[682:]) + "\n")
(root / "remaining-again").write_text("\n".join(files[686:]) + "\n")
(root / "unrelated").write_text("\n".join("different/" + name for name in files) + "\n")
PYFIXTURE
read -r fingerprint completed offset total < <(archive_resume_context "$work/full" 2694 "$checkpoint")
[[ "$completed $offset $total" == '0 0 2694' ]]
# A reboot restarts the same candidate scan, so the displayed checkpoint must
# not become an offset that would count those files twice in the next attempt.
write_archive_checkpoint '{"current":682,"total":2694}' "$fingerprint" 0 2694 "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/reordered" 2694 "$checkpoint")
[[ "$key $completed $offset $total" == "$fingerprint 682 0 2694" ]]
[[ "$(archive_start_summary 2694 "$completed" "$total")" == '682/2694 Archived, resuming archive after reconnect.' ]]
read -r key completed offset total < <(archive_resume_context "$work/unrelated" 2694 "$checkpoint")
[[ "$completed $offset $total" == '0 0 2694' ]]
# A handled disconnect has already persisted its completed files in the ledger.
# Its next candidate list is smaller, while the notification keeps the batch total.
finish_archive_checkpoint false false "$work/remaining" 2012 682 2694 "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/remaining" 2012 "$checkpoint")
[[ "$completed $offset $total" == '682 682 2694' ]]
[[ "$(archive_start_summary 2012 "$completed" "$total")" == '682/2694 Archived, resuming archive after reconnect.' ]]
write_archive_checkpoint '{"current":4,"total":2012}' "$key" "$offset" "$total" "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/remaining" 2012 "$checkpoint")
[[ "$completed $offset $total" == '686 682 2694' ]]
finish_archive_checkpoint false false "$work/remaining-again" 2008 686 2694 "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/remaining-again" 2008 "$checkpoint")
[[ "$completed $offset $total" == '686 686 2694' ]]
# Unkeyed legacy checkpoints, malformed data, and impossible counts cannot be
# safely attributed to this batch, even when their totals happen to match.
for invalid in '{"current":682,"total":2694}' 'not JSON' 'null'; do
  printf '%s' "$invalid" > "$checkpoint"
  read -r key completed offset total < <(archive_resume_context "$work/full" 2694 "$checkpoint")
  [[ "$completed $offset $total" == '0 0 2694' ]]
done
for invalid_current in -1 2695 null true; do
  write_archive_checkpoint "{\"current\":$invalid_current,\"total\":2694}" "$fingerprint" 0 2694 "$checkpoint"
  read -r key completed offset total < <(archive_resume_context "$work/full" 2694 "$checkpoint")
  [[ "$completed $offset $total" == '0 0 2694' ]]
done
write_archive_checkpoint '{"current":682,"total":2694}' "$fingerprint" 1 2694 "$checkpoint"
read -r key completed offset total < <(archive_resume_context "$work/full" 2694 "$checkpoint")
[[ "$completed $offset $total" == '0 0 2694' ]]
for invalid_counts in '-1 2693' '1.5 2695.5' '9223372036854775808 9223372036854778502'; do
  read -r previous original <<< "$invalid_counts"
  write_archive_checkpoint '{"current":682,"total":2694}' "$fingerprint" "$previous" "$original" "$checkpoint"
  read -r key completed offset total < <(archive_resume_context "$work/full" 2694 "$checkpoint")
  [[ "$completed $offset $total" == '0 0 2694' ]]
done
for outcomes in 'true false 2012' 'false true 2012' 'false false 0'; do
  touch "$checkpoint"
  read -r success cancelled remaining <<< "$outcomes"
  finish_archive_checkpoint "$success" "$cancelled" "$work/remaining" "$remaining" 682 2694 "$checkpoint"
  [[ ! -e "$checkpoint" ]]
done
[[ $(wc -l < "$work/full") -eq 2694 && $(wc -l < "$work/remaining") -eq 2012 && ! -e "$work/injected" ]]
echo 'PASS: reconnect and reboot counts, repeated interruptions, unrelated/invalid checkpoints, success and cancellation'

python3 - "$root" "$work/other-functions" <<'PY'
import re, sys
from pathlib import Path
root = Path(sys.argv[1])
pairs = [("run/post-archive-process.sh", "drive_mapping_summary"),
         ("run/temperature_monitor", "temperature_summary"),
         ("run/cifs_archive/copy-music.sh", "music_sync_summary")]
functions = []
for file, name in pairs:
    match = re.search(r"^function " + name + r"\b[^\n]*\n.*?^}", (root/file).read_text(), re.M | re.S)
    assert match, f"missing production formatter {name}"
    functions.append(match[0])
Path(sys.argv[2]).write_text("\n".join(functions))
PY
source "$work/other-functions"
[[ "$(drive_mapping_summary 3 12.02 0 0.00 miles)" == 'Mapped 3 drives since your last archive · 12.02 miles.' ]]
[[ "$(drive_mapping_summary 0 0.00 1 1.50 km)" == 'Mapped 1 drive · 1.50 km.' ]]
[[ "$(drive_mapping_summary 3 12.02 2 8.00 miles)" == 'Mapped 5 drives · 20.02 miles. 3 mapped earlier · 2 mapped now.' ]]
[[ -z "$(drive_mapping_summary 0 0 0 0 miles)" ]]
[[ "$(TEMPERATURE_UNIT=C temperature_summary 41400 42800)" == 'Device temperature: 41°C · Recent peak: 43°C.' ]]
[[ "$(TEMPERATURE_UNIT=F temperature_summary 41400 42800)" == 'Device temperature: 107°F · Recent peak: 109°F.' ]]
[[ "$(music_sync_summary 12 2 0)" == 'Music synced: 12 copied, 2 removed.' ]]
[[ "$(music_sync_summary 12 2 1)" == 'Music sync incomplete: 12 copied, 2 removed, 1 error.' ]]
echo 'PASS: earlier/current mapping, units, temperature, music deletion and errors'
