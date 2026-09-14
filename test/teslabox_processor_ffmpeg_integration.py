#!/usr/bin/env python3
import json
import shutil
import subprocess
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parents[1] / "run" / "teslabox_processor"))

from processor import compose_event


@unittest.skipUnless(shutil.which("ffmpeg") and shutil.which("ffprobe"), "FFmpeg required")
class FFmpegIntegrationTests(unittest.TestCase):
    def test_composes_two_segments_from_four_cameras(self):
        with tempfile.TemporaryDirectory() as temp:
            root = Path(temp)
            event = root / "SentryClips" / "2026-09-13_12-00-00"
            event.mkdir(parents=True)
            (event / "event.json").write_text(
                '{"camera":"5","timestamp":"2026-09-13T12:00:11"}', encoding="utf-8"
            )
            colors = {
                "front": "red",
                "back": "blue",
                "left_repeater": "green",
                "right_repeater": "yellow",
            }
            for second in (10, 30):
                for camera, color in colors.items():
                    target = event / f"2026-09-13_12-00-{second:02d}-{camera}.mp4"
                    subprocess.run(
                        [
                            "ffmpeg", "-hide_banner", "-loglevel", "error", "-y",
                            "-f", "lavfi", "-i", f"color=c={color}:s=320x240:d=2:r=12",
                            "-c:v", "libx264", "-pix_fmt", "yuv420p", str(target),
                        ],
                        check=True,
                    )
            video = root / "composite.mp4"
            preview = root / "preview.jpg"

            compose_event(event, video, preview)

            probe = subprocess.run(
                [
                    "ffprobe", "-v", "error", "-show_entries", "stream=width,height",
                    "-show_entries", "format=duration", "-of", "json", str(video),
                ],
                check=True,
                text=True,
                capture_output=True,
            )
            output = json.loads(probe.stdout)
            self.assertEqual(output["streams"][0], {"width": 1280, "height": 720})
            self.assertGreater(float(output["format"]["duration"]), 3.0)
            self.assertGreater(video.stat().st_size, 0)
            self.assertGreater(preview.stat().st_size, 0)


if __name__ == "__main__":
    unittest.main()
