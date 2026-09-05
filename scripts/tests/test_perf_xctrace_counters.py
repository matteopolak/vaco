#!/usr/bin/env python3
"""Tests for the macOS xctrace CPU-counter export parser and summaries."""

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "perf-xctrace-counters.py"
SPEC = importlib.util.spec_from_file_location("perf_xctrace_counters", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


PROCESS_COUNTER_EXPORT = """<?xml version="1.0" encoding="UTF-8"?>
<trace-query-result>
  <node>
    <row>
      <process id="17" pid="4242" name="vaco">vaco</process>
      <counter name="CPU Cycles" value="1200" />
      <counter name="Instructions Retired" value="900" />
    </row>
    <row>
      <process id="17" pid="4242" name="vaco">vaco</process>
      <counter name="CPU Cycles" value="1500" />
      <counter name="Instructions Retired" value="1100" />
    </row>
    <row>
      <process id="18" pid="4243" name="ffmpeg">ffmpeg</process>
      <counter name="CPU Cycles" value="700" />
      <counter name="Instructions Retired" value="600" />
    </row>
  </node>
</trace-query-result>
"""

XCTRACE_BOTTLENECK_EXPORT = """<?xml version="1.0"?>
<trace-query-result><node><row>
  <process id="3" fmt="yes (38060)"><pid id="4" fmt="38060">38060</pid></process>
  <uint64-array id="6" fmt="0x83c 0x52a 0x1383 0x61c">2108 1322 4995 1564</uint64-array>
</row></node></trace-query-result>
"""


class XcTraceParserTests(unittest.TestCase):
    def test_extracts_named_counters_for_the_launched_process(self):
        parsed = MODULE.parse_process_counters(PROCESS_COUNTER_EXPORT, "vaco")

        self.assertEqual(parsed["process"], "vaco")
        self.assertEqual(parsed["counters"]["cycles"], 1500)
        self.assertEqual(parsed["counters"]["instructions"], 1100)
        self.assertEqual(parsed["sample_rows"], 2)

    def test_refuses_bottleneck_slots_as_retired_instructions(self):
        xml = PROCESS_COUNTER_EXPORT.replace(
            "Instructions Retired", "Instruction Delivery Bottleneck"
        )

        with self.assertRaisesRegex(ValueError, "instructions"):
            MODULE.parse_process_counters(xml, "vaco")

    def test_parses_nested_metric_name_and_value_cells(self):
        xml = """<trace-query-result><row>
          <process name="vaco" />
          <metric><name>CPU Cycles</name><value>321</value></metric>
          <metric><name>Instructions Retired</name><value>123</value></metric>
        </row></trace-query-result>"""

        parsed = MODULE.parse_process_counters(xml, "vaco")

        self.assertEqual(parsed["counters"], {"cycles": 321, "instructions": 123})

    def test_rejects_an_export_without_the_requested_process(self):
        with self.assertRaisesRegex(ValueError, "candidate"):
            MODULE.parse_process_counters(PROCESS_COUNTER_EXPORT, "candidate")

    def test_real_bottleneck_process_format_reaches_instruction_contract(self):
        with self.assertRaisesRegex(ValueError, "instructions"):
            MODULE.parse_process_counters(XCTRACE_BOTTLENECK_EXPORT, "yes")


class HarnessSummaryTests(unittest.TestCase):
    def test_rotates_three_command_order_every_round(self):
        labels = ["vaco", "ffmpeg_t1", "candidate"]

        self.assertEqual(MODULE.rotating_order(labels, 0), labels)
        self.assertEqual(MODULE.rotating_order(labels, 1), ["ffmpeg_t1", "candidate", "vaco"])
        self.assertEqual(MODULE.rotating_order(labels, 2), ["candidate", "vaco", "ffmpeg_t1"])

    def test_parses_bsd_time_target_cpu_and_wall_seconds(self):
        parsed = MODULE.parse_time_l(
            "        0.17 real         0.11 user         0.06 sys\n"
            "             4096  maximum resident set size\n"
        )

        self.assertAlmostEqual(parsed["cpu_seconds"], 0.17)
        self.assertEqual(parsed["wall_seconds"], 0.17)

    def test_pairs_counters_and_cpu_wall_seconds_by_round(self):
        runs = {
            "vaco": [
                {"round": 0, "counters": {"cycles": 200, "instructions": 100},
                 "cpu_seconds": 0.20, "wall_seconds": 0.25},
                {"round": 1, "counters": {"cycles": 180, "instructions": 90},
                 "cpu_seconds": 0.18, "wall_seconds": 0.23},
            ],
            "ffmpeg_t1": [
                {"round": 0, "counters": {"cycles": 100, "instructions": 50},
                 "cpu_seconds": 0.10, "wall_seconds": 0.12},
                {"round": 1, "counters": {"cycles": 100, "instructions": 45},
                 "cpu_seconds": 0.10, "wall_seconds": 0.11},
            ],
        }

        ratios = MODULE.paired_ratios(runs)

        self.assertEqual(ratios["vaco/ffmpeg_t1"]["cycles"]["all"], [2.0, 1.8])
        self.assertEqual(ratios["vaco/ffmpeg_t1"]["instructions"]["median"], 2.0)
        self.assertEqual(ratios["vaco/ffmpeg_t1"]["cpu_seconds"]["all"][0], 2.0)
        self.assertAlmostEqual(ratios["vaco/ffmpeg_t1"]["cpu_seconds"]["all"][1], 1.8)
        self.assertEqual(ratios["vaco/ffmpeg_t1"]["wall_seconds"]["wins"], 0)

    def test_rejects_fewer_than_ten_measured_rounds(self):
        with self.assertRaisesRegex(ValueError, "at least 10"):
            MODULE.validate_rounds(9)


if __name__ == "__main__":
    unittest.main()
