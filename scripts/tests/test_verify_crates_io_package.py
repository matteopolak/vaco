#!/usr/bin/env python3
"""Tests for the read-only installable-package gate."""

from __future__ import annotations

import unittest
import importlib.util
from pathlib import Path


MODULE_PATH = Path(__file__).parents[1] / "verify-crates-io-package.py"
SPEC = importlib.util.spec_from_file_location("verify_crates_io_package", MODULE_PATH)
assert SPEC is not None and SPEC.loader is not None
verify_crates_io_package = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verify_crates_io_package)


class PackageListTests(unittest.TestCase):
    """Ensure the gate catches both missing and legacy install surfaces."""

    def test_package_files_ignores_cargo_diagnostics(self) -> None:
        """Diagnostics must not be mistaken for paths in Cargo's output."""
        output = """
            Packaging vaco v0.1.0 (/workspace/crates/app/vaco)
            Cargo.toml
            README.md
            src/lib.rs
            src/bin/vvmpeg.rs
            src/bin/vvprobe.rs
            warning: manifest has no description
        """
        self.assertEqual(
            verify_crates_io_package.package_files(output),
            verify_crates_io_package.REQUIRED_FILES,
        )

    def test_exact_install_contract_passes(self) -> None:
        """The promised files and binary names produce no violations."""
        self.assertEqual(
            verify_crates_io_package.verify(
                verify_crates_io_package.REQUIRED_FILES,
                ("vvmpeg", "vvprobe"),
            ),
            [],
        )

    def test_legacy_name_and_missing_readme_fail(self) -> None:
        """A package with an old binary source cannot pass the install gate."""
        files = set(verify_crates_io_package.REQUIRED_FILES)
        files.remove("README.md")
        files.add("src/bin/vaco-probe.rs")
        failures = verify_crates_io_package.verify(files, ("vaco", "vaco-probe"))
        self.assertEqual(len(failures), 4)
        self.assertTrue(any("README.md" in failure for failure in failures))
        self.assertTrue(any("legacy" in failure for failure in failures))
        self.assertTrue(any("binary targets" in failure for failure in failures))

    def test_generated_artifact_is_rejected(self) -> None:
        """The tiny facade archive must not carry build or generated output."""
        files = set(verify_crates_io_package.REQUIRED_FILES)
        files.add("target/generated.rs")
        failures = verify_crates_io_package.verify(files, ("vvmpeg", "vvprobe"))
        self.assertEqual(len(failures), 1)
        self.assertIn("unexpected", failures[0])


class MetadataTargetTests(unittest.TestCase):
    """Keep target discovery tied to Cargo metadata rather than a second list."""

    def test_target_names_only_returns_bins(self) -> None:
        metadata = {
            "packages": [
                {
                    "name": "vaco",
                    "targets": [
                        {"name": "vaco", "kind": ["lib"]},
                        {"name": "vvmpeg", "kind": ["bin"]},
                        {"name": "vvprobe", "kind": ["bin"]},
                    ],
                }
            ]
        }
        self.assertEqual(
            verify_crates_io_package.target_names(metadata), ("vvmpeg", "vvprobe")
        )

    def test_public_metadata_contract_includes_license_and_readme(self) -> None:
        package = {
            "name": "vaco",
            "license": "GPL-3.0-or-later",
            "readme": "/workspace/crates/app/vaco/README.md",
            "repository": "https://github.com/matteopolak/vaco",
            "publish": None,
        }
        self.assertEqual(verify_crates_io_package.verify_metadata(package), [])

    def test_wrong_license_is_a_release_blocker(self) -> None:
        package = {
            "name": "vaco",
            "license": "MIT",
            "readme": "README.md",
            "repository": "https://github.com/matteopolak/vaco",
            "publish": None,
        }
        failures = verify_crates_io_package.verify_metadata(package)
        self.assertEqual(len(failures), 1)
        self.assertIn("license", failures[0])


if __name__ == "__main__":
    unittest.main()
