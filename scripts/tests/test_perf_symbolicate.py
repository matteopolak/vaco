#!/usr/bin/env python3
"""Tests for Samply profile frame aggregation."""

import importlib.util
import unittest
from collections import Counter
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "perf-baseline-symbolicate.py"
SPEC = importlib.util.spec_from_file_location("perf_baseline_symbolicate", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class AggregationTests(unittest.TestCase):
    def test_selects_outermost_physical_frame_by_default(self):
        counts = MODULE.aggregate_counts(
            Counter({0x10: 3}),
            {0x10: ["core::hot at core.rs:4", "codec::wrapper at codec.rs:9"]},
            innermost=False,
        )

        self.assertEqual(counts, Counter({"codec::wrapper": 3}))

    def test_selects_innermost_inlined_frame_when_requested(self):
        counts = MODULE.aggregate_counts(
            Counter({0x10: 3, 0x20: 2}),
            {
                0x10: ["core::hot at core.rs:4", "codec::wrapper at codec.rs:9"],
                0x20: ["?? at ??:0"],
            },
            innermost=True,
        )

        self.assertEqual(counts, Counter({"core::hot": 3, "??": 2}))


if __name__ == "__main__":
    unittest.main()
