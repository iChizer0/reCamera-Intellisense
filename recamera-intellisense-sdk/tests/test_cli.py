"""Regression tests for the SDK command-line dispatcher."""

from __future__ import annotations

import contextlib
import io
import json
import shlex
import tempfile
import unittest
from pathlib import Path
from typing import Dict, List, Optional
from unittest.mock import patch

from recamera_intellisense import _cli


def _run(argv, commands, schemas=None):
    """Run main() with patched commands; capture stdout/stderr/status."""
    stdout, stderr = io.StringIO(), io.StringIO()
    schemas = schemas or {name: {"required": set(), "optional": set()} for name in commands}
    with (
        patch.object(_cli, "COMMANDS", commands),
        patch.object(_cli, "COMMAND_SCHEMAS", schemas),
        contextlib.redirect_stdout(stdout),
        contextlib.redirect_stderr(stderr),
    ):
        status = _cli.main(argv)
    return status, stdout.getvalue(), stderr.getvalue()


class CliSerializationTests(unittest.TestCase):
    def test_none_result_serializes_as_json_null(self) -> None:
        stdout = io.StringIO()
        with (
            patch.object(_cli, "COMMANDS", {"returns_none": lambda: None}),
            patch.object(_cli, "COMMAND_SCHEMAS", {"returns_none": {"required": set(), "optional": set()}}),
            contextlib.redirect_stdout(stdout),
        ):
            status = _cli.main(["returns_none"])
        self.assertEqual(status, 0)
        self.assertEqual(stdout.getvalue(), "null\n")

    def test_empty_object_result_stays_an_empty_json_object(self) -> None:
        status, out, _ = _run(["returns_object"], {"returns_object": lambda: {}})
        self.assertEqual(status, 0)
        self.assertEqual(out, "{}\n")


class _Recorder:
    """Fake command capturing its kwargs, with realistic annotations."""

    def __init__(self):
        self.kwargs = None

    def __call__(
        self,
        device_name: str,
        count: int = 0,
        ratio: float = 0.0,
        enabled: bool = False,
        maybe: Optional[int] = None,
        items: Optional[List[Dict[str, str]]] = None,
        payload: Optional[Dict[str, object]] = None,
    ) -> None:
        self.kwargs = dict(
            device_name=device_name,
            count=count,
            ratio=ratio,
            enabled=enabled,
            maybe=maybe,
            items=items,
            payload=payload,
        )
        return None


def _recorded(argv):
    rec = _Recorder()
    schemas = {"cmd": {"required": {"device_name"}, "optional": {"count", "ratio", "enabled", "maybe", "items", "payload"}}}
    status, out, err = _run(["cmd", *argv], {"cmd": rec}, schemas)
    return status, rec.kwargs, err


class FlatArgumentParsingTests(unittest.TestCase):
    def test_key_value_pairs(self) -> None:
        status, kw, _ = _recorded(["device_name=cam1", "count=3", "ratio=0.5", "enabled=true"])
        self.assertEqual(status, 0)
        self.assertEqual(kw["device_name"], "cam1")
        self.assertEqual(kw["count"], 3)
        self.assertEqual(kw["ratio"], 0.5)
        self.assertIs(kw["enabled"], True)

    def test_dash_flag_space_and_equals_forms(self) -> None:
        status, kw, _ = _recorded(["--device-name", "cam1", "--count=7"])
        self.assertEqual(status, 0)
        self.assertEqual(kw["device_name"], "cam1")
        self.assertEqual(kw["count"], 7)

    def test_bool_spellings(self) -> None:
        for raw, expected in [("true", True), ("yes", True), ("on", True), ("1", True),
                              ("false", False), ("no", False), ("off", False), ("0", False)]:
            status, kw, _ = _recorded(["device_name=c", f"enabled={raw}"])
            self.assertEqual(status, 0, raw)
            self.assertIs(kw["enabled"], expected, raw)

    def test_bool_rejects_garbage(self) -> None:
        status, _, err = _recorded(["device_name=c", "enabled=maybe"])
        self.assertEqual(status, 2)
        self.assertIn("boolean", err)
        self.assertIn("usage:", err)

    def test_null_optional(self) -> None:
        status, kw, _ = _recorded(["device_name=c", "maybe=null"])
        self.assertEqual(status, 0)
        self.assertIsNone(kw["maybe"])

    def test_negative_number(self) -> None:
        status, kw, _ = _recorded(["device_name=c", "count=-1"])
        self.assertEqual(status, 0)
        self.assertEqual(kw["count"], -1)

    def test_inline_json_array(self) -> None:
        status, kw, _ = _recorded(["device_name=c", 'items=[{"name":"person"}]'])
        self.assertEqual(status, 0)
        self.assertEqual(kw["items"], [{"name": "person"}])

    def test_inline_json_object(self) -> None:
        status, kw, _ = _recorded(["device_name=c", 'payload={"kind":"timer","interval_seconds":60}'])
        self.assertEqual(status, 0)
        self.assertEqual(kw["payload"], {"kind": "timer", "interval_seconds": 60})

    def test_at_file_value(self) -> None:
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "trigger.json"
            path.write_text('{"kind": "always_on"}', encoding="utf-8")
            status, kw, _ = _recorded(["device_name=c", f"payload=@{path}"])
        self.assertEqual(status, 0)
        self.assertEqual(kw["payload"], {"kind": "always_on"})

    def test_single_json_object_still_supported(self) -> None:
        status, kw, _ = _recorded(['{"device_name":"cam1","count":"4","enabled":"false","maybe":null}'])
        self.assertEqual(status, 0)
        self.assertEqual(kw["count"], 4)          # string coerced via annotation
        self.assertIs(kw["enabled"], False)       # "false" string coerced, NOT bool("false")
        self.assertIsNone(kw["maybe"])

    def test_mixed_json_and_kv_rejected(self) -> None:
        status, _, err = _recorded(['{"device_name":"c"}', "count=1"])
        self.assertEqual(status, 2)
        self.assertIn("cannot mix", err)

    def test_bare_token_rejected(self) -> None:
        status, _, err = _recorded(["device_name"])
        self.assertEqual(status, 2)
        self.assertIn("key=value", err)

    def test_duplicate_key_rejected(self) -> None:
        status, _, err = _recorded(["device_name=a", "--device-name", "b"])
        self.assertEqual(status, 2)
        self.assertIn("duplicate", err)

    def test_missing_required_prints_usage_and_example(self) -> None:
        status, _, err = _recorded([])
        self.assertEqual(status, 2)
        self.assertIn("missing required argument", err)
        self.assertIn("usage:", err)

    def test_unknown_key_prints_usage(self) -> None:
        status, _, err = _recorded(["device_name=c", "bogus=1"])
        self.assertEqual(status, 2)
        self.assertIn("unknown argument", err)
        self.assertIn("usage:", err)

    def test_invalid_json_suggests_flat_form(self) -> None:
        status, _, err = _recorded(["{'device_name':'c'}"])
        self.assertEqual(status, 2)
        self.assertIn("invalid JSON", err)
        self.assertIn("key=value", err)


class CommandHelpTests(unittest.TestCase):
    def test_per_command_help(self) -> None:
        status, out, _ = _run(
            ["get_device", "--help"],
            {"get_device": lambda device_name: None},
            {"get_device": {"required": {"device_name"}, "optional": set()}},
        )
        self.assertEqual(status, 0)
        self.assertIn("usage: recamera get_device device_name=", out)

    def test_every_real_command_has_an_example(self) -> None:
        missing = set(_cli.COMMANDS) - set(_cli.EXAMPLES)
        self.assertEqual(missing, set())

    def test_examples_cover_all_commands_and_parse(self) -> None:
        """Every curated example must at least tokenize into valid key=value args."""
        for name, example in _cli.EXAMPLES.items():
            tokens = example.split()[1:]  # drop 'recamera'
            self.assertEqual(tokens[0], name)
            for tok in tokens[1:]:
                if tok.lstrip().startswith("{"):
                    continue
                body = tok[2:] if tok.startswith("--") else tok
                self.assertIn("=", body, f"{name}: example token {tok!r} is not key=value")

    def test_examples_validate_against_real_schemas(self) -> None:
        """Examples are parsed with shlex through the real parser + validator,
        so help text can never drift from the implementation."""
        for name, example in _cli.EXAMPLES.items():
            argv = shlex.split(example)[1:]  # drop 'recamera'
            self.assertEqual(argv[0], name)
            kwargs = _cli._parse_cli(name, argv[1:])
            kwargs = _cli._apply_aliases(name, kwargs)
            _cli._validate(name, kwargs)  # raises _UsageError on drift


class HelpLabelTests(unittest.TestCase):
    def test_optional_params_get_unwrapped_labels(self) -> None:
        help_text = _cli._command_help("update_device")
        self.assertIn("allow_unsecured=<true|false>", help_text)
        self.assertIn("port=<int>", help_text)
        self.assertIn("host=<string>", help_text)

    def test_optional_list_label_is_json_array(self) -> None:
        help_text = _cli._command_help("set_schedule_rule")
        self.assertIn("schedule=<json-array>", help_text)


class StringCoercionTests(unittest.TestCase):
    def test_str_param_rejects_structured_and_bool_values(self) -> None:
        for bad in (True, False, None, [1], {"a": 1}):
            with self.assertRaises(_cli._UsageError, msg=repr(bad)):
                _cli._coerce("cmd", "token", bad, str)

    def test_str_param_accepts_plain_numbers(self) -> None:
        self.assertEqual(_cli._coerce("cmd", "name", 42, str), "42")
        self.assertEqual(_cli._coerce("cmd", "name", 4.2, str), "4.2")


class AtFileEscapeTests(unittest.TestCase):
    def test_double_at_is_a_literal_at(self) -> None:
        self.assertEqual(_cli._load_at_value("@@not-a-file"), "@not-a-file")

    def test_at_file_roundtrip_through_parser(self) -> None:
        rec = _Recorder()
        schemas = {"cmd": {"required": {"device_name"}, "optional": {"count", "ratio", "enabled", "maybe", "items", "payload"}}}
        with tempfile.TemporaryDirectory() as tmp:
            path = Path(tmp) / "v.json"
            path.write_text('[{"k": "v"}]\n', encoding="utf-8")
            status, out, err = _run(["cmd", "device_name=c", f"items=@{path}"], {"cmd": rec}, schemas)
        self.assertEqual(status, 0, err)
        self.assertEqual(rec.kwargs["items"], [{"k": "v"}])


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
