#!/usr/bin/env python3
"""Fixture-driven tests for the PGO manifest and profile checks."""

import importlib.util
import tempfile
import unittest
from pathlib import Path


SCRIPT = Path(__file__).resolve().parents[1] / "pgo.py"
SPEC = importlib.util.spec_from_file_location("vaco_pgo", SCRIPT)
MODULE = importlib.util.module_from_spec(SPEC)
assert SPEC.loader is not None
SPEC.loader.exec_module(MODULE)


class PgoManifestTests(unittest.TestCase):
    def test_manifest_expands_environment_and_preserves_arguments(self):
        manifest = MODULE.load_manifest_text(
            """
version = 1
max_component_share = 0.15

[[workload]]
name = "decode"
group = "decode"
component = "vaco-codec-vp9"
binary = "vvmpeg"
args = ["-i", "${VACO_PROFILE_FIXTURES}/clip.webm", "-f", "null", "-"]
""",
        )
        command = MODULE.resolve_command(
            manifest["workload"][0],
            {"VACO_PROFILE_FIXTURES": "/fixtures"},
            Path("/target/release"),
        )
        self.assertEqual(command, [
            "/target/release/vvmpeg",
            "-i",
            "/fixtures/clip.webm",
            "-f",
            "null",
            "-",
        ])

    def test_manifest_requires_unique_names_and_declares_training_groups(self):
        with self.assertRaises(MODULE.ManifestError):
            MODULE.load_manifest_text(
                """
version = 1
[[workload]]
name = "same"
group = "decode"
component = "codec"
binary = "vvmpeg"
args = []
[[workload]]
name = "same"
group = "encode"
component = "codec"
binary = "vvmpeg"
args = []
"""
            )

    def test_profile_check_uses_manifest_weight_not_counter_volume(self):
        manifest = MODULE.load_manifest_text(
            """
version = 1
max_component_share = 0.15
required_components = ["vaco-codec-vp9", "vaco-codec-flac"]
[[workload]]
name = "vp9"
group = "decode"
component = "vaco-codec-vp9"
binary = "vvmpeg"
weight = 9
args = []
[[workload]]
name = "flac"
group = "decode"
component = "vaco-codec-flac"
binary = "vvmpeg"
weight = 1
args = []
"""
        )
        report = MODULE.check_profile(
            manifest,
            {"vaco-codec-vp9": 90, "vaco-codec-flac": 1},
            required_components=["vaco-codec-vp9", "vaco-codec-flac"],
        )
        self.assertEqual(report["missing_components"], [])
        self.assertIn("vaco-codec-vp9", report["overweight_components"])

    def test_profile_check_does_not_confuse_hot_counter_volume_with_overfit(self):
        manifest = MODULE.load_manifest_text(
            """
version = 1
max_component_share = 0.6
required_components = ["vaco-codec-vp9", "vaco-codec-flac"]
[[workload]]
name = "vp9"
group = "decode"
component = "vaco-codec-vp9"
binary = "vvmpeg"
weight = 1
args = []
[[workload]]
name = "flac"
group = "decode"
component = "vaco-codec-flac"
binary = "vvmpeg"
weight = 1
args = []
"""
        )
        report = MODULE.check_profile(
            manifest,
            {"vaco-codec-vp9": 1_000_000, "vaco-codec-flac": 1},
        )
        self.assertTrue(report["ok"])
        self.assertEqual(report["shares"], {"vaco-codec-flac": 0.5, "vaco-codec-vp9": 0.5})

    def test_profdata_dump_parser_reads_counts_and_ignores_headers(self):
        counts = MODULE.parse_profdata_dump(
            """
Counters:
  vaco_codec_vp9::decode_frame:
    Hash: 0x0000000000000000
    Counters: 1
    Block counts: [42, 0, 3]
  vaco_codec_flac::decode_frame:
    Hash: 0x0000000000000000
    Counters: 1
    Block counts: [0]
Functions shown: 2
"""
        )
        self.assertEqual(counts["vaco-codec-vp9"], 45)
        self.assertEqual(counts["vaco-codec-flac"], 0)

    def test_profdata_dump_parser_prefers_function_count_to_block_counts(self):
        counts = MODULE.parse_profdata_dump(
            """
Counters:
  vaco_codec_vp9::decode_frame:
    Function count: 7
    Block counts: [7, 3, 2]
"""
        )
        self.assertEqual(counts, {"vaco-codec-vp9": 7})

    def test_profdata_dump_parser_uses_known_crate_boundaries_in_rust_symbols(self):
        counts = MODULE.parse_profdata_dump(
            """
Counters:
  _RNvMs_NtCs_14vaco_codec_vp98framebuf11materialize:
    Block counts: [7, 2]
""",
            {"vaco-codec-vp9"},
        )
        self.assertEqual(counts, {"vaco-codec-vp9": 9})

    def test_lockfile_pins_the_manifest_content(self):
        with tempfile.TemporaryDirectory() as directory:
            root = Path(directory)
            manifest = root / "workload.toml"
            manifest.write_text("version = 1\n[[workload]]\nname = 'x'\ngroup = 'cli'\ncomponent = 'x'\nbinary = 'x'\nargs = []\n")
            lock = root / "lockfile.toml"
            lock.write_text(f'manifest_sha256 = "{MODULE.manifest_sha256(manifest)}"\n')
            MODULE.check_lock(lock, manifest)
            manifest.write_text(manifest.read_text() + "\n")
            with self.assertRaises(MODULE.ManifestError):
                MODULE.check_lock(lock, manifest)


if __name__ == "__main__":
    unittest.main()
