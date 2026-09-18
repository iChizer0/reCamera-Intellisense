"""Acoustic / sound-event detection model API (``/extension/acousticslab``)."""

from __future__ import annotations

if __name__ == "__main__" and __package__ is None:
    import os
    import sys

    sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
    from recamera_intellisense._cli import main

    raise SystemExit(main())

from typing import Any, Dict, Optional

from . import _config, _http

__all__ = ["get_active_acoustic_model"]

PATH_ACTIVE = "/extension/acousticslab/api/v1/active"


def get_active_acoustic_model(device_name: Optional[str] = None) -> Optional[Dict[str, Any]]:
    """Return the active sound-event model, or `None`.

    Keys: `runtime_head_id`, `labels`, `n_classes?`, `sha256?`,
    `activated_at?`. Use `labels` for the `label_filter` of an inference_set
    record rule with `source_filter=["acousticslab"]` (the retired `sed`
    trigger kind is gone). Returns `None` when the AcousticsLab app is
    stopped — it is lifecycle-managed from the App Center; start it there
    (or via the AcousticsLab console) first. `get_record_sources` exposes
    the same labels together with the source's running state.
    """
    dev = _config.resolve(device_name)
    data = _http.get_json(dev, PATH_ACTIVE)
    if not isinstance(data, dict):
        return None
    labels = [c for c in (data.get("labels") or []) if isinstance(c, str)]
    if not data.get("runtime_head_id") and not labels:
        return None
    return {
        "runtime_head_id": data.get("runtime_head_id", ""),
        "labels": labels,
        "n_classes": data.get("n_classes"),
        "sha256": data.get("sha256"),
        "activated_at": data.get("activated_at"),
    }


COMMANDS = {"get_active_acoustic_model": get_active_acoustic_model}
