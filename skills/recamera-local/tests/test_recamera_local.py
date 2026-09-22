#!/usr/bin/env python3
"""Offline tests for the local reCamera skill — no hardware required.

A threaded `http.server` impersonates the device API (stateful rule config,
capture lifecycle, event store, GPIO pins, ISP config), and a tmpdir stands
in for the `/mnt` storage mount. Endpoint resolution is pointed at the fake
via RECAMERA_HOST/PORT/PROTOCOL/TOKEN env vars.
"""

from __future__ import annotations

import base64
import json
import os
import subprocess
import sys
import tempfile
import threading
import time
import unittest
import unittest.mock
import urllib.parse
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path

SKILL_ROOT = Path(__file__).resolve().parent.parent
sys.path.insert(0, str(SKILL_ROOT / "scripts"))

import recamera_local as rl  # noqa: E402
from recamera_local import _local  # noqa: E402
from recamera_local._errors import RecameraError  # noqa: E402

TOKEN = "test-token"


class _State:
    def __init__(self, mnt: Path):
        self.mnt = mnt
        self.record_root = mnt / "rc_mmcblk0p8" / "reCamera"
        self.record_root.mkdir(parents=True, exist_ok=True)
        self.events = []  # daemon event store
        self.capture = {"ready": True, "last": None}
        self.rule_config = {"bRuleEnabled": False, "dWriterConfig": {"sFormat": "MP4", "iIntervalMs": 5000}}
        self.schedule = {"bEnabled": False, "lActiveWeekdays": []}
        self.record_rule = {"sCurrentSelected": "ALWAYS_ON", "dTimer": {"iIntervalSeconds": 60}}
        self.rebooted = False
        self.image_puts = []
        self.gpio = {
            106: {"settings": {"state": "disabled", "edge": "none", "debounce_ms": 0}, "value": 0},
        }
        self.storage_enabled = True
        self.http_activations = 0
        self.sources = {"sources": [
            {"id": "builtin", "kind": "builtin", "name": "Built-in Vision",
             "running": True, "frame_capable": True, "event_capable": True,
             "supports_roi": True, "signals": []},
            {"id": "acousticslab", "kind": "system", "name": "AcousticsLab",
             "running": True, "frame_capable": False, "event_capable": True,
             "supports_roi": False,
             "signals": [{"type": "classification", "classes": ["Cat", "Dog"]}]},
        ]}
        self.apps = [
            {"id": "acousticslab", "name": "AcousticsLab", "version": "1.2",
             "status": "running", "system": True, "installed": True},
            {"id": "counter", "name": "People Counter", "version": "0.3",
             "status": "stopped", "system": False, "installed": True},
        ]
        self.app_actions = []
        self.battery = {"isAttached": True, "isCharging": False,
                        "displaySteps": 4, "totalSteps": 5}
        self.notify = {
            "iMode": 0,
            "dMqtt": {"sURL": "", "iPort": 1883, "sClientId": "", "sUsername": "",
                      "sPassword": "stored-secret", "sTopic": ""},
            "dHttp": {"sUrl": "", "sToken": ""},
            "dUart": {"sPort": "", "sPortDev": ""},
            "dTemplate": {"sDetection": "", "sClassification": ""},
        }
        self.video = {
            0: {"sStreamType": "main", "iEnabled": 1, "sOutputDataType": "H.264",
                "sResolution": "1920*1080", "sFrameRate": "30", "iGOP": 30,
                "sRCMode": "CBR", "sRCQuality": "high", "iMaxRate": 4096},
            1: {"sStreamType": "sub", "iEnabled": 1, "sOutputDataType": "H.265",
                "sResolution": "640*480", "sFrameRate": "15", "iGOP": 15,
                "sRCMode": "VBR", "sRCQuality": "medium", "iMaxRate": 512},
        }
        self.workspaces = {"workspaces": [{"id": "ws1", "name": "Default"}]}
        self.heads = {"ws1": {"heads": [{"head_id": "head_abc", "n_classes": 2,
                                         "status": "ready"}]}}
        self.active_head = {"runtime_head_id": "head_abc", "labels": ["Cat", "Dog"]}


class FakeDevice(BaseHTTPRequestHandler):
    state: _State  # set on the server instance

    def log_message(self, *args):  # silence
        pass

    # -- helpers ---------------------------------------------------------

    def _check_auth(self):
        # The skill must send the explicit Bearer prefix (appmgr's origin guard).
        if self.headers.get("Authorization", "") != f"Bearer {TOKEN}":
            self.send_response(401)
            self.end_headers()
            return False
        return True

    def _json(self, obj, status=200):
        body = json.dumps(obj).encode()
        self.send_response(status)
        self.send_header("Content-Type", "application/json")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _text(self, text, status=200):
        body = text.encode()
        self.send_response(status)
        self.send_header("Content-Type", "text/plain")
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)

    def _body(self):
        length = int(self.headers.get("Content-Length", 0) or 0)
        raw = self.rfile.read(length) if length else b""
        if not raw:
            return None
        ct = self.headers.get("Content-Type", "")
        if "json" in ct:
            return json.loads(raw.decode())
        return raw.decode()

    @property
    def st(self) -> _State:
        return self.server.state  # type: ignore[attr-defined]

    # -- routing ---------------------------------------------------------

    def do_GET(self):
        if not self._check_auth():
            return
        u = urllib.parse.urlsplit(self.path)
        path, q = u.path, urllib.parse.parse_qs(u.query)
        st = self.st

        if path == "/api/v1/recamera-generate-204":
            self.send_response(204)
            self.end_headers()
        elif path == "/slow":
            time.sleep(3)  # lets clients exercise real socket timeouts
            self._json({})
        elif path == "/cgi-bin/entry.cgi/system/device-info":
            self._json({"sSerialNumber": "SN123", "sFirmwareVersion": "0.9.9",
                        "sSensorModel": "IMX335", "sBasePlateModel": "B1"})
        elif path == "/cgi-bin/entry.cgi/system/resource-info":
            self._json({"iCpuUsage": 12, "iNpuUsage": 3,
                        "sMem": {"iMemTotal": "0.5", "iMemUsed": "0.2", "iMemUsage": "40"},
                        "sStorage": {"iStorageTotal": "7", "iStorageUsed": "1", "iStorageUsage": "15"}})
        elif path == "/cgi-bin/entry.cgi/system/time":
            self._json({"sMethod": "ntp", "iTimestamp": 1745152496, "sTimezone": "UTC",
                        "sTz": "UTC0", "dNtpConfig": {"sAddress": "pool.ntp.org", "sPort": "123"}})
        elif path == "/cgi-bin/entry.cgi/record/storage/status":
            mount = str(st.mnt / "rc_mmcblk0p8")
            slot = {"sDevPath": "/dev/mmcblk0p8", "sMountPath": mount,
                    "bInternal": True, "bSelected": True, "bEnabled": st.storage_enabled,
                    "sState": "IDLE", "iStatsSizeBytes": 8000, "iStatsFreeBytes": 4000,
                    "bQuotaRotate": True, "iQuotaLimitBytes": -1}
            self._json({"sDataDirName": "reCamera", "lSlots": [slot]})
        elif path == "/cgi-bin/entry.cgi/record/rule/config":
            self._json(st.rule_config)
        elif path == "/cgi-bin/entry.cgi/record/rule/schedule-rule-config":
            self._json(st.schedule)
        elif path == "/cgi-bin/entry.cgi/record/rule/record-rule-config":
            self._json(st.record_rule)
        elif path == "/cgi-bin/entry.cgi/record/capture/status":
            last = st.capture["last"]
            self._json({"bReadyToStartNew": st.capture["ready"], "bStopRequested": False,
                        "dLastCapture": last})
        elif path == "/cgi-bin/entry.cgi/model/list":
            self._json({"lModels": [
                {"model": "yolo11n", "modelInfo": {"algorithm": "detect", "framework": "tpu",
                                                   "version": "1.0", "classes": ["person", "car"]}},
            ]})
        elif path == "/cgi-bin/entry.cgi/model/inference":
            self._json(getattr(st, "inference", {"iEnable": 0}))
        elif path == "/extension/acousticslab/api/v1/active":
            self._json(st.active_head)
        elif path == "/extension/acousticslab/api/v1/workspaces":
            self._json(st.workspaces)
        elif path.startswith("/extension/acousticslab/api/v1/workspaces/") and path.endswith("/heads"):
            ws_id = path.split("/")[-2]
            self._json(st.heads.get(ws_id, {"heads": []}))
        elif path == "/api/app-center/v1/recording/sources":
            self._json(st.sources)
        elif path == "/api/app-center/v1/apps":
            self._json({"apps": st.apps})
        elif path.startswith("/api/app-center/v1/apps/") and path.endswith("/logs"):
            app_id = path.split("/")[5]
            self._json({"id": app_id, "lines": ["boot ok", "inference started"],
                        "text": "boot ok\ninference started\n"})
        elif path == "/cgi-bin/entry.cgi/system/battery":
            self._json(st.battery)
        elif path == "/cgi-bin/entry.cgi/notify/cfg":
            self._json(st.notify)
        elif path == "/cgi-bin/entry.cgi/config/export":
            self._json({"url": "/cgi-bin/entry.cgi/config/download"})
        elif path == "/cgi-bin/entry.cgi/config/download":
            body = b"TARBALL-CONTENT"
            self.send_response(200)
            self.send_header("Content-Type", "application/octet-stream")
            self.send_header("Content-Length", str(len(body)))
            self.end_headers()
            self.wfile.write(body)
        elif path.startswith("/cgi-bin/entry.cgi/video/") and path.endswith("/encode"):
            stream_id = int(path.split("/")[4])
            self._json(st.video[stream_id])
        elif path == "/api/v1/intellisense/events":
            start = int(q["start"][0]) if "start" in q else None
            end = int(q["end"][0]) if "end" in q else None
            evs = [e for e in st.events
                   if (start is None or e["timestamp"] >= start)
                   and (end is None or e["timestamp"] <= end)]
            self._json(evs)
        elif path == "/api/v1/gpios":
            self._json({str(k): {"info": {"name": f"GPIO{k}", "chip": "gpio0", "line": k,
                                          "capabilities": ["FLOATING"]},
                                 "settings": v["settings"]} for k, v in st.gpio.items()})
        elif path.startswith("/api/v1/gpio/") and path.endswith("/value"):
            pin = int(path.split("/")[4])
            self._text(str(st.gpio[pin]["value"]))
        elif path.startswith("/api/v1/gpio/") and path.endswith("/settings"):
            pin = int(path.split("/")[4])
            self._json(st.gpio[pin]["settings"])
        elif path.startswith("/api/v1/gpio/"):
            pin = int(path.split("/")[4])
            g = st.gpio[pin]
            self._json({"info": {"name": f"GPIO{pin}", "chip": "gpio0", "line": pin,
                                 "capabilities": ["FLOATING"]}, "settings": g["settings"]})
        elif path == "/cgi-bin/entry.cgi/image/0":
            self._json({
                "videoAdjustment": {"iImageRotation": 0, "sImageFlip": "close",
                                    "sPowerLineFrequencyMode": "PAL(50HZ)"},
                "nightToDay": {"iMode": 0, "iDawnTime": 21600, "iDuskTime": 64800,
                               "iNightToDayFilterLevel": 1, "iNightToDayFilterTime": 5,
                               "iProfileSelect": 0},
                "profile": [
                    {"BLC": {"sBLCRegion": "close", "sHDR": "close", "sHLC": "close",
                             "iBLCStrength": 50, "iDarkBoostLevel": 0, "iHDRLevel": 1,
                             "iHLCLevel": 50}},
                    {"BLC": {"sBLCRegion": "close", "sHDR": "close", "sHLC": "close",
                             "iBLCStrength": 50, "iDarkBoostLevel": 0, "iHDRLevel": 1,
                             "iHLCLevel": 50}},
                    {"BLC": {"sBLCRegion": "close", "sHDR": "close", "sHLC": "close",
                             "iBLCStrength": 50, "iDarkBoostLevel": 0, "iHDRLevel": 1,
                             "iHLCLevel": 50}},
                ],
            })
        else:
            self._json({"error": "not found"}, status=404)

    def do_POST(self):
        if not self._check_auth():
            return
        u = urllib.parse.urlsplit(self.path)
        path = u.path
        st = self.st
        body = self._body()

        if path == "/cgi-bin/entry.cgi/system/reboot":
            st.rebooted = True
            self._json({"code": 0})
        elif path == "/cgi-bin/entry.cgi/record/rule/config":
            st.rule_config = body
            self._json({"code": 0})
        elif path == "/cgi-bin/entry.cgi/record/rule/schedule-rule-config":
            st.schedule = body
            self._json({"code": 0})
        elif path == "/cgi-bin/entry.cgi/record/rule/record-rule-config":
            st.record_rule = body
            self._json({"code": 0})
        elif path == "/cgi-bin/entry.cgi/record/rule/http-rule-activate":
            st.http_activations += 1
            self._json({"code": 0})
        elif path == "/cgi-bin/entry.cgi/record/storage/config":
            st.storage_enabled = body.get("dSelectSlotToEnable") is not None
            self._json({"code": 0})
        elif path == "/cgi-bin/entry.cgi/record/storage/control":
            self._json({"code": 0})
        elif path == "/cgi-bin/entry.cgi/record/capture/start":
            out_dir = body.get("sOutput") or ""
            if not out_dir.startswith(str(st.mnt)):
                self._json({"code": 30022, "message": "output outside storage mount"})
                return
            name = f"IMG_{len(os.listdir(out_dir)) if os.path.isdir(out_dir) else 0:04d}.jpg"
            os.makedirs(out_dir, exist_ok=True)
            (Path(out_dir) / name).write_bytes(b"\xff\xd8fake-jpeg\xff\xd9")
            event = {"sID": "cap-1", "sOutputDirectory": out_dir,
                     "sFormat": body.get("sFormat"), "iVideoLengthSeconds": body.get("iVideoLengthSeconds"),
                     "sStatus": "COMPLETED", "iTimestamp": int(time.time() * 1000),
                     "sFileName": name}
            st.capture["last"] = event
            st.capture["ready"] = True
            self._json({"code": 0, "dCapture": dict(event, sStatus="RUNNING")})
        elif path == "/cgi-bin/entry.cgi/record/capture/stop":
            self._json({"code": 0})
        elif path == "/cgi-bin/entry.cgi/model/inference":
            st.inference = {"iEnable": 1, "iFPS": body.get("iFPS", 30),
                            "sModel": body.get("sModel"), "sStatus": "running"}
            self._json({"code": 0})
        elif path == "/api/v1/intellisense/events/clear":
            st.events.clear()
            self._json({"status": "ok"})
        elif path == "/extension/acousticslab/api/v1/active":
            if body and body.get("default"):
                st.active_head = {"runtime_head_id": "factory", "labels": ["Cat"]}
            elif body:
                st.active_head = {"runtime_head_id": body["head_id"],
                                  "labels": ["Cat", "Dog"]}
            self._json({"ok": True})
        elif path == "/cgi-bin/entry.cgi/notify/cfg":
            st.notify.update(body)
            self._json({"code": 0})
        elif path.startswith("/cgi-bin/entry.cgi/video/") and path.endswith("/encode"):
            stream_id = int(path.split("/")[4])
            st.video[stream_id].update(body)
            self._json({"code": 0})
        elif path.startswith("/api/app-center/v1/apps/"):
            parts = path.split("/")
            app_id, action = parts[5], parts[6]
            st.app_actions.append((app_id, action))
            for app in st.apps:
                if app["id"] == app_id:
                    app["status"] = "running" if action in ("start", "restart") else "stopped"
            self._json({"ok": True}, status=202)
        elif path.startswith("/api/v1/gpio/") and path.endswith("/settings"):
            pin = int(path.split("/")[4])
            st.gpio[pin]["settings"].update(body)
            self._json({"code": 0})
        elif path.startswith("/api/v1/gpio/") and path.endswith("/value"):
            pin = int(path.split("/")[4])
            st.gpio[pin]["value"] = int(str(body).strip())
            self._json({"code": 0})
        else:
            self._json({"error": "not found"}, status=404)

    def do_PUT(self):
        if not self._check_auth():
            return
        u = urllib.parse.urlsplit(self.path)
        if u.path.startswith("/cgi-bin/entry.cgi/image/0/"):
            self.st.image_puts.append((u.path, self._body()))
            self._json({"code": 0})
        else:
            self._json({"error": "not found"}, status=404)


class LocalSkillTest(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls._tmp = tempfile.TemporaryDirectory()
        mnt = Path(cls._tmp.name) / "mnt"
        mnt.mkdir()
        cls.mnt = mnt
        cls.server = ThreadingHTTPServer(("127.0.0.1", 0), FakeDevice)
        cls.server.state = _State(mnt)  # type: ignore[attr-defined]
        cls.port = cls.server.server_address[1]
        cls._thread = threading.Thread(target=cls.server.serve_forever, daemon=True)
        cls._thread.start()

    @classmethod
    def tearDownClass(cls):
        cls.server.shutdown()
        cls.server.server_close()
        cls._tmp.cleanup()

    def setUp(self):
        # Fresh device state per test (event store, rule config, captures...).
        import shutil
        mnt = self.mnt
        shutil.rmtree(mnt / "rc_mmcblk0p8", ignore_errors=True)
        self.server.state = _State(mnt)  # type: ignore[attr-defined]
        os.environ["RECAMERA_HOST"] = "127.0.0.1"
        os.environ["RECAMERA_PORT"] = str(self.port)
        os.environ["RECAMERA_PROTOCOL"] = "http"
        os.environ["RECAMERA_TOKEN"] = TOKEN
        os.environ["RECAMERA_LOCAL_READ_ROOTS"] = str(self.mnt)
        _local.reset()

    def tearDown(self):
        for k in ("RECAMERA_HOST", "RECAMERA_PORT", "RECAMERA_PROTOCOL",
                  "RECAMERA_TOKEN", "RECAMERA_LOCAL_READ_ROOTS"):
            os.environ.pop(k, None)
        _local.reset()

    @property
    def st(self) -> _State:
        return self.server.state  # type: ignore[attr-defined]


class TestResolution(LocalSkillTest):
    def test_env_override_resolves(self):
        ep = _local.resolve()
        self.assertEqual((ep.protocol, ep.host, ep.port), ("http", "127.0.0.1", self.port))

    def test_autodetect_uses_candidates(self):
        os.environ.pop("RECAMERA_PORT")
        os.environ.pop("RECAMERA_PROTOCOL")
        _local.reset()
        candidates = (("http", self.port), ("https", 1))
        with unittest.mock.patch.object(_local, "_CANDIDATES", candidates):
            ep = _local.resolve()
        self.assertEqual(ep.port, self.port)

    def test_autodetect_follows_same_host_redirect(self):
        # Device nginx 307s :80 -> :443; the probe must resolve the target.
        main_port = self.port

        class Redirector(BaseHTTPRequestHandler):
            def log_message(self, *args):
                pass

            def do_GET(self):
                self.send_response(307)
                self.send_header("Location", f"http://127.0.0.1:{main_port}/api/v1/recamera-generate-204")
                self.send_header("Content-Length", "0")
                self.end_headers()

        redir = ThreadingHTTPServer(("127.0.0.1", 0), Redirector)
        threading.Thread(target=redir.serve_forever, daemon=True).start()
        try:
            os.environ.pop("RECAMERA_PORT")
            os.environ.pop("RECAMERA_PROTOCOL")
            _local.reset()
            with unittest.mock.patch.object(
                _local, "_CANDIDATES", (("http", redir.server_address[1]),)
            ):
                ep = _local.resolve()
            self.assertEqual(ep.port, main_port)
        finally:
            redir.shutdown()
            redir.server_close()

    def test_unreachable_raises_actionable_error(self):
        os.environ["RECAMERA_PORT"] = "1"  # nothing listens there
        _local.reset()
        with self.assertRaises(RecameraError) as ctx:
            _local.resolve()
        self.assertIn("not reachable", str(ctx.exception))

    def test_wrong_token_surfaces_401_hint(self):
        os.environ["RECAMERA_TOKEN"] = "wrong"
        _local.reset()
        with self.assertRaises(RecameraError) as ctx:
            rl.get_device_info()
        self.assertEqual(ctx.exception.status, 401)
        self.assertIn("token", str(ctx.exception).lower())


class TestStatus(LocalSkillTest):
    def test_get_status_composite(self):
        s = rl.get_status()
        self.assertEqual(s["device"]["serial_number"], "SN123")
        self.assertEqual(s["resources"]["cpu_usage"], 12)
        self.assertEqual(s["storage"][0]["dev_path"], "/dev/mmcblk0p8")
        self.assertIn("trigger", s["record"])
        self.assertEqual(s["record"]["trigger"]["kind"], "always_on")
        self.assertIn("capture", s)

    def test_reboot_requires_confirm(self):
        with self.assertRaises(RecameraError):
            rl.reboot_device()
        self.assertFalse(self.st.rebooted)
        rl.reboot_device(confirm=True)
        self.assertTrue(self.st.rebooted)


class TestCapture(LocalSkillTest):
    def test_capture_image_returns_local_path_without_base64(self):
        result = rl.capture_image(timeout=5.0)
        self.assertEqual(result["event"]["status"], "COMPLETED")
        self.assertTrue(result["path"].startswith(str(self.mnt)))
        self.assertTrue(os.path.isfile(result["path"]))
        self.assertNotIn("content_base64", result)

    def test_capture_image_inline(self):
        result = rl.capture_image(timeout=5.0, inline=True)
        self.assertEqual(
            base64.b64decode(result["content_base64"]), b"\xff\xd8fake-jpeg\xff\xd9"
        )

    def test_capture_output_outside_mount_is_explained(self):
        with self.assertRaises(RecameraError) as ctx:
            rl.capture_image(output="/tmp/elsewhere", timeout=2.0)
        self.assertEqual(ctx.exception.code, 30022)
        self.assertIn("Hint", str(ctx.exception))


class TestDetect(LocalSkillTest):
    def test_models_and_activation(self):
        models = rl.get_detection_models_info()
        self.assertEqual(models[0]["labels"], ["person", "car"])
        active = rl.set_detection_model(model_name="yolo11n", fps=15)
        self.assertEqual(active["name"], "yolo11n")
        self.assertEqual(active["fps"], 15)

    def test_unknown_model_lists_installed(self):
        with self.assertRaises(ValueError) as ctx:
            rl.set_detection_model(model_name="nope")
        self.assertIn("yolo11n", str(ctx.exception))

    def test_set_detection_rules_installs_inference_set_and_writer(self):
        rl.set_detection_rules(rules=[{"name": "person", "label_filter": ["person"]}])
        self.assertEqual(self.st.record_rule["sCurrentSelected"], "INFERENCE_SET")
        rule = self.st.record_rule["lInferenceSet"][0]
        self.assertEqual(rule["sID"], "person")
        self.assertEqual(rule["lClassFilter"], ["person"])
        # full-frame default region
        self.assertEqual(rule["lRegionFilter"][0]["lPolygon"][2], [1.0, 1.0])
        self.assertTrue(self.st.rule_config["bRuleEnabled"])
        self.assertEqual(self.st.rule_config["dWriterConfig"]["sFormat"], "JPG")

    def test_get_detection_rules_empty_when_other_trigger(self):
        self.assertEqual(rl.get_detection_rules(), [])
        rl.set_detection_rules(rules=[{"name": "car", "label_filter": ["car"]}])
        rules = rl.get_detection_rules()
        self.assertEqual([r["name"] for r in rules], ["car"])

    def test_events_normalized_and_cleared(self):
        self.st.events.append({
            "event": "RULE", "type": "INFERENCE_SET", "id": "person", "uid": 1,
            "timestamp": 1745152496000,
            "file_event": {"op": "ADDED", "path": "/mnt/rc_mmcblk0p8/reCamera/a.jpg",
                           "size": 10, "event_uid": 1, "timestamp": 1745152496001},
        })
        events = rl.get_detection_events(start_unix_ms=1745152496000)
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0]["rule_name"], "person")
        self.assertEqual(events[0]["snapshot_path"], "/mnt/rc_mmcblk0p8/reCamera/a.jpg")
        rl.clear_detection_events()
        self.assertEqual(rl.get_detection_events(), [])

    def test_wait_event_blocks_until_fresh_event(self):
        mark = 1745152496000
        self.st.events.append({"event": "RULE", "type": "INFERENCE_SET", "id": "old",
                               "uid": 1, "timestamp": mark})

        def push():
            time.sleep(0.4)
            self.st.events.append({"event": "RULE", "type": "INFERENCE_SET", "id": "new",
                                   "uid": 2, "timestamp": mark + 5000})

        threading.Thread(target=push, daemon=True).start()
        result = rl.wait_event(timeout_s=5, since_unix_ms=mark, poll_interval_s=0.1)
        self.assertFalse(result["timed_out"])
        self.assertEqual([e["rule_name"] for e in result["events"]], ["new"])
        self.assertEqual(result["watermark_unix_ms"], mark + 5000)

    def test_wait_event_timeout_keeps_watermark(self):
        result = rl.wait_event(timeout_s=0.5, since_unix_ms=123, poll_interval_s=0.1)
        self.assertTrue(result["timed_out"])
        self.assertEqual(result["events"], [])
        self.assertEqual(result["watermark_unix_ms"], 123)

    def test_acoustic_model(self):
        model = rl.get_active_acoustic_model()
        self.assertEqual(model["labels"], ["Cat", "Dog"])


class TestTrigger(LocalSkillTest):
    def test_trigger_switch_preserves_siblings(self):
        rl.set_record_trigger(trigger={"kind": "gpio", "num": 106, "signal": "FALLING"})
        self.assertEqual(self.st.record_rule["sCurrentSelected"], "GPIO")
        self.assertEqual(self.st.record_rule["dGPIO"]["iNum"], 106)
        # the remembered timer config survived the switch
        self.assertEqual(self.st.record_rule["dTimer"], {"iIntervalSeconds": 60})

    def test_bad_trigger_kind_rejected(self):
        with self.assertRaises(ValueError):
            rl.set_record_trigger(trigger={"kind": "nope"})

    def test_schedule_roundtrip(self):
        rl.set_detection_schedule([{"start": "Mon 08:00:00", "end": "Mon 18:00:00"}])
        self.assertEqual(rl.get_detection_schedule(),
                         [{"start": "Mon 08:00:00", "end": "Mon 18:00:00"}])
        rl.set_detection_schedule(None)
        self.assertIsNone(rl.get_detection_schedule())

    def test_http_activate(self):
        rl.activate_http_trigger()
        self.assertEqual(self.st.http_activations, 1)


class TestRecords(LocalSkillTest):
    def _seed(self):
        day = self.st.record_root / "2026-04-20"
        day.mkdir(parents=True, exist_ok=True)
        (day / "clip-001.mp4").write_bytes(b"0" * 100)
        (day / "snap-001.jpg").write_bytes(b"\xff\xd8img\xff\xd9")

    def test_list_records_paginates(self):
        self._seed()
        top = rl.list_records()
        self.assertEqual(top["entries"][0]["name"], "2026-04-20")
        self.assertTrue(top["entries"][0]["is_dir"])
        day = rl.list_records(path="2026-04-20", limit=1)
        self.assertEqual(day["total"], 2)
        self.assertTrue(day["has_more"])
        page2 = rl.list_records(path="2026-04-20", limit=1, offset=1)
        self.assertFalse(page2["has_more"])
        self.assertNotEqual(day["entries"][0]["name"], page2["entries"][0]["name"])

    def test_list_records_rejects_traversal(self):
        with self.assertRaises(ValueError):
            rl.list_records(path="../..")

    def test_read_file_inline_and_traversal_guard(self):
        self._seed()
        target = str(self.st.record_root / "2026-04-20" / "snap-001.jpg")
        out = rl.read_file(path=target)
        self.assertEqual(base64.b64decode(out["content_base64"]), b"\xff\xd8img\xff\xd9")
        with self.assertRaises(ValueError):
            rl.read_file(path="/etc/passwd")  # outside the allowed roots
        with self.assertRaises(ValueError):
            rl.read_file(path=str(self.mnt / ".." / "etc" / "passwd"))

    def test_read_file_budget_note(self):
        big = self.st.record_root / "big.bin"
        big.write_bytes(b"0" * 1024)
        out = rl.read_file(path=str(big), max_inline_bytes=10)
        self.assertIn("note", out)
        self.assertNotIn("content_base64", out)

    def test_delete_file_requires_confirm(self):
        self._seed()
        target = str(self.st.record_root / "2026-04-20" / "clip-001.mp4")
        with self.assertRaises(RecameraError):
            rl.delete_file(path=target)
        self.assertTrue(os.path.isfile(target))
        rl.delete_file(path=target, confirm=True)
        self.assertFalse(os.path.exists(target))


class TestGpio(LocalSkillTest):
    def test_set_then_get_value(self):
        rl.set_gpio_value(pin_id=106, value=1)
        self.assertEqual(self.st.gpio[106]["settings"]["state"], "push-pull")
        self.assertEqual(rl.get_gpio_value(pin_id=106), 1)
        self.assertEqual(self.st.gpio[106]["settings"]["state"], "floating")

    def test_bad_value_rejected(self):
        with self.assertRaises(ValueError):
            rl.set_gpio_value(pin_id=106, value=2)


class TestImage(LocalSkillTest):
    def test_get_image_settings_shape(self):
        cfg = rl.get_image_settings()
        self.assertEqual(cfg["video_adjustment"]["rotation"], 0)
        self.assertEqual(len(cfg["profiles"]), 3)
        self.assertIn("backlight", cfg["profiles"][0])

    def test_set_image_settings_merges_and_puts(self):
        rl.set_image_settings(section="video_adjustment", values={"rotation": 180})
        path, payload = self.st.image_puts[-1]
        self.assertTrue(path.endswith("/video-adjustment"))
        self.assertEqual(payload["iImageRotation"], 180)
        self.assertEqual(payload["sImageFlip"], "close")  # merged, not replaced

    def test_blc_hdr_hlc_mutex_enforced(self):
        with self.assertRaises(ValueError):
            rl.set_image_settings(
                section="backlight", scene_id=0,
                values={"blc_region": "open", "hdr": "open"},
            )

    def test_profile_section_requires_scene(self):
        with self.assertRaises(ValueError):
            rl.set_image_settings(section="adjustment", values={"brightness": 60})


class TestSourcesAndSedRetirement(LocalSkillTest):
    def test_get_record_sources_normalized(self):
        sources = rl.get_record_sources()
        by_id = {s["id"]: s for s in sources}
        self.assertEqual(by_id["builtin"]["kind"], "builtin")
        self.assertTrue(by_id["builtin"]["supports_roi"])
        self.assertEqual(by_id["acousticslab"]["classes"], ["Cat", "Dog"])

    def test_rules_default_to_builtin_source(self):
        rl.set_detection_rules(rules=[{"name": "person", "label_filter": ["person"]}])
        rule = self.st.record_rule["lInferenceSet"][0]
        self.assertEqual(rule["lSourceFilter"], ["builtin"])

    def test_unknown_source_id_rejected(self):
        with self.assertRaises(ValueError) as ctx:
            rl.set_detection_rules(rules=[{"name": "x", "source_filter": ["zzz"]}])
        self.assertIn("unknown source_filter", str(ctx.exception))

    def test_unproducible_label_rejected(self):
        with self.assertRaises(ValueError) as ctx:
            rl.set_detection_rules(rules=[{
                "name": "alarm", "source_filter": ["acousticslab"],
                "label_filter": ["Nope"],
            }])
        self.assertIn("cannot be produced", str(ctx.exception))

    def test_sound_rule_via_source_filter(self):
        rl.set_detection_rules(rules=[{
            "name": "alarm", "source_filter": ["acousticslab"],
            "label_filter": ["Cat"], "debounce_times": 3,
        }])
        rule = self.st.record_rule["lInferenceSet"][0]
        self.assertEqual(rule["lSourceFilter"], ["acousticslab"])
        self.assertEqual(rule["lClassFilter"], ["Cat"])

    def test_sed_set_raises_retired_hint(self):
        with self.assertRaises(ValueError) as ctx:
            rl.set_record_trigger(trigger={"kind": "sed", "label_filter": ["Cat"]})
        self.assertIn("retired", str(ctx.exception))

    def test_sed_parse_raises_but_get_detection_rules_gates(self):
        self.st.record_rule = {"sCurrentSelected": "SED", "dSED": {"sID": "x"}}
        # Raw parse refuses with a migration hint...
        with self.assertRaises(ValueError) as ctx:
            rl.get_record_trigger()
        self.assertIn("retired", str(ctx.exception))
        # ...while the detection facade treats it as "not inference_set".
        self.assertEqual(rl.get_detection_rules(), [])

    def test_get_status_survives_retired_sed_trigger(self):
        self.st.record_rule = {"sCurrentSelected": "SED", "dSED": {"sID": "x"}}
        s = rl.get_status()
        self.assertIsNone(s["record"]["trigger"])


class TestApps(LocalSkillTest):
    def test_list_apps(self):
        apps = rl.list_apps()
        by_id = {a["id"]: a for a in apps}
        self.assertTrue(by_id["acousticslab"]["system"])
        self.assertEqual(by_id["counter"]["status"], "stopped")

    def test_get_app_logs(self):
        logs = rl.get_app_logs(app_id="acousticslab", tail=50)
        self.assertIn("inference started", logs["text"])

    def test_start_app_queues(self):
        result = rl.start_app("counter")
        self.assertTrue(result["changed"])
        self.assertEqual(self.st.app_actions, [("counter", "start")])

    def test_stop_system_app_needs_confirm(self):
        with self.assertRaises(ValueError):
            rl.stop_app("acousticslab")
        self.assertEqual(self.st.app_actions, [])
        rl.stop_app("acousticslab", confirm=True)
        self.assertEqual(self.st.app_actions, [("acousticslab", "stop")])

    def test_unknown_app_rejected(self):
        with self.assertRaises(ValueError) as ctx:
            rl.start_app("nope")
        self.assertIn("unknown app", str(ctx.exception))

    def test_app_id_guard(self):
        with self.assertRaises(ValueError):
            rl.get_app_logs(app_id="../etc")


class TestAcousticHeads(LocalSkillTest):
    def test_list_marks_active(self):
        models = rl.list_acoustic_models()
        self.assertEqual(models[0]["head_id"], "head_abc")
        self.assertTrue(models[0]["active"])

    def test_set_validates_ids(self):
        with self.assertRaises(ValueError):
            rl.set_acoustic_model(workspace_id="nope", head_id="head_abc")
        with self.assertRaises(ValueError):
            rl.set_acoustic_model(workspace_id="ws1", head_id="nope")
        result = rl.set_acoustic_model(workspace_id="ws1", head_id="head_abc")
        self.assertTrue(result["changed"])

    def test_default_restores_factory(self):
        result = rl.set_acoustic_model(default=True)
        self.assertTrue(result["default"])
        self.assertEqual(self.st.active_head["runtime_head_id"], "factory")


class TestNotify(LocalSkillTest):
    def test_get_redacts_secrets(self):
        cfg = rl.get_notify_config()
        self.assertEqual(cfg["mqtt"]["password"], "***")
        self.assertEqual(cfg["mode_name"], "off")

    def test_set_merges_and_preserves_stored_secret(self):
        rl.set_notify_config(mqtt={"topic": "recamera/events"})
        posted = self.st.notify["dMqtt"]
        self.assertEqual(posted["sTopic"], "recamera/events")
        self.assertEqual(posted["sPassword"], "stored-secret")  # merged, not clobbered

    def test_set_rejects_redaction_placeholder(self):
        with self.assertRaises(ValueError) as ctx:
            rl.set_notify_config(mqtt={"password": "***"})
        self.assertIn("redaction placeholder", str(ctx.exception))

    def test_mode_requires_channel_url(self):
        with self.assertRaises(ValueError):
            rl.set_notify_config(mode=1)  # mqtt.url empty


class TestVideo(LocalSkillTest):
    def test_get_video_encode(self):
        enc = rl.get_video_encode(stream="sub")
        self.assertEqual(enc["codec"], "H.265")
        self.assertEqual(enc["resolution"], "640*480")

    def test_set_video_encode_verifies(self):
        result = rl.set_video_encode(stream="main", resolution="2560x1440", frame_rate=25)
        self.assertEqual(result["applied"]["resolution"], "2560*1440")
        self.assertEqual(self.st.video[0]["sFrameRate"], "25")
        self.assertEqual(self.st.video[0]["sOutputDataType"], "H.264")  # untouched

    def test_set_video_encode_rejects_bad_values(self):
        with self.assertRaises(ValueError):
            rl.set_video_encode(codec="VP9")
        with self.assertRaises(ValueError):
            rl.set_video_encode(resolution="100x100")
        with self.assertRaises(ValueError):
            rl.set_video_encode()  # nothing to change


class TestBackupAndBattery(LocalSkillTest):
    def test_export_device_config(self):
        out = str(Path(self.mnt) / "backup.tar")
        result = rl.export_device_config(output=out)
        self.assertEqual(result["size_bytes"], len(b"TARBALL-CONTENT"))
        self.assertEqual(Path(out).read_bytes(), b"TARBALL-CONTENT")
        with self.assertRaises(ValueError):  # no overwrite
            rl.export_device_config(output=out)

    def test_battery_status(self):
        bat = rl.get_battery_status()
        self.assertTrue(bat["attached"])
        self.assertEqual(bat["display_steps"], 4)
        self.assertEqual(rl.get_status()["battery"]["total_steps"], 5)


class TestConcurrency(LocalSkillTest):
    def test_parallel_threads_each_get_own_connection(self):
        # Regression: pooled conn must be per-thread (observed on-device as
        # CannotSendRequest when two threads shared one socket).
        errors = []

        def hammer(fn):
            try:
                for _ in range(15):
                    fn()
            except Exception as exc:
                errors.append(exc)

        threads = [
            threading.Thread(target=hammer, args=(rl.get_system_time,)),
            threading.Thread(target=hammer, args=(rl.get_capture_status,)),
            threading.Thread(target=hammer, args=(rl.get_storage_status,)),
            threading.Thread(target=hammer, args=(rl.get_record_config,)),
        ]
        for t in threads:
            t.start()
        for t in threads:
            t.join()
        self.assertEqual(errors, [])


class TestTimeoutRecovery(LocalSkillTest):
    def test_timeout_drops_pooled_connection_and_recovers(self):
        # Regression: a timeout must not poison the pool; next call succeeds.
        with self.assertRaises(RecameraError) as ctx:
            _local.get_json("/slow", timeout=0.2)
        self.assertIn("timed out", str(ctx.exception))
        self.assertEqual(rl.get_device_info()["serial_number"], "SN123")


class TestCli(LocalSkillTest):
    def _env(self):
        env = dict(os.environ)
        env["PYTHONPATH"] = str(SKILL_ROOT / "scripts")
        return env

    def _run(self, *args, input_text=None):
        return subprocess.run(
            [sys.executable, "-m", "recamera_local", *args],
            capture_output=True, text=True, env=self._env(), input=input_text,
        )

    def test_compact_json_by_default(self):
        proc = self._run("get_device_info")
        self.assertEqual(proc.returncode, 0, proc.stderr)
        self.assertNotIn("\n  ", proc.stdout)  # no indentation
        self.assertEqual(json.loads(proc.stdout)["serial_number"], "SN123")

    def test_usage_error_prints_example(self):
        proc = self._run("set_gpio_value", "pin_id=106")  # missing value
        self.assertEqual(proc.returncode, 2)
        self.assertIn("usage:", proc.stderr)
        self.assertIn("example:", proc.stderr)

    def test_serve_mode_roundtrip(self):
        lines = (
            '{"id": 1, "cmd": "get_system_time"}\n'
            '{"id": 2, "cmd": "list-commands"}\n'
            '{"id": 3, "cmd": "set_gpio_value", "args": {"pin_id": 106, "value": 1}}\n'
            '{"cmd": "exit"}\n'
        )
        proc = self._run("serve", input_text=lines)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        out = [json.loads(line) for line in proc.stdout.strip().splitlines()]
        self.assertEqual(out[0]["id"], 1)
        self.assertTrue(out[0]["ok"])
        self.assertEqual(out[0]["result"]["timezone"], "UTC")
        self.assertIn("wait_event", out[1]["result"])
        self.assertTrue(out[2]["ok"])

    def test_serve_mode_error_keeps_session_alive(self):
        lines = (
            '{"cmd": "set_gpio_value", "args": {"pin_id": 106}}\n'
            '{"cmd": "get_system_time"}\n'
        )
        proc = self._run("serve", input_text=lines)
        self.assertEqual(proc.returncode, 0, proc.stderr)
        out = [json.loads(line) for line in proc.stdout.strip().splitlines()]
        self.assertFalse(out[0]["ok"])
        self.assertIn("missing required argument", out[0]["error"])
        self.assertTrue(out[1]["ok"])


if __name__ == "__main__":
    unittest.main(verbosity=2)
