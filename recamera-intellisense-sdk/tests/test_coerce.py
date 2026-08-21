"""Tests for strict boolean coercion at the API boundary."""

from __future__ import annotations

import unittest
from unittest.mock import patch

from recamera_intellisense._coerce import to_bool


class ToBoolTests(unittest.TestCase):
    def test_real_booleans_pass_through(self) -> None:
        self.assertIs(to_bool(True), True)
        self.assertIs(to_bool(False), False)

    def test_int_zero_one(self) -> None:
        self.assertIs(to_bool(1), True)
        self.assertIs(to_bool(0), False)

    def test_string_spellings(self) -> None:
        for raw in ("true", "TRUE", " yes ", "on", "1"):
            self.assertIs(to_bool(raw), True, raw)
        for raw in ("false", "FALSE", " no ", "off", "0"):
            self.assertIs(to_bool(raw), False, raw)

    def test_rejects_silent_truthy_strings(self) -> None:
        """The bool("false") footgun: arbitrary strings must never coerce."""
        for raw in ("maybe", "", "2", "disabled"):
            with self.assertRaises(ValueError, msg=raw):
                to_bool(raw)

    def test_rejects_other_types(self) -> None:
        for raw in (None, 2, [1], {"a": 1}):
            with self.assertRaises(ValueError, msg=repr(raw)):
                to_bool(raw)


class BoolBoundaryTests(unittest.TestCase):
    """Functions must route caller-supplied booleans through to_bool."""

    def test_set_detection_rules_rejects_string_garbage(self) -> None:
        from recamera_intellisense import detection

        with self.assertRaises(ValueError):
            detection.set_detection_rules("cam1", rules=[], ensure_writer="not-a-bool")

    def test_set_detection_rules_string_false_disables_ensures(self) -> None:
        """'false' (string) must mean False, not bool('false') == True."""
        from recamera_intellisense import detection

        calls = {"storage": 0, "trigger": 0, "config_get": 0, "config_set": 0}
        with (
            patch.object(detection._storage, "ensure_storage", lambda *a, **k: calls.__setitem__("storage", calls["storage"] + 1)),
            patch.object(detection._rule, "set_record_trigger", lambda *a, **k: calls.__setitem__("trigger", calls["trigger"] + 1)),
            patch.object(detection._rule, "get_record_config", lambda *a, **k: calls.__setitem__("config_get", calls["config_get"] + 1) or {"rule_enabled": True, "writer": {"format": "JPG", "interval_ms": 0}}),
            patch.object(detection._rule, "set_record_config", lambda *a, **k: calls.__setitem__("config_set", calls["config_set"] + 1)),
        ):
            detection.set_detection_rules("cam1", rules=[], ensure_writer="false", ensure_storage="false")
        self.assertEqual(calls["storage"], 0, "ensure_storage='false' must not run ensure_storage")
        self.assertEqual(calls["config_get"], 0, "ensure_writer='false' must not touch the writer config")
        self.assertEqual(calls["trigger"], 1, "trigger must still be installed")

    def test_set_record_config_rejects_garbage_bool(self) -> None:
        from recamera_intellisense import rule

        with patch.object(rule._config, "resolve", return_value={"host": "h", "token": ""}):
            with self.assertRaises(ValueError):
                rule.set_record_config("cam1", rule_enabled="enabled", writer_format="JPG")

    def test_storage_task_submit_rejects_garbage_sync(self) -> None:
        from recamera_intellisense import storage

        with self.assertRaises(ValueError):
            storage.storage_task_submit("cam1", action="EJECT", dev_path="/dev/x", sync="maybe")


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
