"""Tests for the rule module: source_filter round-trips, sed retirement,
record-source discovery, and compile-time rule validation."""

from __future__ import annotations

import unittest
from unittest.mock import patch

from recamera_intellisense import _config, detection, rule

DEV = {"name": "cam1", "host": "h", "token": "", "protocol": "http",
       "allow_unsecured": False, "port": None}

SOURCES_PAYLOAD = {
    "version": 1,
    "sources": [
        {"id": "builtin", "kind": "builtin", "name": "Built-in AI",
         "running": True, "frame_capable": True, "event_capable": False,
         "supports_roi": True, "signals": []},
        {"id": "acousticslab", "kind": "system", "name": "AcousticsLab",
         "running": True, "frame_capable": True, "event_capable": False,
         "supports_roi": False,
         "signals": [{"id": "classification", "type": "classification",
                      "classes": ["Speech", "Cat", "Cat"],
                      "supports_roi": False}]},
        "malformed",
    ],
}


def _resolve(module):
    # `_config` is a shared module; patching its `resolve` covers every
    # command module regardless of which one imported it.
    return patch.object(_config, "resolve", return_value=DEV)


def _sources(**overrides):
    base = {
        "id": "acousticslab", "kind": "system", "name": "AcousticsLab",
        "running": True, "frame_capable": True, "event_capable": False,
        "supports_roi": False, "classes": ["Speech", "Cat"],
    }
    base.update(overrides)
    return [
        {"id": "builtin", "kind": "builtin", "name": "Built-in AI",
         "running": True, "frame_capable": True, "event_capable": False,
         "supports_roi": True, "classes": []},
        base,
    ]


class DetectionRuleCodecTests(unittest.TestCase):
    def test_encode_emits_source_filter(self):
        out = rule._detection_rule_to_json(
            {"name": "al", "source_filter": ["acousticslab"],
             "label_filter": ["Cat"]})
        self.assertEqual(out["lSourceFilter"], ["acousticslab"])
        self.assertEqual(out["lClassFilter"], ["Cat"])

    def test_encode_defaults_source_filter_to_empty(self):
        out = rule._detection_rule_to_json({"name": "x"})
        self.assertEqual(out["lSourceFilter"], [])

    def test_parse_reads_source_filter(self):
        parsed = rule._parse_detection_rule({
            "sID": "al", "iDebounceTimes": 2,
            "lSourceFilter": ["acousticslab"], "lClassFilter": ["Cat"]})
        self.assertEqual(parsed["source_filter"], ["acousticslab"])

    def test_parse_missing_source_filter_is_empty(self):
        parsed = rule._parse_detection_rule({"sID": "x"})
        self.assertEqual(parsed["source_filter"], [])

    def test_roundtrip_preserves_source_binding(self):
        """A migrated AL rule must survive read -> write with its source."""
        device_rule = {
            "sID": "acousticslab", "iDebounceTimes": 3,
            "lConfidenceFilter": [0.5, 1.0], "lClassFilter": ["Cat"],
            "lSourceFilter": ["acousticslab"], "lRegionFilter": [],
        }
        parsed = rule._parse_detection_rule(device_rule)
        out = rule._detection_rule_to_json(parsed)
        self.assertEqual(out["lSourceFilter"], ["acousticslab"])
        self.assertEqual(out["sID"], "acousticslab")


class SedRetirementTests(unittest.TestCase):
    def test_trigger_patch_rejects_sed_with_guidance(self):
        with self.assertRaises(ValueError) as ctx:
            rule.trigger_to_json({"kind": "sed", "label_filter": ["Cat"]})
        msg = str(ctx.exception)
        self.assertIn("retired", msg)
        self.assertIn("acousticslab", msg)

    def test_parse_trigger_rejects_sed_with_guidance(self):
        with self.assertRaises(ValueError) as ctx:
            rule.parse_trigger({"sCurrentSelected": "SED", "dSED": {}})
        self.assertIn("retired", str(ctx.exception))

    def test_merge_no_longer_carries_dsed_sibling(self):
        current = {"sCurrentSelected": "SED",
                   "dSED": {"sID": "old", "lClassFilter": ["Cat"]},
                   "dTimer": {"iIntervalSeconds": 60}}
        out = rule._merge_trigger_payload(current, {"kind": "timer",
                                                    "interval_seconds": 30})
        self.assertNotIn("dSED", out)
        self.assertEqual(out["dTimer"], {"iIntervalSeconds": 30})
        self.assertEqual(out["sCurrentSelected"], "TIMER")


class RecordSourcesTests(unittest.TestCase):
    def test_normalizes_sources_and_dedupes_classes(self):
        with _resolve(rule), patch.object(
                rule._http, "get_json", return_value=dict(SOURCES_PAYLOAD)):
            sources = rule.get_record_sources()
        self.assertEqual(len(sources), 2)  # malformed entry dropped
        builtin, al = sources
        self.assertEqual(builtin["id"], "builtin")
        self.assertEqual(builtin["classes"], [])  # unknowable
        self.assertEqual(al["classes"], ["Speech", "Cat"])  # deduped
        self.assertTrue(al["running"])
        self.assertFalse(al["supports_roi"])


class SetDetectionRulesValidationTests(unittest.TestCase):
    def _run(self, rules, sources):
        captured = {}

        def _capture(device_name, *, trigger):
            captured["trigger"] = trigger

        with _resolve(detection), patch.object(
                detection._rule, "get_record_sources", return_value=sources), \
                patch.object(detection._storage, "ensure_storage"), \
                patch.object(detection._rule, "set_record_trigger", _capture), \
                patch.object(detection._rule, "get_record_config",
                             return_value={"rule_enabled": True,
                                           "writer": {"format": "JPG",
                                                      "interval_ms": 0}}):
            detection.set_detection_rules(rules=rules)
        return captured.get("trigger")

    def test_rules_default_to_builtin_source(self):
        trigger = self._run([{"name": "r", "label_filter": ["person"]}],
                            _sources())
        self.assertEqual(trigger["rules"][0]["source_filter"], ["builtin"])

    def test_builtin_source_skips_label_validation(self):
        # builtin classes follow the selected vision model: unknowable here.
        trigger = self._run([{"name": "r", "label_filter": ["person"]}],
                            _sources())
        self.assertEqual(trigger["rules"][0]["label_filter"], ["person"])

    def test_acoustic_rule_with_producible_label_passes(self):
        trigger = self._run(
            [{"name": "al", "source_filter": ["acousticslab"],
              "label_filter": ["Cat"]}], _sources())
        self.assertEqual(trigger["rules"][0]["source_filter"],
                         ["acousticslab"])

    def test_unknown_source_fails_loudly(self):
        with self.assertRaises(ValueError) as ctx:
            self._run([{"name": "r", "source_filter": ["lidar"],
                        "label_filter": []}], _sources())
        self.assertIn("unknown source_filter", str(ctx.exception))
        self.assertIn("acousticslab", str(ctx.exception))

    def test_unproducible_label_fails_loudly(self):
        with self.assertRaises(ValueError) as ctx:
            self._run([{"name": "al", "source_filter": ["acousticslab"],
                        "label_filter": ["Dog"]}], _sources())
        self.assertIn("Dog", str(ctx.exception))
        self.assertIn("cannot be produced", str(ctx.exception))

    def test_stopped_source_is_reported_in_the_error(self):
        with self.assertRaises(ValueError) as ctx:
            self._run([{"name": "al", "source_filter": ["acousticslab"],
                        "label_filter": ["Cat"]}],
                      _sources(running=False, classes=[]))
        self.assertIn("STOPPED", str(ctx.exception))

    def test_explicit_empty_source_filter_means_all_sources(self):
        # All-source rules include builtin -> label validation skipped.
        trigger = self._run([{"name": "r", "source_filter": [],
                              "label_filter": ["anything"]}], _sources())
        self.assertEqual(trigger["rules"][0]["source_filter"], [])


class GetDetectionRulesTests(unittest.TestCase):
    def _rules_for_raw(self, raw):
        with _resolve(detection), patch.object(
                rule._http, "get_json", return_value=dict(raw)):
            return detection.get_detection_rules()

    def test_returns_empty_when_trigger_is_not_inference_set(self):
        raw = {"sCurrentSelected": "TIMER", "dTimer": {"iIntervalSeconds": 60}}
        self.assertEqual(self._rules_for_raw(raw), [])

    def test_retired_sed_selection_returns_empty_instead_of_raising(self):
        # Legacy dSED survives in-session until the boot migration converts it;
        # the facade contract says non-INFERENCE_SET kinds yield [].
        raw = {"sCurrentSelected": "SED",
               "dSED": {"sID": "", "lClassFilter": ["Cat"]}}
        self.assertEqual(self._rules_for_raw(raw), [])

    def test_returns_rules_with_source_filter(self):
        raw = {"sCurrentSelected": "INFERENCE_SET", "lInferenceSet": [
            {"sID": "acousticslab", "iDebounceTimes": 3,
             "lConfidenceFilter": [0.5, 1.0], "lClassFilter": ["Cat"],
             "lSourceFilter": ["acousticslab"], "lRegionFilter": []}]}
        rules = self._rules_for_raw(raw)
        self.assertEqual(rules[0]["source_filter"], ["acousticslab"])
        self.assertEqual(rules[0]["label_filter"], ["Cat"])


if __name__ == "__main__":
    unittest.main()
