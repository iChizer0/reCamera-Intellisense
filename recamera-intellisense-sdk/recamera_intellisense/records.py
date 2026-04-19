"""Record browsing via the relay + nginx autoindex (paths are relative to the slot's data dir)."""

from __future__ import annotations

import base64
import json
from typing import Any, Dict, List, Optional

from . import _config, _http, relay as _relay
from ._errors import RecameraError

__all__ = ["list_records", "fetch_record"]

_MAX_INLINE_BYTES = 5 * 1024 * 1024
_IMAGE_EXT = {".jpg", ".jpeg", ".png", ".webp", ".bmp", ".gif"}


def _relay_endpoint(uuid: str, rel: str, *, directory: bool = False) -> str:
    rel = rel.strip("/")
    if not rel:
        return f"/storage/relay/{uuid}/"
    if directory:
        return f"/storage/relay/{uuid}/{rel}/"
    return f"/storage/relay/{uuid}/{rel}"


def list_records(
    device_name: str,
    *,
    path: str = "",
    dev_path: Optional[str] = None,
) -> List[Dict[str, Any]]:
    """List the directory at *path* (relative to the record data directory)."""
    dev = _config.resolve(device_name)
    resolved_dev_path, uuid = _relay.ensure_relay_uuid(device_name, dev_path)
    del resolved_dev_path
    endpoint = _relay_endpoint(uuid, path, directory=True)
    body, _ = _http.get_bytes(dev, endpoint)
    return _parse_autoindex(body)


def _parse_autoindex(body: bytes) -> List[Dict[str, Any]]:
    try:
        data = json.loads(body.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError) as exc:
        raise RecameraError(
            "Directory listing is not JSON (autoindex disabled?)"
        ) from exc
    if isinstance(data, dict) and "code" in data and "name" not in data:
        raise RecameraError(
            f"Directory listing not supported by device "
            f"(code={data.get('code')}): {data.get('message', 'unknown error')}"
        )
    if not isinstance(data, list):
        raise RecameraError("Autoindex response is not a JSON array.")
    out: List[Dict[str, Any]] = []
    for item in data:
        if not isinstance(item, dict):
            continue
        entry = {
            "name": item.get("name", ""),
            "type": item.get("type", ""),
            "mtime": item.get("mtime"),
        }
        if "size" in item:
            entry["size"] = int(item["size"])
        out.append(entry)
    return out


def fetch_record(
    device_name: str,
    *,
    path: str,
    dev_path: Optional[str] = None,
    max_inline_bytes: int = _MAX_INLINE_BYTES,
) -> Dict[str, Any]:
    """Fetch a recorded file via the relay.

    Returns one of:
      * ``{path, content_type, content_base64, size, url}`` — images or payloads ≤ 5 MiB.
      * ``{path, url, size, content_type, note}`` — payload too large to inline.
    """
    dev = _config.resolve(device_name)
    _, uuid = _relay.ensure_relay_uuid(device_name, dev_path)
    rel = path.strip("/")
    endpoint = _relay_endpoint(uuid, rel)
    body, ct = _http.get_bytes(dev, endpoint)
    url = _relay.build_relay_url(device_name, uuid, rel)
    is_image = any(rel.lower().endswith(ext) for ext in _IMAGE_EXT)
    if is_image or len(body) <= max_inline_bytes:
        return {
            "path": rel,
            "content_type": ct,
            "content_base64": base64.b64encode(body).decode("ascii"),
            "size": len(body),
            "url": url,
        }
    return {
        "path": rel,
        "url": url,
        "size": len(body),
        "content_type": ct,
        "note": "payload exceeds inline budget; fetch the URL directly (relay token is bearer-free).",
    }


COMMANDS = {
    "list_records": list_records,
    "fetch_record": fetch_record,
}
COMMAND_SCHEMAS = {
    "list_records": {"required": {"device_name"}, "optional": {"path", "dev_path"}},
    "fetch_record": {
        "required": {"device_name", "path"},
        "optional": {"dev_path", "max_inline_bytes"},
    },
}
