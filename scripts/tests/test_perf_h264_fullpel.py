"""Unit tests for the no-build H.264 full-pel measurement harness."""
import importlib.util
import pathlib
import unittest


SCRIPT = pathlib.Path(__file__).parents[1] / "perf-h264-fullpel.py"
SPEC = importlib.util.spec_from_file_location("perf_h264_fullpel", SCRIPT)
HARNESS = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(HARNESS)


class H264FullPelHarnessTests(unittest.TestCase):
    def test_commands_emit_rawvideo_without_an_output_file(self):
        self.assertEqual(
            HARNESS.vaco_command("/tmp/vaco", "/tmp/input.mp4", 4)[-7:],
            ["-map", "0:v:0", "-c:v", "rawvideo", "-f", "rawvideo", "-"],
        )
        self.assertEqual(
            HARNESS.ffmpeg_command("ffmpeg", "/tmp/input.mp4", 4)[-7:],
            ["-map", "0:v:0", "-pix_fmt", "yuv420p", "-f", "rawvideo", "-"],
        )

    def test_paired_ratio_keeps_pairing_and_reports_candidate_wins(self):
        rounds = [
            {"baseline": {"cpu": 10.0}, "candidate": {"cpu": 9.0}},
            {"baseline": {"cpu": 20.0}, "candidate": {"cpu": 22.0}},
            {"baseline": {"cpu": 40.0}, "candidate": {"cpu": 36.0}},
        ]
        result = HARNESS.paired_ratio(rounds, "candidate", "baseline", "cpu")
        self.assertEqual(result["all"], [0.9, 1.1, 0.9])
        self.assertEqual(result["median"], 0.9)
        self.assertEqual(result["wins"], 2)

    def test_threads_reject_duplicates_and_non_positive_values(self):
        self.assertEqual(HARNESS.parse_threads("1,2,4,8"), [1, 2, 4, 8])
        with self.assertRaises(Exception):
            HARNESS.parse_threads("1,1")
        with self.assertRaises(Exception):
            HARNESS.parse_threads("0,1")
