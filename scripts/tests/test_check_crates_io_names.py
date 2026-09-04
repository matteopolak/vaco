#!/usr/bin/env python3
"""Fixtures for exact crates.io ownership and availability preflight."""

from __future__ import annotations

import unittest

from scripts import check_crates_io_names


class CratesIoNamePreflightTests(unittest.TestCase):
    """Ensure an existing crate is acceptable only when exact ownership matches."""

    def test_available_and_owned_names_pass(self) -> None:
        """A 404 is available and an exact expected owner is safe to release."""
        replies = {
            "/crates/available": (404, {}),
            "/crates/owned": (200, {"crate": {"id": "owned"}}),
            "/crates/owned/owners": (200, {"users": [{"login": "vaco-org"}]}),
        }
        report = check_crates_io_names.check_names(
            ["available", "owned"], "vaco-org", replies.__getitem__
        )

        self.assertEqual(report.available, ["available"])
        self.assertEqual(report.owned, ["owned"])
        self.assertEqual(report.conflicts, {})

    def test_conflicting_owner_fails_before_any_publish(self) -> None:
        """The gate records the conflicting exact name instead of a partial release."""
        replies = {
            "/crates/available": (404, {}),
            "/crates/conflict": (200, {"crate": {"id": "conflict"}}),
            "/crates/conflict/owners": (200, {"users": [{"login": "someone-else"}]}),
        }
        report = check_crates_io_names.check_names(
            ["available", "conflict"], "vaco-org", replies.__getitem__
        )

        self.assertEqual(report.available, ["available"])
        self.assertEqual(report.conflicts, {"conflict": ["someone-else"]})


if __name__ == "__main__":
    unittest.main()
