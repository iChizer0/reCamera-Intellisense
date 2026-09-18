"""Result-push (notify) configuration API (``/notify/cfg``).

The pipeline is shared by the built-in vision inference and system sources
such as AcousticsLab. Secrets (passwords/tokens) are redacted on read: the
device returns them in cleartext, and they must not enter agent contexts.
"""

from __future__ import annotations

if __name__ == "__main__" and __package__ is None:
    import os
    import sys

    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    from recamera_intellisense._cli import main

    raise SystemExit(main())

from typing import Any, Dict, Optional

from . import _config, _http
from ._errors import RecameraError

__all__ = ["get_notify_config"]

PATH_CFG = "/cgi-bin/entry.cgi/notify/cfg"

_MODE_NAMES = {0: "off", 1: "mqtt", 2: "http", 3: "uart"}
_REDACTED = "***"


def _redact(value: Any) -> Any:
    """Non-empty secret strings become ``"***"``."""
    return _REDACTED if isinstance(value, str) and value else value


def get_notify_config(device_name: Optional[str] = None) -> Dict[str, Any]:
    """Result-push configuration (secrets redacted).

    Shape: ``{mode, mode_name, mqtt, http, uart, templates}``; ``mode`` is
    0=off / 1=MQTT / 2=HTTP / 3=UART. ``password``/``token`` read back as
    ``"***"`` when set. Empty ``templates`` values select the built-in
    per-task defaults.
    """
    dev = _config.resolve(device_name)
    d = _http.get_json(dev, PATH_CFG)
    if d is None:
        d = {}  # codebase GET norm: tolerate an empty body
    if not isinstance(d, dict):
        raise RecameraError("get notify config failed: unexpected payload")
    mqtt = d.get("dMqtt") or {}
    http = d.get("dHttp") or {}
    # Wire quirk: MQTT spells its host field "sURL", HTTP "sUrl".
    uart = d.get("dUart") or {}
    tpl = d.get("dTemplate") or {}
    mode = int(d.get("iMode", 0))
    return {
        "mode": mode,
        "mode_name": _MODE_NAMES.get(mode, f"unknown({mode})"),
        "mqtt": {
            "url": mqtt.get("sURL", ""),
            "port": int(mqtt.get("iPort", 1883)),
            "client_id": mqtt.get("sClientId", ""),
            "username": mqtt.get("sUsername", ""),
            "password": _redact(mqtt.get("sPassword", "")),
            "topic": mqtt.get("sTopic", ""),
        },
        "http": {
            "url": http.get("sUrl", ""),
            "token": _redact(http.get("sToken", "")),
        },
        "uart": {
            "port": uart.get("sPort", ""),
            "port_dev": uart.get("sPortDev", ""),
        },
        "templates": {
            "classification": tpl.get("sClassification", ""),
            "detection": tpl.get("sDetection", ""),
            "keypoint": tpl.get("sKeypoint", ""),
            "segmentation": tpl.get("sSegmentation", ""),
            "tracking": tpl.get("sTracking", ""),
        },
    }


COMMANDS = {"get_notify_config": get_notify_config}
