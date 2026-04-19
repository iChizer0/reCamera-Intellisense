"""Daemon file API + intellisense events (``/api/v1/file``, ``/api/v1/intellisense/events``)."""

from __future__ import annotations

import base64
from typing import Any, Dict, List, Optional, Union

from . import _config, _http

__all__ = [
    "fetch_file",
    "delete_file",
    "get_intellisense_events",
    "clear_intellisense_events",
]

PATH_FILE = "/api/v1/file"
PATH_EVENTS = "/api/v1/intellisense/events"
_MAX_INLINE_BYTES = 5 * 1024 * 1024
_IMAGE_EXT = {".jpg", ".jpeg", ".png", ".webp", ".bmp", ".gif"}


def fetch_file(
    device_name: str,
    *,
    path: str,
    max_inline_bytes: int = _MAX_INLINE_BYTES,
    raw: bool = False,
) -> Union[bytes, Dict[str, Any]]:
    """Fetch an on-device file.

    Returns inline base64 for images / payloads ≤ ``max_inline_bytes``.
    When ``raw=True``, returns the raw ``bytes`` (useful for pipelines).
    """
    dev = _config.resolve(device_name)
    body, ct = _http.get_bytes(dev, PATH_FILE, params={"path": path})
    if raw:
        return body
    is_image = any(path.lower().endswith(ext) for ext in _IMAGE_EXT)
    if is_image or len(body) <= max_inline_bytes:
        return {
            "path": path,
            "content_type": ct,
            "size": len(body),
            "content_base64": base64.b64encode(body).decode("ascii"),
        }
    return {
        "path": path,
        "content_type": ct,
        "size": len(body),
        "note": "payload exceeds inline budget; re-fetch with a higher max_inline_bytes "
        "or use the MCP streaming transport for large blobs.",
    }


def delete_file(device_name: str, *, path: str) -> None:
    """Delete an on-device file via the daemon."""
    dev = _config.resolve(device_name)
    _http.delete(dev, PATH_FILE, params={"path": path})


def get_intellisense_events(
    device_name: str,
    *,
    start_unix_ms: Optional[int] = None,
    end_unix_ms: Optional[int] = None,
) -> List[Dict[str, Any]]:
    """Fetch raw intellisense events from the daemon event store."""
    dev = _config.resolve(device_name)
    params: Dict[str, Any] = {}
    if start_unix_ms is not None:
        params["start_unix_ms"] = int(start_unix_ms)
    if end_unix_ms is not None:
        params["end_unix_ms"] = int(end_unix_ms)
    data = _http.get_json(dev, PATH_EVENTS, params=params or None)
    if isinstance(data, list):
        return data
    if isinstance(data, dict) and isinstance(data.get("events"), list):
        return data["events"]
    return []


def clear_intellisense_events(device_name: str) -> None:
    """Clear all buffered intellisense events on the daemon."""
    dev = _config.resolve(device_name)
    _http.delete(dev, PATH_EVENTS)


COMMANDS = {
    "fetch_file": fetch_file,
    "delete_file": delete_file,
    "get_intellisense_events": get_intellisense_events,
    "clear_intellisense_events": clear_intellisense_events,
}
COMMAND_SCHEMAS = {
    "fetch_file": {
        "required": {"device_name", "path"},
        "optional": {"max_inline_bytes"},
    },
    "delete_file": {"required": {"device_name", "path"}, "optional": set()},
    "get_intellisense_events": {
        "required": {"device_name"},
        "optional": {"start_unix_ms", "end_unix_ms"},
    },
    "clear_intellisense_events": {"required": {"device_name"}, "optional": set()},
}
