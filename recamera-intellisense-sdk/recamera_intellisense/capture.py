"""Capture status, start/stop, and the ``capture_image`` helper."""

from __future__ import annotations

import base64
import time
from typing import Any, Dict, Optional

from . import _config, _http
from ._errors import RecameraError

__all__ = ["get_capture_status", "start_capture", "stop_capture", "capture_image"]

PATH_STATUS = "/cgi-bin/entry.cgi/record/capture/status"
PATH_START = "/cgi-bin/entry.cgi/record/capture/start"
PATH_STOP = "/cgi-bin/entry.cgi/record/capture/stop"

FORMAT_IMAGE = "JPG"
OUTPUT_FALLBACK = "/mnt/rc_mmcblk0p8/reCamera"
_POLL_INTERVAL_S = 0.5
_DEFAULT_TIMEOUT_S = 5.0
_TERMINAL = {"COMPLETED", "FAILED", "INTERRUPTED", "CANCELED"}


def _parse_event(d: Dict[str, Any]) -> Dict[str, Any]:
    return {
        "id": d.get("sID", ""),
        "output_directory": d.get("sOutputDirectory", ""),
        "format": d.get("sFormat", ""),
        "video_length_seconds": d.get("iVideoLengthSeconds"),
        "status": d.get("sStatus", "UNKNOWN"),
        "timestamp_unix_ms": int(d.get("iTimestamp", 0) or 0),
        "file_name": d.get("sFileName", ""),
    }


def get_capture_status(device_name: str) -> Dict[str, Any]:
    """Current capture state (includes the last event, if any)."""
    dev = _config.resolve(device_name)
    d = _http.get_json(dev, PATH_STATUS) or {}
    last = d.get("dLastCapture")
    return {
        "last_capture": _parse_event(last) if isinstance(last, dict) else None,
        "ready_to_start_new": bool(d.get("bReadyToStartNew", False)),
        "stop_requested": bool(d.get("bStopRequested", False)),
    }


def start_capture(
    device_name: str,
    *,
    output: Optional[str] = None,
    format: str = FORMAT_IMAGE,
    video_length_seconds: Optional[int] = None,
) -> Dict[str, Any]:
    """Start a capture; returns the initial capture event."""
    dev = _config.resolve(device_name)
    payload: Dict[str, Any] = {
        "sOutput": output or OUTPUT_FALLBACK,
        "sFormat": str(format).upper(),
    }
    if video_length_seconds is not None:
        payload["iVideoLengthSeconds"] = int(video_length_seconds)
    resp = _http.post_json(dev, PATH_START, payload=payload)
    _http.expect_ok(resp, "start capture")
    capture = resp.get("dCapture")
    if not isinstance(capture, dict):
        raise RecameraError("start_capture response missing dCapture field.")
    return _parse_event(capture)


def stop_capture(device_name: str) -> None:
    """Stop the running capture (no-op for JPG)."""
    dev = _config.resolve(device_name)
    resp = _http.post_json(dev, PATH_STOP)
    _http.expect_ok(resp, "stop capture")


def capture_image(
    device_name: str,
    *,
    output: Optional[str] = None,
    timeout: float = _DEFAULT_TIMEOUT_S,
) -> Dict[str, Any]:
    """Start a JPG capture, poll to completion (terminal states ``COMPLETED/FAILED/INTERRUPTED/CANCELED``),
    fetch the file via the daemon, and return ``{event, path, size, content_base64}``.
    """
    # Resolve output dir via current storage status if not supplied.
    if output is None:
        try:
            from .storage import get_storage_status

            slots = get_storage_status(device_name)
            slot = next((s for s in slots if s["enabled"] and s["mount_path"]), None)
            if slot:
                base = slot["mount_path"].rstrip("/")
                data_dir = slot.get("data_dir", "").strip("/")
                output = f"{base}/{data_dir}" if data_dir else base
        except Exception:
            output = None
    output = output or OUTPUT_FALLBACK

    capture = start_capture(device_name, output=output, format=FORMAT_IMAGE)
    deadline = time.time() + float(timeout)
    final = dict(capture)
    while time.time() < deadline:
        time.sleep(_POLL_INTERVAL_S)
        status = get_capture_status(device_name)
        last = status["last_capture"]
        if last and last["id"] == capture["id"] and last["status"] in _TERMINAL:
            final = last
            break
    if final["status"] != "COMPLETED":
        raise RecameraError(f"Capture did not complete (status: {final['status']!r})")
    remote = f"{final['output_directory'].rstrip('/')}/{final['file_name']}"
    from .files import fetch_file

    blob = fetch_file(device_name, path=remote, raw=True)
    return {
        "event": final,
        "path": remote,
        "size": len(blob),
        "content_base64": base64.b64encode(blob).decode("ascii"),
    }


COMMANDS = {
    "get_capture_status": get_capture_status,
    "start_capture": start_capture,
    "stop_capture": stop_capture,
    "capture_image": capture_image,
}
COMMAND_SCHEMAS = {
    "get_capture_status": {"required": {"device_name"}, "optional": set()},
    "start_capture": {
        "required": {"device_name"},
        "optional": {"output", "format", "video_length_seconds"},
    },
    "stop_capture": {"required": {"device_name"}, "optional": set()},
    "capture_image": {"required": {"device_name"}, "optional": {"output", "timeout"}},
}
