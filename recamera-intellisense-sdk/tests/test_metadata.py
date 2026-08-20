"""Package metadata consistency tests."""

from __future__ import annotations

import unittest
from pathlib import Path

import recamera_intellisense


class VersionConsistencyTests(unittest.TestCase):
    def test_runtime_version_matches_project_metadata(self) -> None:
        pyproject = Path(__file__).resolve().parents[1] / "pyproject.toml"
        version_line = next(
            line for line in pyproject.read_text(encoding="utf-8").splitlines()
            if line.startswith("version = ")
        )
        package_version = version_line.split('"', 2)[1]
        self.assertEqual(recamera_intellisense.__version__, package_version)


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
