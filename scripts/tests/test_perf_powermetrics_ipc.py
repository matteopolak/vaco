#!/usr/bin/env python3
"""Tests for the fail-closed macOS powermetrics IPC parser."""

import importlib.util
import plistlib
import unittest
from pathlib import Path
from unittest import mock


SCRIPT = Path(__file__).resolve().parents[1] / "perf-powermetrics-ipc.py"
SPEC = importlib.util.spec_from_file_location("perf_powermetrics_ipc", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


def sample(**overrides):
    value = {
        "is_delta": True,
        "elapsed_ns": 2_000_000_000,
        "tasks": [{
            "id": 4242,
            "name": "vvmpeg",
            "cpu_instructions": 12.5,
            "cpu_cycles": 24.0,
        }],
    }
    value.update(overrides)
    return plistlib.dumps(value) + b"\0"


class PowermetricsParserTests(unittest.TestCase):
    def test_converts_only_the_named_delta_rate_with_its_elapsed_interval(self):
        parsed = MODULE.parse_process_sample(sample(), 4242, "vvmpeg")

        self.assertEqual(parsed["counters"], {"instructions": 25.0, "cycles": 48.0})
        self.assertEqual(parsed["raw_rates_per_second"]["instructions"], 12.5)
        self.assertEqual(parsed["process_scope"], "launched PID only; child processes are excluded")

    def test_rejects_a_lifetime_sample(self):
        with self.assertRaisesRegex(ValueError, "not a delta"):
            MODULE.parse_process_sample(sample(is_delta=False), 4242, "vvmpeg")

    def test_rejects_missing_or_ambiguous_target_pid(self):
        with self.assertRaisesRegex(ValueError, "found 0"):
            MODULE.parse_process_sample(sample(), 9, "vvmpeg")

        duplicate = plistlib.loads(sample().rstrip(b"\0"))
        duplicate["tasks"].append(duplicate["tasks"][0].copy())
        with self.assertRaisesRegex(ValueError, "found 2"):
            MODULE.parse_process_sample(plistlib.dumps(duplicate), 4242, "vvmpeg")

    def test_rejects_name_reuse_invalid_rows_and_bad_counter_fields(self):
        with self.assertRaisesRegex(ValueError, "expected 'ffmpeg'"):
            MODULE.parse_process_sample(sample(), 4242, "ffmpeg")

        invalid = plistlib.loads(sample().rstrip(b"\0"))
        invalid["tasks"][0]["invalid"] = True
        with self.assertRaisesRegex(ValueError, "invalid"):
            MODULE.parse_process_sample(plistlib.dumps(invalid), 4242, "vvmpeg")

        malformed = plistlib.loads(sample().rstrip(b"\0"))
        malformed["tasks"][0]["cpu_cycles"] = "24"
        with self.assertRaisesRegex(ValueError, "not numeric"):
            MODULE.parse_process_sample(plistlib.dumps(malformed), 4242, "vvmpeg")

    def test_rejects_multiple_plists(self):
        with self.assertRaisesRegex(ValueError, "exactly one"):
            MODULE.parse_process_sample(sample() + sample(), 4242, "vvmpeg")


class PowermetricsHarnessTests(unittest.TestCase):
    def test_rotates_command_order(self):
        labels = ["vaco", "ffmpeg_t1", "candidate"]
        self.assertEqual(MODULE.rotating_order(labels, 1), ["ffmpeg_t1", "candidate", "vaco"])
        self.assertEqual(MODULE.rotating_order(labels, 2), ["candidate", "vaco", "ffmpeg_t1"])

    def test_refuses_fewer_than_ten_rounds(self):
        with self.assertRaisesRegex(ValueError, "at least 10"):
            MODULE.validate_rounds(9)

    def test_noninteractive_preflight_explains_the_root_requirement(self):
        completed = mock.Mock(returncode=1, stderr=b"sudo: a password is required\n")
        with mock.patch.object(MODULE.subprocess, "run", return_value=completed):
            with self.assertRaisesRegex(RuntimeError, "noninteractive root"):
                MODULE.preflight("sudo", "/usr/bin/powermetrics")

    def test_preflight_handles_a_sandbox_that_cannot_launch_sudo(self):
        with mock.patch.object(MODULE.subprocess, "run", side_effect=PermissionError(1, "Operation not permitted")):
            with self.assertRaisesRegex(RuntimeError, "could not launch"):
                MODULE.preflight("sudo", "/usr/bin/powermetrics")


if __name__ == "__main__":
    unittest.main()
