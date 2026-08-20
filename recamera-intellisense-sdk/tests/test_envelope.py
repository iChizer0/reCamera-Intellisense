"""Device error-envelope handling: HTTP 200 + {"code": N} must raise, not parse as data."""

from __future__ import annotations

import json
import unittest
from unittest.mock import patch

from recamera_intellisense import _http
from recamera_intellisense._errors import RecameraError

ERROR_BODY = {"code": 500, "message": "Backend connection failed: -1"}
DEV = {"name": "cam1", "host": "h", "token": "", "protocol": "http", "allow_unsecured": False, "port": None}


def _respond(body):
    return patch.object(
        _http, "_request", return_value=(json.dumps(body).encode(), "application/json")
    )


class GetJsonEnvelopeTests(unittest.TestCase):
    def test_error_envelope_raises(self) -> None:
        with _respond(ERROR_BODY):
            with self.assertRaises(RecameraError) as ctx:
                _http.get_json(DEV, "/x")
        self.assertIn("500", str(ctx.exception))
        self.assertIn("Backend connection failed", str(ctx.exception))

    def test_code_zero_and_codeless_payloads_pass(self) -> None:
        with _respond({"code": 0, "data": 1}):
            self.assertEqual(_http.get_json(DEV, "/x"), {"code": 0, "data": 1})
        with _respond({"lSlots": []}):
            self.assertEqual(_http.get_json(DEV, "/x"), {"lSlots": []})

    def test_list_and_empty_payloads_pass(self) -> None:
        with _respond([]):
            self.assertEqual(_http.get_json(DEV, "/x"), [])
        with patch.object(_http, "_request", return_value=(b"", "")):
            self.assertIsNone(_http.get_json(DEV, "/x"))


class GetterPropagationTests(unittest.TestCase):
    """Representative getters must propagate the transport-level envelope error."""

    def _check(self, module, fn, *args, **kwargs) -> None:
        with (
            patch.object(module._config, "resolve", return_value=DEV),
            _respond(ERROR_BODY),
        ):
            with self.assertRaises(RecameraError, msg=fn.__name__):
                fn("cam1", *args, **kwargs)

    def test_getters_raise(self) -> None:
        from recamera_intellisense import acoustic, capture, files, gpio, model, rule, storage

        self._check(storage, storage.get_storage_status)
        self._check(model, model.get_detection_models_info)
        self._check(model, model.get_detection_model)
        self._check(rule, rule.get_rule_system_info)
        self._check(rule, rule.get_record_config)
        self._check(rule, rule.get_schedule_rule)
        self._check(rule, rule.get_record_trigger)
        self._check(capture, capture.get_capture_status)
        self._check(gpio, gpio.list_gpios)
        self._check(gpio, gpio.get_gpio_info, pin_id=106)
        self._check(acoustic, acoustic.get_active_acoustic_model)
        self._check(files, files.get_intellisense_events)

    def test_success_payloads_still_parse(self) -> None:
        from recamera_intellisense import model, storage

        with (
            patch.object(storage._config, "resolve", return_value=DEV),
            _respond({"sDataDirName": "reCamera", "lSlots": []}),
        ):
            self.assertEqual(storage.get_storage_status("cam1"), [])
        with (
            patch.object(model._config, "resolve", return_value=DEV),
            _respond([{"model": "m", "modelInfo": {"classes": []}}]),
        ):
            self.assertEqual(model.get_detection_models_info("cam1")[0]["name"], "m")


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
