"""Regression tests for the connectivity probe's redirect semantics.

A firmware quirk found in the field: the device's :443 vhost 307-redirects to
plain http://127.0.0.1:80. Counting that as "reachable" made zero-config
detection persist an endpoint every real call then refused (cross-origin).
"""

from __future__ import annotations

import threading
import unittest
import unittest.mock
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

from recamera_intellisense import device


class _Handler(BaseHTTPRequestHandler):
    redirect_location = None  # set per test; None = answer 204 directly

    def do_GET(self) -> None:
        loc = type(self).redirect_location
        if loc is not None:
            self.send_response(307)
            self.send_header("Location", loc)
            self.end_headers()
        else:
            self.send_response(204)
            self.end_headers()

    def log_message(self, *args) -> None:  # keep test output clean
        pass


class ProbeRedirectTests(unittest.TestCase):
    def _server(self, location):
        _Handler.redirect_location = location
        srv = ThreadingHTTPServer(("127.0.0.1", 0), _Handler)
        t = threading.Thread(target=srv.serve_forever, daemon=True)
        t.start()
        self.addCleanup(srv.shutdown)
        self.addCleanup(srv.server_close)
        return srv.server_address[1]

    def test_direct_204_is_reachable(self) -> None:
        port = self._server(None)
        self.assertIsNone(device._probe("127.0.0.1", port, "", False, False, 2.0))

    def test_cross_origin_redirect_is_not_reachable(self) -> None:
        # Emulates the firmware's https:443 -> http://127.0.0.1:80 downgrade.
        port = self._server("http://127.0.0.1:80/api/v1/recamera-generate-204")
        err = device._probe("127.0.0.1", port, "", False, False, 2.0)
        self.assertIsNotNone(err)
        self.assertIn("redirects to another origin", err)

    def test_relative_redirect_is_followed(self) -> None:
        port = self._server(None)  # second hop answers 204
        _Handler.redirect_location = None
        # First hop redirects relatively; same server then serves 204.
        _Handler.redirect_location = "/api/v1/recamera-generate-204"
        self.assertIsNone(device._probe("127.0.0.1", port, "", False, False, 2.0))

    def test_same_origin_absolute_redirect_is_followed(self) -> None:
        port = self._server(None)
        _Handler.redirect_location = (
            f"http://127.0.0.1:{port}/api/v1/recamera-generate-204"
        )
        self.assertIsNone(device._probe("127.0.0.1", port, "", False, False, 2.0))

    def test_loopback_detection_prefers_plain_http(self) -> None:
        port = self._server(None)
        with unittest.mock.patch.object(device, "_DEFAULT_PROBE_TIMEOUT", 0.5):
            found = device.detect_local_device("127.0.0.1", port, token="")
        self.assertIsNotNone(found)
        self.assertEqual(found["protocol"], "http")


if __name__ == "__main__":  # pragma: no cover
    unittest.main()
