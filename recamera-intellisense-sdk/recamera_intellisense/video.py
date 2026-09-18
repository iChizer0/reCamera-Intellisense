"""Video stream encode settings (``/video/{0,1}/encode``), read-only."""

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

__all__ = ["get_video_encode"]

PATH_ENCODE = "/cgi-bin/entry.cgi/video/{}/encode"

_STREAM_IDS = {"main": 0, "sub": 1}


def get_video_encode(
    device_name: Optional[str] = None,
    *,
    stream: str = "main",
) -> Dict[str, Any]:
    """Encode parameters for `stream` (``"main"``|``"sub"``):
    ``{stream, stream_type, enabled, codec, resolution, frame_rate, gop,
    rc_mode, rc_quality, max_rate(kbps)}``."""
    key = str(stream).strip().lower()
    if key not in _STREAM_IDS:
        raise ValueError(
            f"stream must be one of {sorted(_STREAM_IDS)}; got {stream!r}"
        )
    dev = _config.resolve(device_name)
    d = _http.get_json(dev, PATH_ENCODE.format(_STREAM_IDS[key]))
    if not isinstance(d, dict):
        raise RecameraError("get video encode failed: unexpected payload")
    return {
        "stream": key,
        "stream_type": d.get("sStreamType", ""),
        "enabled": bool(d.get("iEnabled", 0)),
        "codec": d.get("sOutputDataType", ""),
        "resolution": d.get("sResolution", ""),
        "frame_rate": d.get("sFrameRate", ""),
        "gop": int(d.get("iGOP", 0)),
        "rc_mode": d.get("sRCMode", ""),
        "rc_quality": d.get("sRCQuality", ""),
        "max_rate": int(d.get("iMaxRate", 0)),
    }


COMMANDS = {"get_video_encode": get_video_encode}
