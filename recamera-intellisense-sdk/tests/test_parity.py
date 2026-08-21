"""Single-source-of-truth guarantees: CLI schemas derive from signatures,
the package exports every command, and device records never leak tokens."""

from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path
from unittest.mock import patch

import recamera_intellisense as pkg
from recamera_intellisense import _cli, _config, device


class SchemaDerivationTests(unittest.TestCase):
    def test_schemas_match_signatures_for_all_commands(self) -> None:
        import inspect

        for name, fn in _cli.COMMANDS.items():
            params = inspect.signature(fn).parameters.values()
            expected_req = {p.name for p in params if p.default is inspect.Parameter.empty} - _cli.CLI_EXCLUDE
            expected_opt = {p.name for p in params if p.default is not inspect.Parameter.empty} - _cli.CLI_EXCLUDE
            spec = _cli.COMMAND_SCHEMAS[name]
            self.assertEqual(spec["required"], expected_req, name)
            self.assertEqual(spec["optional"], expected_opt, name)

    def test_raw_is_python_only(self) -> None:
        spec = _cli.COMMAND_SCHEMAS["fetch_file"]
        self.assertNotIn("raw", spec["required"] | spec["optional"])


class ExportParityTests(unittest.TestCase):
    def test_all_commands_are_exported(self) -> None:
        missing = set(_cli.COMMANDS) - set(pkg.__all__)
        self.assertEqual(missing, set())
        for name in pkg.__all__:
            self.assertTrue(callable(getattr(pkg, name, None)), name)


_STORE = {"cam1": {"host": "192.0.2.1", "token": "sk_secret", "protocol": "http", "allow_unsecured": False}}


class TokenHygieneTests(unittest.TestCase):
    def _with_store(self):
        tmp = tempfile.TemporaryDirectory()
        root = Path(tmp.name)
        (root / "devices.json").write_text(json.dumps(_STORE), encoding="utf-8")
        patches = (
            patch.object(_config, "RECAMERA_DIR", root),
            patch.object(_config, "DEVICE_PROFILES_PATH", root / "devices.json"),
        )
        for p in patches:
            p.start()
        return tmp, patches

    def test_public_outputs_strip_token(self) -> None:
        tmp, patches = self._with_store()
        try:
            for record in device.list_devices():
                self.assertNotIn("token", record)
            self.assertNotIn("token", device.get_device("cam1"))
        finally:
            for p in patches:
                p.stop()
            tmp.cleanup()

    def test_transport_keeps_token_internally(self) -> None:
        tmp, patches = self._with_store()
        try:
            self.assertEqual(_config.resolve("cam1")["token"], "sk_secret")
        finally:
            for p in patches:
                p.stop()
            tmp.cleanup()


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
