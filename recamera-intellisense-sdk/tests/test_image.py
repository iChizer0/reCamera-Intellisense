"""Tests for the system and image (ISP) modules."""

from __future__ import annotations

import json
import unittest
from unittest.mock import patch

from recamera_intellisense import image, system
from recamera_intellisense._errors import RecameraError

DEV = {"name": "cam1", "host": "h", "token": "", "protocol": "http", "allow_unsecured": False, "port": None}

DEVICE_INFO = {
    "sSerialNumber": "unknown",
    "sFirmwareVersion": "V1.1.1",
    "sSensorModel": "SC850SL",
    "sBasePlateModel": "Base Board-V1.0",
}
RESOURCE_INFO = {
    "iCpuUsage": 5,
    "iNpuUsage": 20,
    "sMem": {"iMemTotal": 1.94, "iMemUsage": 30, "iMemUsed": 0.59},
    "sStorage": {"iStorageTotal": 11.29, "iStorageUsage": 38, "iStorageUsed": 4.35},
}
TIME = {
    "dNtpConfig": {"sAddress": "pool.ntp.org", "sPort": "123", "status": 0},
    "iTimestamp": 1787220510,
    "sMethod": "ntp",
    "sTimezone": "UTC",
    "sTz": "UTC+0",
}
IMAGE_CONFIG = {
    "id": 0,
    "videoAdjustment": {
        "iImageRotation": 0,
        "sImageFlip": "close",
        "sPowerLineFrequencyMode": "NTSC(60HZ)",
    },
    "nightToDay": {
        "iMode": 0,
        "iNightToDayFilterLevel": 0,
        "iNightToDayFilterTime": 5,
        "iDawnTime": 28800,
        "iDuskTime": 64800,
        "iProfileSelect": 0,
        "iProfileCur": 1,
    },
    "profile": [
        {
            "imageAdjustment": {"iBrightness": 50, "iContrast": 50, "iHue": 50, "iSaturation": 50, "iSharpness": 50},
            "exposure": {"iExposureGain": 1, "sExposureMode": "auto", "sExposureTime": "1/6", "sGainMode": "auto"},
            "BLC": {"sBLCRegion": "close", "iBLCStrength": 1, "iDarkBoostLevel": 50, "sHDR": "close", "iHDRLevel": 1, "sHLC": "close", "iHLCLevel": 1},
            "whiteBlance": {"iWhiteBalanceCT": 2800, "sWhiteBlanceStyle": "auto"},
            "imageEnhancement": {"iNoiseReduceMode": 1, "iSpatialDenoiseLevel": 50, "iTemporalDenoiseLevel": 50},
        }
        for _ in range(3)
    ],
}


def _resolve(module):
    return patch.object(module._config, "resolve", return_value=DEV)


class SystemTests(unittest.TestCase):
    def test_get_device_info(self) -> None:
        with _resolve(system), patch.object(system._http, "get_json", return_value=dict(DEVICE_INFO)):
            info = system.get_device_info("cam1")
        self.assertEqual(info["firmware_version"], "V1.1.1")
        self.assertEqual(info["sensor_model"], "SC850SL")

    def test_get_resource_info_normalizes_nested_blocks(self) -> None:
        with _resolve(system), patch.object(system._http, "get_json", return_value=dict(RESOURCE_INFO)):
            info = system.get_resource_info("cam1")
        self.assertEqual(info["npu_usage"], 20)
        self.assertEqual(info["memory"], {"total_gb": 1.94, "used_gb": 0.59, "usage_percent": 30})
        self.assertEqual(info["storage"]["usage_percent"], 38)

    def test_get_system_time(self) -> None:
        with _resolve(system), patch.object(system._http, "get_json", return_value=dict(TIME)):
            t = system.get_system_time("cam1")
        self.assertEqual(t["ntp"], {"address": "pool.ntp.org", "port": "123"})
        self.assertEqual(t["tz"], "UTC+0")

    def test_reboot_posts_and_checks_envelope(self) -> None:
        with _resolve(system), patch.object(system._http, "post_json", return_value={"code": 0}) as post:
            self.assertIsNone(system.reboot_device("cam1"))
        self.assertEqual(post.call_args.args[1], system.PATH_REBOOT)
        with _resolve(system), patch.object(system._http, "post_json", return_value={"code": 500, "message": "x"}):
            with self.assertRaises(RecameraError):
                system.reboot_device("cam1")


class GetImageSettingsTests(unittest.TestCase):
    def test_normalizes_full_config(self) -> None:
        with _resolve(image), patch.object(image._http, "get_json", return_value=json.loads(json.dumps(IMAGE_CONFIG))):
            cfg = image.get_image_settings("cam1")
        self.assertEqual(cfg["video_adjustment"]["rotation"], 0)
        self.assertEqual(cfg["video_adjustment"]["power_line_frequency"], "NTSC(60HZ)")
        self.assertEqual(cfg["night_to_day"]["dawn_time"], 28800)
        self.assertEqual(cfg["night_to_day"]["profile_current"], 1)
        self.assertEqual(len(cfg["profiles"]), 3)
        p0 = cfg["profiles"][0]
        self.assertEqual(p0["adjustment"]["brightness"], 50)
        self.assertEqual(p0["backlight"]["blc_region"], "close")
        self.assertEqual(p0["white_balance"]["color_temperature"], 2800)
        self.assertEqual(p0["enhancement"]["noise_reduce_mode"], 1)


def _run_set(section, values, scene_id=None, config=None):
    """Run set_image_settings with mocked IO; return (put_path, put_payload)."""
    calls = {}
    def fake_put(dev, path, payload=None, **kw):
        calls["path"], calls["payload"] = path, payload
        return {"code": 0}
    with (
        _resolve(image),
        patch.object(image._http, "get_json", return_value=json.loads(json.dumps(config or IMAGE_CONFIG))),
        patch.object(image._http, "put_json", side_effect=fake_put),
    ):
        image.set_image_settings("cam1", section=section, values=values, scene_id=scene_id)
    return calls["path"], calls["payload"]


class SetImageSettingsTests(unittest.TestCase):
    def test_unknown_section_and_field_rejected(self) -> None:
        with self.assertRaises(ValueError):
            image.set_image_settings("cam1", section="nope", values={"x": 1})
        with self.assertRaises(ValueError):
            image.set_image_settings("cam1", section="adjustment", values={"nope": 1}, scene_id=0)

    def test_scene_id_rules(self) -> None:
        with self.assertRaises(ValueError):
            image.set_image_settings("cam1", section="adjustment", values={"brightness": 50})
        with self.assertRaises(ValueError):
            image.set_image_settings("cam1", section="video_adjustment", values={"rotation": 0}, scene_id=1)

    def test_range_enum_and_fraction_validation(self) -> None:
        with self.assertRaises(ValueError):  # out of range
            image.set_image_settings("cam1", section="adjustment", values={"brightness": 101}, scene_id=0)
        with self.assertRaises(ValueError):  # bad enum
            image.set_image_settings("cam1", section="video_adjustment", values={"rotation": 45})
        with self.assertRaises(ValueError):  # bad fraction
            image.set_image_settings("cam1", section="exposure", values={"exposure_time": "6"}, scene_id=0)
        with self.assertRaises(ValueError):  # bool is not an int
            image.set_image_settings("cam1", section="adjustment", values={"brightness": True}, scene_id=0)

    def test_bool_rejected_for_int_fields_and_enums(self) -> None:
        with self.assertRaises(ValueError):  # False == 0 would pass the rotation enum
            image.set_image_settings("cam1", section="video_adjustment", values={"rotation": False})
        with self.assertRaises(ValueError):  # True == 1 would pass the hdr_level enum
            image.set_image_settings("cam1", section="backlight", values={"hdr_level": True}, scene_id=0)

    def test_malformed_profile_entry_raises_value_error(self) -> None:
        bad = json.loads(json.dumps(IMAGE_CONFIG))
        bad["profile"][1] = None
        with self.assertRaises(ValueError):
            _run_set("adjustment", {"brightness": 50}, scene_id=1, config=bad)

    def test_read_modify_write_merges_and_puts_full_section(self) -> None:
        path, payload = _run_set("adjustment", {"brightness": 80}, scene_id=2)
        self.assertEqual(path, "/cgi-bin/entry.cgi/image/0/2/adjustment")
        self.assertEqual(payload["iBrightness"], 80)
        self.assertEqual(payload["iContrast"], 50)  # untouched fields preserved

    def test_global_sections_put_without_scene(self) -> None:
        path, payload = _run_set("video_adjustment", {"rotation": 180})
        self.assertEqual(path, "/cgi-bin/entry.cgi/image/0/video-adjustment")
        self.assertEqual(payload["iImageRotation"], 180)
        self.assertEqual(payload["sImageFlip"], "close")

    def test_backlight_mutual_exclusion_checked_after_merge(self) -> None:
        with self.assertRaises(ValueError):
            _run_set("backlight", {"hdr": "open", "hlc": "open"}, scene_id=0)
        # single open against an all-close current config is fine
        _, payload = _run_set("backlight", {"hdr": "open"}, scene_id=0)
        self.assertEqual(payload["sHDR"], "open")

    def test_night_to_day_dusk_must_follow_dawn(self) -> None:
        with self.assertRaises(ValueError):
            _run_set("night_to_day", {"dawn_time": 70000})  # current dusk is 64800
        _, payload = _run_set("night_to_day", {"dusk_time": 60000})
        self.assertEqual(payload["iDuskTime"], 60000)

    def test_white_blance_uses_device_spelling(self) -> None:
        path, payload = _run_set("white_balance", {"color_temperature": 5000}, scene_id=1)
        self.assertEqual(path, "/cgi-bin/entry.cgi/image/0/1/white-blance")
        self.assertEqual(payload["iWhiteBalanceCT"], 5000)

    def test_error_envelope_raises(self) -> None:
        with (
            _resolve(image),
            patch.object(image._http, "get_json", return_value=json.loads(json.dumps(IMAGE_CONFIG))),
            patch.object(image._http, "put_json", return_value={"code": 30001, "message": "bad"}),
        ):
            with self.assertRaises(RecameraError):
                image.set_image_settings("cam1", section="adjustment", values={"brightness": 50}, scene_id=0)


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
