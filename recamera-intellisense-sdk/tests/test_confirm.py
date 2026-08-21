"""Destructive-operation confirmation gates and security warnings."""

import contextlib
import io
import unittest
from unittest.mock import patch

from recamera_intellisense import _http, files, storage, system
from recamera_intellisense._errors import RecameraError


class ConfirmGateTests(unittest.TestCase):
    def test_storage_task_submit_requires_confirm(self) -> None:
        with patch.object(storage._http, "post_json") as post:
            with self.assertRaisesRegex(RecameraError, "confirm=true"):
                storage.storage_task_submit("cam1", action="FORMAT", dev_path="/dev/x")
        post.assert_not_called()

    def test_storage_task_submit_with_confirm_proceeds(self) -> None:
        with (
            patch.object(storage._config, "resolve", return_value={"name": "cam1"}),
            patch.object(storage._http, "post_json", return_value={"code": 0}) as post,
        ):
            storage.storage_task_submit("cam1", action="EJECT", dev_path="/dev/x", confirm=True)
        self.assertEqual(post.call_count, 1)

    def test_storage_task_submit_confirm_is_strict_bool(self) -> None:
        with self.assertRaises(ValueError):
            storage.storage_task_submit(
                "cam1", action="EJECT", dev_path="/dev/x", confirm="maybe"
            )

    def test_delete_file_requires_confirm(self) -> None:
        with patch.object(files._http, "delete") as delete:
            with self.assertRaisesRegex(RecameraError, "confirm=true"):
                files.delete_file("cam1", path="/mnt/data/rec/x.jpg")
        delete.assert_not_called()

    def test_reboot_requires_confirm(self) -> None:
        with patch.object(system._http, "post_json") as post:
            with self.assertRaisesRegex(RecameraError, "confirm=true"):
                system.reboot_device("cam1")
        post.assert_not_called()


class InsecureTlsWarningTests(unittest.TestCase):
    def setUp(self) -> None:
        self._saved = _http._WARNED_INSECURE
        _http._WARNED_INSECURE = False

    def tearDown(self) -> None:
        _http._WARNED_INSECURE = self._saved

    def test_warns_once_per_process(self) -> None:
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            _http.insecure_ssl_context("192.168.1.10")
            _http.insecure_ssl_context("192.168.1.11")
        out = err.getvalue()
        self.assertEqual(out.count("TLS verification disabled"), 1)
        self.assertIn("192.168.1.10", out)

    def test_ssl_context_uses_warning_path(self) -> None:
        device = {"host": "cam", "protocol": "https", "allow_unsecured": True}
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            ctx = _http._ssl_context(device)
        self.assertIs(ctx, _http._INSECURE_SSL)
        self.assertIn("TLS verification disabled", err.getvalue())

    def test_verified_devices_stay_silent(self) -> None:
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            self.assertIsNone(
                _http._ssl_context({"host": "cam", "protocol": "https"})
            )
        self.assertEqual(err.getvalue(), "")

    def test_http_device_with_allow_unsecured_does_not_warn(self) -> None:
        # Fallback context must be lazy: only a real https hop may warn.
        device = {"host": "cam", "protocol": "http", "allow_unsecured": True}
        err = io.StringIO()
        with contextlib.redirect_stderr(err):
            ctx = _http._ssl_context(device)
            self.assertIsNone(ctx)
            allow_fallback = ctx is None and bool(device.get("allow_unsecured", False))
            self.assertTrue(allow_fallback)
        self.assertEqual(err.getvalue(), "")


class EnsureStorageNoticeTests(unittest.TestCase):
    def _slots(self, enabled: bool, rotate: bool) -> list:
        return [
            {
                "dev_path": storage.DEFAULT_INTERNAL_DEV_PATH,
                "enabled": enabled,
                "state": "IDLE",
                "quota_rotate": rotate,
                "quota_limit_bytes": 1024,
            }
        ]

    def test_notice_when_enabling_slot_and_rotation(self) -> None:
        calls = []
        with (
            patch.object(storage, "get_storage_status", side_effect=[self._slots(False, False), self._slots(True, False)]),
            patch.object(storage, "set_storage_slot", lambda *a, **k: calls.append("slot")),
            patch.object(storage, "configure_storage_quota", lambda *a, **k: calls.append("quota")),
            patch.object(storage.time, "sleep", lambda *_: None),
        ):
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                storage.ensure_storage("cam1")
        self.assertEqual(calls, ["slot", "quota"])
        self.assertIn("auto-enabled internal storage slot", err.getvalue())

    def test_notice_when_only_rotation_missing(self) -> None:
        with (
            patch.object(storage, "get_storage_status", return_value=self._slots(True, False)),
            patch.object(storage, "configure_storage_quota") as quota,
        ):
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                storage.ensure_storage("cam1")
        quota.assert_called_once()
        self.assertIn("quota rotation", err.getvalue())

    def test_silent_when_already_configured(self) -> None:
        with patch.object(storage, "get_storage_status", return_value=self._slots(True, True)):
            err = io.StringIO()
            with contextlib.redirect_stderr(err):
                storage.ensure_storage("cam1")
        self.assertEqual(err.getvalue(), "")


if __name__ == "__main__":
    unittest.main()
