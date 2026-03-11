#!/usr/bin/env python3
"""
reCamera GPIO Manager.

Used for reCamera GPIO pin control and monitoring, including: listing all available GPIO pins,
querying pin details and current values, and setting output pin values/reading input pin values with
pin settings automatically configured.

Refer to __all__ for the public API functions, COMMANDS and COMMAND_SCHEMAS for the CLI interface.
"""

from __future__ import annotations

import enum
import json
import os.path as osp
import sys
import urllib.error
import urllib.request
from typing import Any, Dict, List, TypedDict

SCRIPTS_DIR = osp.dirname(osp.abspath(__file__))
if SCRIPTS_DIR not in sys.path:
    sys.path.append(SCRIPTS_DIR)

from rc_common import (  # noqa: E402
    CONNECTION_TIMEOUT,
    req_get_json,
    req_post_json,
    print_json_stdout,
    validate_command_args,
)
from rc_device import (  # noqa: E402
    DeviceRecord,
    get_device_api_url,
    get_device_api_headers,
    get_device_ssl_context,
    resolve_device_from_args,
)


# MARK: Public API (Important)
__all__ = [
    "GPIOState",
    "EdgeDetect",
    "PinInfo",
    "PinSettings",
    "PinDescriptor",
    "list_gpios",
    "get_gpio_info",
    "set_gpio_value",
    "get_gpio_value",
]


# MARK: Types (Important)


class GPIOState(enum.Enum):
    ERROR = "error"
    DISABLED = "disabled"
    PUSH_PULL = "push-pull"
    OPEN_DRAIN = "open-drain"
    OPEN_SOURCE = "open-source"
    FLOATING = "floating"
    PULL_UP = "pull-up"
    PULL_DOWN = "pull-down"


class EdgeDetect(enum.Enum):
    NONE = "none"
    RISING = "rising"
    FALLING = "falling"
    BOTH = "both"


class PinInfo(TypedDict):
    name: str
    chip: str
    line: int
    capabilities: List[GPIOState]


class PinSettings(TypedDict):
    state: GPIOState
    edge: EdgeDetect
    debounce_ms: int


class PinDescriptor(TypedDict):
    pin_id: int
    info: PinInfo
    settings: PinSettings


# MARK: Constants and globals
GPIO_API_BASE = "/api/v1"
GPIO_OUTPUT_STATE = GPIOState.PUSH_PULL
GPIO_INPUT_STATE = GPIOState.FLOATING
GPIO_OUTPUT_STATES = {
    GPIOState.PUSH_PULL,
    GPIOState.OPEN_DRAIN,
    GPIOState.OPEN_SOURCE,
}
GPIO_DEBOUNCE_MS_DEFAULT = 100
GPIO_WATCH_EDGE_DEFAULT = EdgeDetect.BOTH
GPIO_INPUT_STATES = {
    GPIOState.FLOATING,
    GPIOState.PULL_UP,
    GPIOState.PULL_DOWN,
}


# MARK: Internal helpers


def _gpio_url(device: DeviceRecord, path: str) -> str:
    return get_device_api_url(device, f"{GPIO_API_BASE}{path}")


def _parse_pin_info(data: Dict[str, Any]) -> PinInfo:
    raw_caps = data.get("capabilities", [])
    caps: List[GPIOState] = []
    if isinstance(raw_caps, list):
        for c in raw_caps:
            try:
                caps.append(GPIOState(c))
            except ValueError:
                pass
    return PinInfo(
        name=str(data.get("name", "")),
        chip=str(data.get("chip", "")),
        line=int(data.get("line", 0)),
        capabilities=caps,
    )


def _parse_pin_settings(data: Dict[str, Any]) -> PinSettings:
    return PinSettings(
        state=GPIOState(data.get("state", "disabled")),
        edge=EdgeDetect(data.get("edge", "none")),
        debounce_ms=int(data.get("debounce_ms", 0)),
    )


def _parse_pin_descriptor(pin_id: int, data: Dict[str, Any]) -> PinDescriptor:
    info_raw = data.get("info", {})
    settings_raw = data.get("settings", {})
    return PinDescriptor(
        pin_id=pin_id,
        info=_parse_pin_info(info_raw if isinstance(info_raw, dict) else {}),
        settings=_parse_pin_settings(
            settings_raw if isinstance(settings_raw, dict) else {}
        ),
    )


def _get_pin_settings(device: DeviceRecord, pin_id: int) -> PinSettings:
    url = _gpio_url(device, f"/gpio/{pin_id}/settings")
    headers = get_device_api_headers(device)
    ssl_ctx = get_device_ssl_context(device)
    data = req_get_json(
        url,
        headers,
        error_prefix=f"Failed to get settings for GPIO pin {pin_id}",
        ssl_context=ssl_ctx,
    )
    if not isinstance(data, dict):
        raise RuntimeError("Invalid response format: expected an object")
    return _parse_pin_settings(data)


def _ensure_output(device: DeviceRecord, pin_id: int) -> None:
    settings = _get_pin_settings(device, pin_id)
    if settings["state"] in GPIO_OUTPUT_STATES:
        return
    url = _gpio_url(device, f"/gpio/{pin_id}/settings")
    headers = get_device_api_headers(device)
    ssl_ctx = get_device_ssl_context(device)
    req_post_json(
        url,
        headers,
        payload={"state": GPIO_OUTPUT_STATE.value},
        error_prefix=f"Failed to configure GPIO pin {pin_id} as output",
        ssl_context=ssl_ctx,
    )


def _ensure_input(
    device: DeviceRecord,
    pin_id: int,
    debounce_ms: int | None = None,
    edge: EdgeDetect | None = None,
) -> None:
    settings = _get_pin_settings(device, pin_id)
    state_ok = settings["state"] in GPIO_INPUT_STATES
    debounce_ok = debounce_ms is None or settings["debounce_ms"] == debounce_ms
    edge_ok = edge is None or settings["edge"] == edge
    if state_ok and debounce_ok and edge_ok:
        return
    url = _gpio_url(device, f"/gpio/{pin_id}/settings")
    headers = get_device_api_headers(device)
    ssl_ctx = get_device_ssl_context(device)
    payload: Dict[str, Any] = {}
    if not state_ok:
        payload["state"] = GPIO_INPUT_STATE.value
    if not debounce_ok:
        payload["debounce_ms"] = debounce_ms
    if not edge_ok:
        payload["edge"] = edge.value  # type: ignore[union-attr]
    req_post_json(
        url,
        headers,
        payload=payload,
        error_prefix=f"Failed to configure GPIO pin {pin_id} as input",
        ssl_context=ssl_ctx,
    )


def _read_value(device: DeviceRecord, pin_id: int) -> int:
    url = _gpio_url(device, f"/gpio/{pin_id}/value")
    headers = get_device_api_headers(device)
    ssl_ctx = get_device_ssl_context(device)
    data = req_get_json(
        url,
        headers,
        error_prefix=f"Failed to get value for GPIO pin {pin_id}",
        ssl_context=ssl_ctx,
    )
    return int(data)


def _write_value(
    device: DeviceRecord,
    pin_id: int,
    value: int,
) -> None:
    url = _gpio_url(device, f"/gpio/{pin_id}/value")
    headers = dict(get_device_api_headers(device))
    headers["Content-Type"] = "text/plain"
    ssl_ctx = get_device_ssl_context(device)
    body = str(value).encode("utf-8")
    request = urllib.request.Request(url, data=body, headers=headers, method="POST")
    try:
        with urllib.request.urlopen(
            request, timeout=CONNECTION_TIMEOUT, context=ssl_ctx
        ) as response:
            response.read()
    except urllib.error.HTTPError as e:
        raise RuntimeError(
            f"Failed to set value for GPIO pin {pin_id}: HTTP {e.code} {e.reason}"
        ) from e
    except urllib.error.URLError as e:
        raise RuntimeError(
            f"Failed to set value for GPIO pin {pin_id}: {e.reason}"
        ) from e
    except TimeoutError as e:
        raise RuntimeError(
            f"Failed to set value for GPIO pin {pin_id}: request timed out after {CONNECTION_TIMEOUT}s"
        ) from e


# MARK: Public API functions (Important)


def list_gpios(device: DeviceRecord) -> List[PinDescriptor]:
    """
    List all GPIO pins with their info and current settings.

    Return a list of *PinDescriptor* objects on success, otherwise raise an error.
    """
    url = _gpio_url(device, "/gpios")
    headers = get_device_api_headers(device)
    ssl_ctx = get_device_ssl_context(device)
    data = req_get_json(
        url,
        headers,
        error_prefix="Failed to list GPIO pins",
        ssl_context=ssl_ctx,
    )
    if not isinstance(data, dict):
        raise RuntimeError(
            "Invalid response format: expected a JSON object keyed by pin ID"
        )
    return [
        _parse_pin_descriptor(int(pin_id), pin_data)
        for pin_id, pin_data in data.items()
        if isinstance(pin_data, dict)
    ]


def get_gpio_info(device: DeviceRecord, pin_id: int) -> PinDescriptor:
    """
    Get detailed information about a specific GPIO pin.

    Return a *PinDescriptor* with pin info and current settings on success,
    otherwise raise an error.
    """
    url = _gpio_url(device, f"/gpio/{pin_id}")
    headers = get_device_api_headers(device)
    ssl_ctx = get_device_ssl_context(device)
    data = req_get_json(
        url,
        headers,
        error_prefix=f"Failed to get info for GPIO pin {pin_id}",
        ssl_context=ssl_ctx,
    )
    if not isinstance(data, dict):
        raise RuntimeError("Invalid response format: expected an object")
    return _parse_pin_descriptor(pin_id, data)


def set_gpio_value(device: DeviceRecord, pin_id: int, value: int) -> int:
    """
    Set the value of a GPIO pin (0 or 1), auto-configures the pin as output if not already.

    Return the value that was set on success, otherwise raise an error.
    """
    if value not in (0, 1):
        raise ValueError(f"GPIO value must be 0 or 1, got {value}")
    _ensure_output(device, pin_id)
    _write_value(device, pin_id, value)
    return value


def get_gpio_value(
    device: DeviceRecord, pin_id: int, debounce_ms: int = GPIO_DEBOUNCE_MS_DEFAULT
) -> int:
    """
    Get the current value of a GPIO pin, auto-configures the pin as input if not already.

    Set the *debounce_ms* (milliseconds) on the pin before reading (default 100ms).

    Return the current pin value (0 or 1) on success, otherwise raise an error.
    """
    _ensure_input(device, pin_id, debounce_ms=debounce_ms)
    return _read_value(device, pin_id)


# MARK: CLI interface (Important)
COMMANDS = {
    "list_gpios": list_gpios,
    "get_gpio_info": get_gpio_info,
    "set_gpio_value": set_gpio_value,
    "get_gpio_value": get_gpio_value,
}
COMMAND_SCHEMAS = {
    "list_gpios": {
        "required_one_of": [("device_name", "device")],
        "optional": set(),
    },
    "get_gpio_info": {
        "required": {"pin_id"},
        "required_one_of": [("device_name", "device")],
        "optional": set(),
    },
    "set_gpio_value": {
        "required": {"pin_id", "value"},
        "required_one_of": [("device_name", "device")],
        "optional": set(),
    },
    "get_gpio_value": {
        "required": {"pin_id"},
        "required_one_of": [("device_name", "device")],
        "optional": {"debounce_ms"},
    },
}


def _usage() -> str:
    return (
        "Usage: python3 rc_gpio.py <command> [json-args]\n\n"
        "Commands:\n"
        '  list_gpios          \'{"device_name":"cam1"}\'\n'
        '  get_gpio_info       \'{"device_name":"cam1","pin_id":42}\'\n'
        '  set_gpio_value      \'{"device_name":"cam1","pin_id":42,"value":1}\'\n'
        '  get_gpio_value      \'{"device_name":"cam1","pin_id":42}\'\n'
        '                      \'{"device_name":"cam1","pin_id":42,"debounce_ms":100}\'\n\n'
        "Device resolution:\n"
        '  - Provide either "device_name" or inline "device" object\n'
        '  - Inline device format: {"name":"...","host":"...","token":"..."[,"port":80]}\n\n'
    )


def _build_call_kwargs(command: str, args: Dict[str, Any]) -> Dict[str, Any]:
    kwargs: Dict[str, Any] = {"device": resolve_device_from_args(args)}

    if command in (
        "get_gpio_info",
        "set_gpio_value",
        "get_gpio_value",
    ):
        kwargs["pin_id"] = int(args["pin_id"])

    if command == "set_gpio_value":
        kwargs["value"] = int(args["value"])

    if command == "get_gpio_value":
        if "debounce_ms" in args:
            kwargs["debounce_ms"] = int(args["debounce_ms"])

    return kwargs


def main() -> None:
    if len(sys.argv) < 2 or sys.argv[1] in ("-h", "--help"):
        print(_usage())
        sys.exit(0)

    command = sys.argv[1]
    if command not in COMMANDS:
        print(
            f"Unknown command: '{command}'. Available: {', '.join(COMMANDS.keys())}.",
            file=sys.stderr,
        )
        sys.exit(2)

    if len(sys.argv) > 3:
        print(
            f"Command '{command}' accepts at most one JSON object argument.",
            file=sys.stderr,
        )
        sys.exit(2)

    args: Dict[str, Any] = {}
    if len(sys.argv) == 3:
        try:
            loaded = json.loads(sys.argv[2])
        except json.JSONDecodeError as e:
            print(f"Invalid JSON argument: {e}", file=sys.stderr)
            sys.exit(2)
        if not isinstance(loaded, dict):
            print("Command arguments must be a JSON object.", file=sys.stderr)
            sys.exit(2)
        args = loaded

    try:
        validate_command_args(command, args, COMMAND_SCHEMAS)
        call_kwargs = _build_call_kwargs(command, args)
    except LookupError as e:
        print(str(e), file=sys.stderr)
        sys.exit(3)
    except Exception as e:
        print(f"Invalid arguments for command '{command}': {e}", file=sys.stderr)
        sys.exit(2)

    try:
        result = COMMANDS[command](**call_kwargs)
        if result is None:
            print_json_stdout({"ok": True})
        else:
            print_json_stdout(result)
        sys.exit(0)
    except Exception as e:
        print(f"Error executing command '{command}': {e}", file=sys.stderr)
        sys.exit(4)


if __name__ == "__main__":
    main()
