#!/usr/bin/env python3
"""Tests for the Linux hardware-cycle benchmark parser and summaries."""

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "perf-hwcycles.py"
SPEC = importlib.util.spec_from_file_location("perf_hwcycles", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class PerfStatParserTests(unittest.TestCase):
    def parse(self, text):
        with tempfile.NamedTemporaryFile("w", encoding="utf-8") as fh:
            fh.write(text)
            fh.flush()
            return MODULE.parse_perf_stat(Path(fh.name))

    def test_parses_counts_and_timing_without_locale_separators(self):
        parsed = self.parse(
            "123456;;cycles;998877;100.00;;\n"
            "234567;;instructions;998877;100.00;1.90;insn per cycle\n"
            "12.500000;msec;task-clock;998877;100.00;0.75;CPUs utilized\n"
            "7;;context-switches;998877;100.00;;\n"
            "2;;cpu-migrations;998877;100.00;;\n"
            "0.014200;;;seconds time elapsed\n"
            "0.011000;;;seconds user\n"
            "0.003000;;;seconds sys\n"
        )

        self.assertEqual(parsed["counters"]["cycles"], 123456)
        self.assertEqual(parsed["counters"]["instructions"], 234567)
        self.assertEqual(parsed["counters"]["task-clock"], 12.5)
        self.assertEqual(parsed["counters"]["context-switches"], 7)
        self.assertEqual(parsed["counter_running_pct"]["cycles"], 100.0)
        self.assertEqual(parsed["timings_seconds"]["elapsed"], 0.0142)
        self.assertEqual(parsed["timings_seconds"]["user"], 0.011)
        self.assertEqual(parsed["timings_seconds"]["sys"], 0.003)
        self.assertEqual(parsed["unavailable_events"], [])

    def test_marks_unsupported_and_multiplexed_hardware_events_unusable(self):
        parsed = self.parse(
            "<not supported>;;cycles;0;0.00;;\n"
            "800;;instructions;500;82.50;;\n"
            "1.25;msec;task-clock;500;100.00;;\n"
        )

        self.assertEqual(parsed["unavailable_events"], ["cycles"])
        self.assertEqual(parsed["multiplexed_events"], ["instructions"])
        self.assertFalse(parsed["hardware_counts_usable"])

    def test_accepts_hybrid_pmu_event_names_as_cycles_and_instructions(self):
        parsed = self.parse(
            "400;;cpu_core/cycles/;100;100.00;;\n"
            "600;;cpu_atom/cycles/;100;100.00;;\n"
            "1000;;cpu_core/instructions/;100;100.00;;\n"
            "1200;;cpu_atom/instructions/;100;100.00;;\n"
        )

        self.assertEqual(parsed["counters"]["cycles"], 1000)
        self.assertEqual(parsed["counters"]["instructions"], 2200)
        self.assertTrue(parsed["hardware_counts_usable"])

    def test_parses_compact_timing_rows_from_older_perf(self):
        parsed = self.parse(
            "10;;cycles;100;100.00;;\n"
            "20;;instructions;100;100.00;;\n"
            "0.001650064;seconds time elapsed\n"
            "0.001001000;seconds user\n"
            "0.000441000;seconds sys\n"
        )

        self.assertEqual(parsed["timings_seconds"]["elapsed"], 0.001650064)
        self.assertEqual(parsed["timings_seconds"]["user"], 0.001001)
        self.assertEqual(parsed["timings_seconds"]["sys"], 0.000441)


class SummaryTests(unittest.TestCase):
    def test_summarizes_medians_and_paired_ratios(self):
        runs = {
            "vaco": [
                {"counters": {"cycles": 220, "instructions": 500}},
                {"counters": {"cycles": 200, "instructions": 480}},
                {"counters": {"cycles": 210, "instructions": 490}},
            ],
            "ffmpeg_t1": [
                {"counters": {"cycles": 100, "instructions": 250}},
                {"counters": {"cycles": 100, "instructions": 240}},
                {"counters": {"cycles": 100, "instructions": 245}},
            ],
        }

        summary = MODULE.summarize_runs(runs)
        ratios = MODULE.paired_ratios(runs)

        self.assertEqual(summary["vaco"]["cycles"]["median"], 210)
        self.assertEqual(summary["vaco"]["cycles"]["min"], 200)
        self.assertEqual(summary["vaco"]["cycles"]["max"], 220)
        self.assertEqual(ratios["vaco/ffmpeg_t1"]["cycles"]["all"], [2.2, 2.0, 2.1])
        self.assertEqual(ratios["vaco/ffmpeg_t1"]["cycles"]["median"], 2.1)
        self.assertEqual(ratios["vaco/ffmpeg_t1"]["cycles"]["wins"], 0)


if __name__ == "__main__":
    unittest.main()
