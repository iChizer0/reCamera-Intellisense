"""Tests for device resolution fallbacks in `_config.resolve`."""

import json
import os
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

from recamera_intellisense import _config
from recamera_intellisense._errors import RecameraError

ENTRY = {
    "host": "192.168.1.10",
    "token": "tok",
    "protocol": "http",
    "allow_unsecured": False,
}
DETECTED = {
    "host": "127.0.0.1",
    "port": 443,
    "protocol": "https",
    "allow_unsecured": True,
}


class ResolveTests(unittest.TestCase):
    def setUp(self) -> None:
        self.tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self.tmp.cleanup)
        root = Path(self.tmp.name)
        patches = [
            patch.object(_config, "RECAMERA_DIR", root / ".recamera"),
            patch.object(_config, "DEVICE_PROFILES_PATH", root / ".recamera" / "devices.json"),
            patch.dict(os.environ, {}, clear=False),
        ]
        for p in patches:
            p.start()
            self.addCleanup(p.stop)
        for var in ("RECAMERA_DEVICE", "RECAMERA_TOKEN", "RECAMERA_HOST", "RECAMERA_PORT"):
            os.environ.pop(var, None)

    def _seed(self, devices: dict) -> None:
        _config.save_all(devices)

    def test_explicit_name_wins_over_env(self) -> None:
        self._seed({"a": dict(ENTRY), "b": dict(ENTRY, host="192.168.1.11")})
        os.environ["RECAMERA_DEVICE"] = "b"
        self.assertEqual(_config.resolve("a")["host"], "192.168.1.10")

    def test_env_device_selects(self) -> None:
        self._seed({"a": dict(ENTRY), "b": dict(ENTRY, host="192.168.1.11")})
        os.environ["RECAMERA_DEVICE"] = "b"
        self.assertEqual(_config.resolve()["host"], "192.168.1.11")

    def test_unknown_env_name_errors(self) -> None:
        self._seed({"a": dict(ENTRY)})
        os.environ["RECAMERA_DEVICE"] = "nope"
        with self.assertRaises(RecameraError):
            _config.resolve()

    def test_sole_device_auto_selected(self) -> None:
        self._seed({"cam": dict(ENTRY)})
        rec = _config.resolve()
        self.assertEqual(rec["name"], "cam")
        self.assertEqual(rec["host"], "192.168.1.10")

    def test_blank_name_counts_as_omitted(self) -> None:
        self._seed({"cam": dict(ENTRY)})
        self.assertEqual(_config.resolve("")["name"], "cam")
        self.assertEqual(_config.resolve("  ")["name"], "cam")

    def test_non_string_name_rejected(self) -> None:
        self._seed({"cam": dict(ENTRY)})
        with self.assertRaisesRegex(RecameraError, "must be a string"):
            _config.resolve(123)  # type: ignore[arg-type]

    def test_multiple_devices_require_a_name(self) -> None:
        self._seed({"a": dict(ENTRY), "b": dict(ENTRY)})
        with self.assertRaisesRegex(RecameraError, "multiple devices"):
            _config.resolve()

    def test_no_devices_no_token_teaches_both_paths(self) -> None:
        with self.assertRaisesRegex(RecameraError, "RECAMERA_TOKEN"):
            _config.resolve()

    def test_local_detect_registers_and_caches(self) -> None:
        os.environ["RECAMERA_TOKEN"] = "secret"
        with patch(
            "recamera_intellisense.device.detect_local_device", return_value=dict(DETECTED)
        ) as detect:
            rec = _config.resolve()
            self.assertEqual(rec["name"], "local")
            self.assertEqual(rec["protocol"], "https")
            self.assertTrue(rec["allow_unsecured"])
            # persisted for subsequent calls: no second detection
            again = _config.resolve()
            self.assertEqual(detect.call_count, 1)
            self.assertEqual(again["host"], "127.0.0.1")
        on_disk = json.loads(_config.DEVICE_PROFILES_PATH.read_text())
        self.assertEqual(on_disk["local"]["token"], "secret")
        self.assertEqual(on_disk["local"]["port"], 443)

    def test_local_detect_unreachable(self) -> None:
        os.environ["RECAMERA_TOKEN"] = "secret"
        with patch("recamera_intellisense.device.detect_local_device", return_value=None):
            with self.assertRaisesRegex(RecameraError, "not reachable"):
                _config.resolve()

    def test_local_detect_honours_host_and_port_env(self) -> None:
        os.environ["RECAMERA_TOKEN"] = "secret"
        os.environ["RECAMERA_HOST"] = "192.168.42.1"
        os.environ["RECAMERA_PORT"] = "8080"
        seen = {}

        def fake(host, port, *, token, timeout=5.0):
            seen.update(host=host, port=port, token=token)
            return {"host": host, "port": port, "protocol": "http", "allow_unsecured": False}

        with patch("recamera_intellisense.device.detect_local_device", side_effect=fake):
            _config.resolve()
        self.assertEqual(seen, {"host": "192.168.42.1", "port": 8080, "token": "secret"})

    def test_invalid_port_env(self) -> None:
        os.environ["RECAMERA_TOKEN"] = "secret"
        os.environ["RECAMERA_PORT"] = "abc"
        with self.assertRaisesRegex(RecameraError, "RECAMERA_PORT"):
            _config.resolve()


if __name__ == "__main__":
    unittest.main()
