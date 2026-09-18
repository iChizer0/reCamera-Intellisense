"""Tests for batch-B write surfaces: set_notify_config (merge keeps stored
secrets, redaction-echo rejected), app lifecycle (system-app confirm gate),
set_acoustic_model (loud id validation), set_video_encode (validation matrix +
verify-after-write)."""

import unittest
from unittest.mock import patch

from recamera_intellisense import _config, acoustic, apps, notify, video
from recamera_intellisense._errors import RecameraError
from recamera_intellisense._config import DeviceRecord


DEV = DeviceRecord(name="cam", host="192.0.2.10", protocol="https",
                   allow_unsecured=True, token="k")

STORED_CFG = {
    "iMode": 0,
    "dMqtt": {"sURL": "mqtt://old", "iPort": 1883, "sClientId": "cam",
              "sUsername": "u", "sPassword": "REAL-SECRET", "sTopic": "t"},
    "dHttp": {"sUrl": "", "sToken": "HTTP-SECRET"},
    "dTemplate": {"sDetection": "", "sClassification": "tmpl"},
    "dUart": {"sPort": 1, "sPortDev": "/dev/ttyS1"},
}

ENCODE_MAIN = {
    "iEnabled": 1, "iGOP": 60, "iMaxRate": 4096, "sFrameRate": "30",
    "sOutputDataType": "H.264", "sRCMode": "CBR", "sRCQuality": "highest",
    "sResolution": "3840*2160", "sStreamType": "mainStream",
}


class _Ctx:
    """Patch the shared _config.resolve for the duration of a test."""

    def __enter__(self):
        self._patch = patch.object(_config, "resolve", return_value=DEV)
        self._patch.start()
        return DEV

    def __exit__(self, *exc):
        self._patch.stop()
        return False


class SetNotifyConfigTests(unittest.TestCase):
    def _run(self, post_return=None, **kwargs):
        with _Ctx(), patch.object(
                notify._http, "get_json", return_value=dict(STORED_CFG)), \
                patch.object(notify._http, "post_json",
                             return_value=post_return or {"code": 0}) as post:
            return notify.set_notify_config(**kwargs), post

    def test_merge_keeps_stored_secret(self):
        result, post = self._run(mode=1, mqtt={"topic": "events"})
        sent = post.call_args.kwargs["payload"]
        self.assertEqual(sent["dMqtt"]["sPassword"], "REAL-SECRET")
        self.assertEqual(sent["dMqtt"]["sTopic"], "events")
        self.assertEqual(sent["iMode"], 1)
        self.assertNotIn("dHttp", sent)  # untouched section not resent
        self.assertEqual(result["mode_name"], "mqtt")
        self.assertIn("restart", result["note"])

    def test_explicit_secret_overrides(self):
        _, post = self._run(mqtt={"password": "new"})
        self.assertEqual(post.call_args.kwargs["payload"]["dMqtt"]["sPassword"],
                         "new")

    def test_empty_secret_clears(self):
        _, post = self._run(http={"token": ""})
        self.assertEqual(post.call_args.kwargs["payload"]["dHttp"]["sToken"], "")

    def test_redaction_echo_rejected(self):
        with self.assertRaises(ValueError) as ctx:
            self._run(mqtt={"password": "***"})
        self.assertIn("redaction placeholder", str(ctx.exception))

    def test_mode_requires_url(self):
        with self.assertRaises(ValueError):
            self._run(mode=2)  # stored dHttp.sUrl is ""
        self._run(mode=2, http={"url": "https://hook.local/x"})  # ok

    def test_validation(self):
        with self.assertRaises(ValueError):
            self._run()  # nothing to change
        with self.assertRaises(ValueError):
            self._run(mode=9)
        with self.assertRaises(ValueError):
            self._run(mqtt={"bogus": 1})
        with self.assertRaises(ValueError):
            self._run(mqtt={"port": 70000})
        with self.assertRaises(ValueError):
            self._run(templates={"unknown_task": "x"})

    def test_templates_merge_and_map(self):
        _, post = self._run(templates={"detection": "D"})
        sent = post.call_args.kwargs["payload"]["dTemplate"]
        self.assertEqual(sent["sDetection"], "D")
        self.assertEqual(sent["sClassification"], "tmpl")  # kept

    def test_device_error_raises(self):
        with self.assertRaises(RecameraError):
            self._run(post_return={"code": -1}, mode=0)


class ReviewRegressionTests(unittest.TestCase):
    def test_empty_sections_are_absent(self):
        with _Ctx(), patch.object(
                notify._http, "get_json", return_value=dict(STORED_CFG)),                 patch.object(notify._http, "post_json",
                             return_value={"code": 0}) as post:
            # all-empty -> nothing to change (no pointless device restart)
            with self.assertRaises(ValueError):
                notify.set_notify_config(mqtt={}, http={}, templates={})
            post.assert_not_called()
            # empty section alongside a real change is skipped, not resent
            notify.set_notify_config(mode=0, mqtt={})
            self.assertNotIn("dMqtt", post.call_args.kwargs["payload"])

    def test_int_field_rejects_non_integral_float(self):
        with _Ctx():
            with self.assertRaises(ValueError):
                video.set_video_encode(frame_rate=29.9)
            with self.assertRaises(ValueError):
                video.set_video_encode(gop=True)

    def test_codec_case_normalized(self):
        _, post = SetVideoEncodeTests()._run(codec="h.265",
                                             after={"sOutputDataType": "H.265"})
        self.assertEqual(post.call_args.kwargs["payload"]["sOutputDataType"],
                         "H.265")

    def test_acoustic_unreachable_has_actionable_hint(self):
        def _boom(dev, path, params=None, **kw):
            raise RecameraError("HTTP 502: Bad Gateway")
        with _Ctx(), patch.object(acoustic._http, "get_json", _boom):
            with self.assertRaises(RecameraError) as ctx:
                acoustic.set_acoustic_model(workspace_id="ws1", head_id="h1")
        self.assertIn("start_app", str(ctx.exception))


class LifecycleTests(unittest.TestCase):
    APPS = {"apps": [
        {"id": "acousticslab", "name": "AL", "system": True,
         "status": "running", "version": "1"},
        {"id": "my-app", "name": "M", "status": "stopped", "version": "1"},
    ]}

    def _run(self, func, *args, **kwargs):
        with _Ctx(), patch.object(
                apps._http, "get_json", return_value=dict(self.APPS)), \
                patch.object(apps._http, "post_json",
                             return_value={"code": 0}) as post:
            return func(*args, **kwargs), post

    def test_start_needs_no_confirm(self):
        result, post = self._run(apps.start_app, app_id="acousticslab")
        self.assertEqual(result["action"], "start")
        self.assertTrue(result["async"])
        self.assertTrue(post.call_args[0][1].endswith("/apps/acousticslab/start"))

    def test_stop_system_app_requires_confirm(self):
        with self.assertRaises(ValueError) as ctx:
            self._run(apps.stop_app, app_id="acousticslab")
        self.assertIn("confirm=true", str(ctx.exception))
        result, _ = self._run(apps.stop_app, app_id="acousticslab",
                              confirm=True)
        self.assertTrue(result["changed"])

    def test_user_app_stops_freely(self):
        result, _ = self._run(apps.stop_app, app_id="my-app")
        self.assertTrue(result["changed"])

    def test_unknown_app_lists_available(self):
        with self.assertRaises(ValueError) as ctx:
            self._run(apps.restart_app, app_id="ghost")
        self.assertIn("acousticslab", str(ctx.exception))


class SetAcousticModelTests(unittest.TestCase):
    def _http(self, dev, path, params=None, **kw):
        if path.endswith("/workspaces"):
            return {"workspaces": [{"id": "ws1"}]}
        if path.endswith("/ws1/heads"):
            return {"heads": [{"head_id": "h1"}]}
        return {"code": 0}

    def test_requires_ids_or_default(self):
        with _Ctx():
            with self.assertRaises(ValueError):
                acoustic.set_acoustic_model()
            with self.assertRaises(ValueError):
                acoustic.set_acoustic_model(default=True, head_id="h1")

    def test_unknown_ids_fail_loudly(self):
        with _Ctx(), patch.object(acoustic._http, "get_json", self._http):
            with self.assertRaises(ValueError) as ctx:
                acoustic.set_acoustic_model(workspace_id="ghost", head_id="h1")
            self.assertIn("ws1", str(ctx.exception))
            with self.assertRaises(ValueError) as ctx:
                acoustic.set_acoustic_model(workspace_id="ws1", head_id="ghost")
            self.assertIn("h1", str(ctx.exception))

    def test_posts_activation(self):
        with _Ctx(), patch.object(acoustic._http, "get_json", self._http), \
                patch.object(acoustic._http, "post_json",
                             return_value={"code": 0}) as post:
            result = acoustic.set_acoustic_model(workspace_id="ws1",
                                                 head_id="h1")
        self.assertTrue(result["changed"])
        self.assertEqual(post.call_args.kwargs["payload"],
                         {"workspace_id": "ws1", "head_id": "h1"})

    def test_default_payload(self):
        with _Ctx(), patch.object(acoustic._http, "post_json",
                                  return_value={"code": 0}) as post:
            acoustic.set_acoustic_model(default=True)
        self.assertEqual(post.call_args.kwargs["payload"], {"default": True})


class SetVideoEncodeTests(unittest.TestCase):
    def _run(self, after=None, post_return=None, **kwargs):
        reads = {"code": 0, **ENCODE_MAIN}
        if after:
            reads.update(after)

        def _get(dev, path, params=None, **kw):
            return dict(reads)

        with _Ctx(), patch.object(video._http, "get_json", _get), \
                patch.object(video._http, "post_json",
                             return_value=post_return or {"code": 0}) as post:
            return video.set_video_encode(**kwargs), post

    def test_nothing_to_change(self):
        with _Ctx():
            with self.assertRaises(ValueError):
                video.set_video_encode()

    def test_validation_matrix(self):
        bad = [
            {"codec": "VP9"}, {"resolution": "100*100"},
            {"resolution": "garbage"}, {"frame_rate": 0},
            {"frame_rate": "abc"}, {"gop": 121}, {"rc_mode": "auto"},
            {"rc_quality": "ultra"}, {"max_rate": 2}, {"stream": "third"},
        ]
        with _Ctx():
            for kwargs in bad:
                with self.assertRaises(ValueError, msg=str(kwargs)):
                    video.set_video_encode(**kwargs)

    def test_payload_types_and_verify(self):
        result, post = self._run(codec="H.265", frame_rate=25, gop=50,
                                 rc_mode="vbr", rc_quality="HIGH",
                                 max_rate=2048, enabled=True,
                                 after={"sOutputDataType": "H.265",
                                        "sFrameRate": "25", "iGOP": 50,
                                        "sRCMode": "VBR", "sRCQuality": "high",
                                        "iMaxRate": 2048, "iEnabled": 1})
        sent = post.call_args.kwargs["payload"]
        self.assertEqual(sent["sFrameRate"], "25")  # numeric STRING on the wire
        self.assertEqual(sent["iGOP"], 50)
        self.assertEqual(sent["iEnabled"], 1)
        self.assertEqual(result["applied"]["rc_mode"], "VBR")

    def test_resolution_normalization(self):
        _, post = self._run(resolution="1920x1080",
                            after={"sResolution": "1920*1080"})
        self.assertEqual(post.call_args.kwargs["payload"]["sResolution"],
                         "1920*1080")

    def test_verify_mismatch_raises(self):
        with self.assertRaises(RecameraError) as ctx:
            self._run(codec="H.265")  # device "after" still reports H.264
        self.assertIn("did not apply", str(ctx.exception))


if __name__ == "__main__":
    unittest.main()
