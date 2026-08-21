"""Guard: the skill bundle must mirror the SDK package exactly.

`skills/recamera-intellisense/scripts/recamera_intellisense/` is materialized
(real copies, not symlinks) by the ClawHub publish flow, so drift is possible.
Loading the stale copy once caused a destructive device operation during
verification — this test makes drift a hard failure instead.
"""

import hashlib
import unittest
from pathlib import Path

SDK_PKG = Path(__file__).resolve().parents[1] / "recamera_intellisense"
BUNDLE = (
    Path(__file__).resolve().parents[2]
    / "skills"
    / "recamera-intellisense"
    / "scripts"
    / "recamera_intellisense"
)


def _digest(path: Path) -> str:
    return hashlib.sha256(path.read_bytes()).hexdigest()


@unittest.skipUnless(BUNDLE.is_dir(), "skill bundle not present (SDK checkout only)")
class BundleSyncTests(unittest.TestCase):
    def test_bundle_mirrors_sdk_package(self) -> None:
        sdk_files = {p.name: p for p in SDK_PKG.glob("*.py")}
        bundle_files = {p.name: p for p in BUNDLE.glob("*.py")}
        self.assertEqual(
            sorted(sdk_files),
            sorted(bundle_files),
            "file sets differ between SDK package and skill bundle",
        )
        stale = [
            name
            for name, src in sdk_files.items()
            if _digest(src) != _digest(bundle_files[name])
        ]
        self.assertEqual(
            stale, [], f"stale bundle copies: {stale}; re-copy from the SDK package"
        )


if __name__ == "__main__":
    unittest.main()
