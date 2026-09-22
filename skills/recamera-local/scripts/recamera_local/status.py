"""System endpoints + the composite `get_status` snapshot (`/system/...`)."""

from __future__ import annotations

from typing import Any, Dict

from . import _local as http
from ._coerce import require_confirm
from ._errors import RecameraError

__all__ = [
    "get_device_info",
    "get_resource_info",
    "get_system_time",
    "get_battery_status",
    "reboot_device",
    "get_status",
]

PATH_DEVICE_INFO = "/cgi-bin/entry.cgi/system/device-info"
PATH_RESOURCE_INFO = "/cgi-bin/entry.cgi/system/resource-info"
PATH_TIME = "/cgi-bin/entry.cgi/system/time"
PATH_BATTERY = "/cgi-bin/entry.cgi/system/battery"
PATH_REBOOT = "/cgi-bin/entry.cgi/system/reboot"


def get_device_info() -> Dict[str, Any]:
    """Firmware/hardware identity of the device."""
    d = http.get_json(PATH_DEVICE_INFO) or {}
    return {
        "serial_number": d.get("sSerialNumber"),
        "firmware_version": d.get("sFirmwareVersion"),
        "sensor_model": d.get("sSensorModel"),
        "base_plate_model": d.get("sBasePlateModel"),
    }


def _usage_block(d: Dict[str, Any], total: str, used: str, pct: str) -> Dict[str, Any]:
    return {"total_gb": d.get(total), "used_gb": d.get(used), "usage_percent": d.get(pct)}


def get_resource_info() -> Dict[str, Any]:
    """CPU/NPU/memory/storage utilisation (percentages 0-100)."""
    d = http.get_json(PATH_RESOURCE_INFO) or {}
    return {
        "cpu_usage": d.get("iCpuUsage"),
        "npu_usage": d.get("iNpuUsage"),
        "memory": _usage_block(d.get("sMem") or {}, "iMemTotal", "iMemUsed", "iMemUsage"),
        "storage": _usage_block(
            d.get("sStorage") or {}, "iStorageTotal", "iStorageUsed", "iStorageUsage"
        ),
    }


def get_system_time() -> Dict[str, Any]:
    """Device clock, timezone, and NTP configuration."""
    d = http.get_json(PATH_TIME) or {}
    ntp = d.get("dNtpConfig") or {}
    return {
        "method": d.get("sMethod"),
        "timestamp": d.get("iTimestamp"),
        "timezone": d.get("sTimezone"),
        "tz": d.get("sTz"),
        "ntp": {"address": ntp.get("sAddress"), "port": ntp.get("sPort")},
    }


def get_battery_status() -> Dict[str, Any]:
    """Battery/power status: ``{attached, charging, display_steps,
    total_steps}``. ``attached`` is false on base plates without a battery."""
    d = http.get_json(PATH_BATTERY) or {}
    return {
        "attached": bool(d.get("isAttached", False)),
        "charging": bool(d.get("isCharging", False)),
        "display_steps": int(d.get("displaySteps", 0)),
        "total_steps": int(d.get("totalSteps", 0)),
    }


def reboot_device(*, confirm: bool = False) -> None:
    """Reboot the device. Disruptive: all streams, captures, and sessions drop.

    Requires `confirm=True`.
    """
    require_confirm(confirm, "reboot device")
    resp = http.post_json(PATH_REBOOT)
    http.expect_ok(resp, "reboot device")


def get_status() -> Dict[str, Any]:
    """One-call snapshot: identity, load, storage, capture, rule pipeline, model.

    The cheap "where do I stand" first call — replaces six round-trips of
    individual getters. Individual getters stay available for drilling down.
    """
    from . import capture as _capture
    from . import detect as _detect
    from . import storage as _storage
    from . import trigger as _trigger

    slots = [
        {
            "dev_path": s["dev_path"],
            "mount_path": s["mount_path"],
            "enabled": s["enabled"],
            "selected": s["selected"],
            "state": s["state"],
            "free_bytes": s["free_bytes"],
            "size_bytes": s["size_bytes"],
            "quota_rotate": s["quota_rotate"],
        }
        for s in _storage.get_storage_status()
    ]
    record_config = _trigger.get_record_config()
    try:
        active_trigger = _trigger.get_record_trigger()
    except (ValueError, RecameraError):
        active_trigger = None
    try:
        detection_model = _detect.get_detection_model()
    except RecameraError:
        detection_model = None
    try:
        battery = get_battery_status()
    except RecameraError:
        battery = None  # base plates without a battery may not serve this
    return {
        "device": get_device_info(),
        "resources": get_resource_info(),
        "battery": battery,
        "storage": slots,
        "capture": _capture.get_capture_status(),
        "record": {**record_config, "trigger": active_trigger},
        "detection_model": detection_model,
    }


COMMANDS = {
    "get_device_info": get_device_info,
    "get_resource_info": get_resource_info,
    "get_system_time": get_system_time,
    "get_battery_status": get_battery_status,
    "reboot_device": reboot_device,
    "get_status": get_status,
}
