#!/usr/bin/env python3
"""Fixtures for the deterministic crates.io publication migration planner."""

from __future__ import annotations

import json
from pathlib import Path
import subprocess
import tempfile
import unittest


REPOSITORY = Path(__file__).resolve().parents[2]
PLANNER = REPOSITORY / "scripts" / "plan-publish-migration.py"


class PublishMigrationPlanTests(unittest.TestCase):
    """Exercise closure classification and the guarded fixture-only rewriter."""

    def setUp(self) -> None:
        self.temporary = tempfile.TemporaryDirectory()
        self.root = Path(self.temporary.name)
        self.manifests = {
            "app": self.write_manifest(
                "crates/app/Cargo.toml",
                """[package]
name = "app"
version = "0.1.0"

[dependencies]
library = { path = "../library" }

[dev-dependencies]
test-only = { path = "../test-only" }
""",
            ),
            "library": self.write_manifest(
                "crates/library/Cargo.toml",
                """[package]
name = "library"
version = "0.1.0"
publish = false
""",
            ),
            "excluded": self.write_manifest(
                "crates/excluded/Cargo.toml",
                """[package]
name = "excluded"
version = "0.1.0"
""",
            ),
            "test-only": self.write_manifest(
                "crates/test-only/Cargo.toml",
                """[package]
name = "test-only"
version = "0.1.0"
publish = false
""",
            ),
        }
        self.metadata = self.root / "metadata.json"
        self.metadata.write_text(json.dumps(self.fixture_metadata()), encoding="utf-8")

    def tearDown(self) -> None:
        self.temporary.cleanup()

    def write_manifest(self, relative: str, contents: str) -> Path:
        """Create one isolated package manifest used only by this fixture."""
        manifest = self.root / relative
        manifest.parent.mkdir(parents=True, exist_ok=True)
        manifest.write_text(contents, encoding="utf-8")
        return manifest

    def fixture_metadata(self) -> dict[str, object]:
        """Model one runtime edge, one excluded member, and one dev-only edge."""
        packages = []
        for name, manifest in self.manifests.items():
            dependencies: list[dict[str, object]] = []
            if name == "app":
                dependencies = [
                    {
                        "name": "library",
                        "kind": None,
                        "path": str(self.manifests["library"].parent),
                        "req": "*",
                    },
                    {
                        "name": "test-only",
                        "kind": "dev",
                        "path": str(self.manifests["test-only"].parent),
                        "req": "*",
                    },
                ]
            packages.append(
                {
                    "id": name,
                    "name": name,
                    "version": "0.1.0",
                    "manifest_path": str(manifest),
                    "publish": [] if name in {"library", "test-only"} else None,
                    "dependencies": dependencies,
                }
            )
        return {
            "packages": packages,
            "workspace_members": ["app", "library", "excluded", "test-only"],
            "resolve": {
                "nodes": [
                    {
                        "id": "app",
                        "deps": [
                            {"pkg": "library", "dep_kinds": [{"kind": None}]},
                            {"pkg": "test-only", "dep_kinds": [{"kind": "dev"}]},
                        ],
                    },
                    {"id": "library", "deps": []},
                    {"id": "excluded", "deps": []},
                    {"id": "test-only", "deps": []},
                ]
            },
        }

    def invoke(self, *extra: str) -> subprocess.CompletedProcess[str]:
        """Run the planner against the fixture without accessing this workspace."""
        return subprocess.run(
            [
                "python3",
                str(PLANNER),
                "--root",
                "app",
                "--metadata",
                str(self.metadata),
                "--workspace-root",
                str(self.root),
                *extra,
            ],
            check=False,
            capture_output=True,
            text=True,
        )

    def test_plan_excludes_dev_dependencies_and_orders_operations(self) -> None:
        """Only shipped edges enter the closure and the JSON plan is stable."""
        result = self.invoke()
        self.assertEqual(result.returncode, 0, result.stderr)
        plan = json.loads(result.stdout)

        self.assertEqual(plan["closure"]["internal_packages"], ["app", "library"])
        self.assertEqual(
            plan["operations"],
            [
                {
                    "dependency": "library",
                    "expected_version": "=0.1.0",
                    "kind": "set-path-version",
                    "manifest": "crates/app/Cargo.toml",
                    "package": "app",
                },
                {
                    "kind": "set-publish",
                    "manifest": "crates/excluded/Cargo.toml",
                    "package": "excluded",
                    "publish": False,
                },
                {
                    "kind": "set-publish",
                    "manifest": "crates/library/Cargo.toml",
                    "package": "library",
                    "publish": True,
                },
            ],
        )

    def test_apply_rewrites_only_manifests_in_the_plan(self) -> None:
        """The explicit apply switch updates planned fixture files and no dev edge."""
        result = self.invoke("--apply")
        self.assertEqual(result.returncode, 0, result.stderr)

        self.assertIn('version = "=0.1.0"', self.manifests["app"].read_text())
        self.assertIn('test-only = { path = "../test-only" }', self.manifests["app"].read_text())
        self.assertIn("publish = true", self.manifests["library"].read_text())
        self.assertIn("publish = false", self.manifests["excluded"].read_text())
        self.assertIn("publish = false", self.manifests["test-only"].read_text())


if __name__ == "__main__":
    unittest.main()
