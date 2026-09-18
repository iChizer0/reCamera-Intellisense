"""Tests for batch-A read surfaces: notify config (secret redaction),
apps listing/logs, video encode, battery, config backup, acoustic models."""

from __future__ import annotations

import os
import tempfile
import unittest
from unittest.mock import patch

from recamera_intellisense import _config, acoustic, apps, backup, notify, system, video

DEV = {"name": "cam1", "host": "h", "token": "", "protocol": "http",
       "allow_unsecured": False, "port": None}


def _resolve():
    return patch.object(_config, "resolve", return_value=DEV)


NOTIFY_CFG = {
    "dHttp": {"sToken": "secret-token", "sUrl": "https://hooks.example/x"},
    "dMqtt": {"iPort": 1883, "sClientId": "rec", "sPassword": "admin",
              "sTopic": "results/data", "sURL": "mqtt://broker", "sUsername": "admin"},
    "dTemplate": {"sClassification": "", "sDetection": "{...}",
                  "sKeypoint": "", "sSegmentation": "", "sTracking": ""},
    "dUart": {"sPort": "ttyS4", "sPortDev": "/dev/shm/vserial1.sock"},
    "iMode": 1,
}

APPS_PAYLOAD = {
    "apps": [
        {"id": "builtin", "type": "builtin", "system": True, "installed": True,
         "name": "AI Model Inference", "name_zh": "AI 模型推理",
         "version": "firmware", "status": "running",
         "manifest": {"name": "AI Model Inference", "description": "Built-in."}},
        {"id": "acousticslab", "type": "system", "system": True,
         "installed": True, "name": "AcousticsLab", "version": "firmware",
         "status": "running", "manifest": {"description": "Audio workbench."}},
        {"id": "yolo-detector", "type": "app", "installed": True,
         "status": "stopped", "manifest": {"name": "YOLO", "description": "d"}},
        "malformed",
    ]
}

ENCODE = {"iEnabled": 1, "iGOP": 120, "iMaxRate": 16384, "id": 0,
          "sFrameRate": "30", "sOutputDataType": "H.264", "sRCMode": "CBR",
          "sRCQuality": "highest", "sResolution": "3840*2160",
          "sStreamType": "mainStream"}

WORKSPACES = {"workspaces": [
    {"id": "ws1", "name": "test", "created_at": "2026-08-27T10:32:51Z"}]}
HEADS = {"heads": [
    {"head_id": "h1", "n_classes": 2, "created_at": "t1", "status": "current"},
    {"head_id": "h2", "n_classes": 5, "created_at": "t2", "status": "current"}]}
ACTIVE = {"runtime_head_id": "h2", "labels": ["_background_noise_", "cat"]}


class NotifyConfigTests(unittest.TestCase):
    def test_shape_and_secret_redaction(self):
        with _resolve(), patch.object(
                notify._http, "get_json", return_value=dict(NOTIFY_CFG)):
            cfg = notify.get_notify_config()
        self.assertEqual(cfg["mode"], 1)
        self.assertEqual(cfg["mode_name"], "mqtt")
        self.assertEqual(cfg["mqtt"]["password"], "***")
        self.assertEqual(cfg["http"]["token"], "***")
        self.assertEqual(cfg["mqtt"]["username"], "admin")  # usernames are not secrets
        self.assertNotIn("secret-token", str(cfg["http"]))
        self.assertEqual(cfg["mqtt"]["url"], "mqtt://broker")
        self.assertEqual(cfg["uart"]["port"], "ttyS4")
        self.assertEqual(cfg["templates"]["detection"], "{...}")

    def test_empty_secrets_stay_empty(self):
        bare = {**NOTIFY_CFG,
                "dHttp": {"sToken": "", "sUrl": ""},
                "dMqtt": {"iPort": 1883, "sPassword": ""},
                "iMode": 0}
        with _resolve(), patch.object(
                notify._http, "get_json", return_value=bare):
            cfg = notify.get_notify_config()
        self.assertEqual(cfg["http"]["token"], "")
        self.assertEqual(cfg["mqtt"]["password"], "")
        self.assertEqual(cfg["mode_name"], "off")


class AppsTests(unittest.TestCase):
    def test_list_apps_normalizes_and_skips_malformed(self):
        with _resolve(), patch.object(
                apps._http, "get_json", return_value=dict(APPS_PAYLOAD)):
            result = apps.list_apps()
        self.assertEqual(len(result), 3)
        self.assertEqual(result[0]["id"], "builtin")
        self.assertTrue(result[0]["system"])
        self.assertEqual(result[2]["name"], "YOLO")  # falls back to manifest
        self.assertEqual(result[2]["status"], "stopped")
        self.assertFalse(result[2]["system"])

    def test_get_app_logs_clamps_tail(self):
        seen = {}

        def _get(dev, path, params=None, **kw):
            seen["path"] = path
            seen["params"] = params
            return {"id": "acousticslab", "lines": ["a", "b"], "text": "a\nb"}

        with _resolve(), patch.object(apps._http, "get_json", _get):
            out = apps.get_app_logs(app_id="acousticslab", tail=99999)
        self.assertEqual(seen["path"], "/api/app-center/v1/apps/acousticslab/logs")
        self.assertEqual(seen["params"], {"tail": "2000"})
        self.assertEqual(out["lines"], ["a", "b"])


class VideoEncodeTests(unittest.TestCase):
    def test_main_stream_normalization(self):
        with _resolve(), patch.object(
                video._http, "get_json", return_value=dict(ENCODE)):
            enc = video.get_video_encode(stream="main")
        self.assertEqual(enc["codec"], "H.264")
        self.assertEqual(enc["resolution"], "3840*2160")
        self.assertEqual(enc["max_rate"], 16384)
        self.assertTrue(enc["enabled"])

    def test_sub_stream_maps_to_id_1(self):
        seen = {}

        def _get(dev, path, **kw):
            seen["path"] = path
            return dict(ENCODE)

        with _resolve(), patch.object(video._http, "get_json", _get):
            video.get_video_encode(stream="sub")
        self.assertIn("/video/1/encode", seen["path"])

    def test_invalid_stream_rejected(self):
        with _resolve():
            with self.assertRaises(ValueError):
                video.get_video_encode(stream="third")


class BatteryTests(unittest.TestCase):
    def test_normalization(self):
        payload = {"displaySteps": 0, "isAttached": False,
                   "isCharging": False, "totalSteps": 5}
        with _resolve(), patch.object(
                system._http, "get_json", return_value=payload):
            b = system.get_battery_status()
        self.assertEqual(b, {"attached": False, "charging": False,
                             "display_steps": 0, "total_steps": 5})


class BackupTests(unittest.TestCase):
    def test_export_writes_local_file(self):
        blob = b"TAR-BYTES" * 100
        with _resolve(), patch.object(
                backup._http, "get_json",
                return_value={"size": len(blob), "url": "/download/config.tar"}), \
                patch.object(backup._http, "get_bytes",
                             return_value=(blob, "application/x-tar")) as gb:
            with tempfile.TemporaryDirectory() as tmp:
                out = backup.export_device_config(
                    output=os.path.join(tmp, "cfg.tar"))
                self.assertEqual(open(out["output"], "rb").read(), blob)
                self.assertEqual(out["size_bytes"], len(blob))
        _, called_url = gb.call_args[0][0], gb.call_args[0][1]
        self.assertEqual(called_url, "/download/config.tar")

    def test_export_requires_output(self):
        with _resolve():
            with self.assertRaises(ValueError):
                backup.export_device_config(output="")


class AppIdValidationTests(unittest.TestCase):
    def test_invalid_app_id_rejected_before_any_http(self):
        with _resolve(), patch.object(
                apps._http, "get_json") as get:
            for bad in ("../etc", "UPPER", "with space", "", "x" * 65, "under_score"):
                with self.assertRaises(ValueError, msg=bad):
                    apps.get_app_logs(app_id=bad)
            get.assert_not_called()

    def test_valid_app_id_passes(self):
        def _get(dev, path, params=None, **kw):
            return {"id": "my-app-1", "lines": [], "text": ""}
        with _resolve(), patch.object(apps._http, "get_json", _get):
            self.assertEqual(apps.get_app_logs(app_id="my-app-1")["id"], "my-app-1")


class BackupOverwriteTests(unittest.TestCase):
    def test_existing_output_refused_without_touching_device(self):
        with _resolve(), tempfile.TemporaryDirectory() as tmp:
            out = os.path.join(tmp, "cfg.tar")
            open(out, "wb").write(b"keep-me")
            with patch.object(backup._http, "get_json") as get:
                with self.assertRaises(ValueError) as ctx:
                    backup.export_device_config(output=out)
            get.assert_not_called()
            self.assertIn("refusing to overwrite", str(ctx.exception))
            self.assertEqual(open(out, "rb").read(), b"keep-me")


class GetToleranceTests(unittest.TestCase):
    def test_none_body_tolerated_like_other_getters(self):
        with _resolve(), patch.object(
                notify._http, "get_json", return_value=None):
            cfg = notify.get_notify_config()
        self.assertEqual(cfg["mode"], 0)
        with _resolve(), patch.object(
                apps._http, "get_json", return_value=None):
            self.assertEqual(apps.list_apps(), [])


class AcousticModelsTests(unittest.TestCase):
    def test_lists_heads_and_marks_active(self):
        def _get(dev, path, **kw):
            if path.endswith("/workspaces"):
                return dict(WORKSPACES)
            if path.endswith("/heads"):
                return dict(HEADS)
            if path.endswith("/active"):
                return dict(ACTIVE)
            raise AssertionError(path)

        with _resolve(), patch.object(acoustic._http, "get_json", _get):
            models = acoustic.list_acoustic_models()
        self.assertEqual(len(models), 2)
        by_id = {m["head_id"]: m for m in models}
        self.assertFalse(by_id["h1"]["active"])
        self.assertTrue(by_id["h2"]["active"])
        self.assertEqual(by_id["h2"]["workspace_name"], "test")

    def test_stopped_app_returns_empty(self):
        from recamera_intellisense._errors import RecameraError
        with _resolve(), patch.object(
                acoustic._http, "get_json", side_effect=RecameraError("down")):
            self.assertEqual(acoustic.list_acoustic_models(), [])


if __name__ == "__main__":
    unittest.main()
