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
names = ("archive_start_summary", "archive_finish_summary", "notification_duration")
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
[[ "$(archive_finish_summary true 704 704 5342)" == 'Archived 704 files in 1h 29m.' ]]
[[ "$(archive_finish_summary true 1 1 4)" == 'Archived 1 file in 4s.' ]]
[[ "$(archive_finish_summary false 704 691 4)" == 'Archive interrupted — 691 files archived, 13 remaining.' ]]
[[ "$(archive_finish_summary false 1 1 4)" == 'Archive interrupted — 1 file archived, 0 remaining.' ]]
[[ "$(notification_duration 62)" == '1m 2s' ]]
[[ "$(notification_duration 0)" == '0s' ]]
[[ "$(notification_duration 3600)" == '1h' ]]
echo 'PASS: current-batch counts, partial failure, singular and duration'

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
