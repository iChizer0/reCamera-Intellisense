#!/usr/bin/env python3
"""
reCamera Device Manager.

Used for reCamera device connection profiles (host, token) management, local device
discovery, add, update, remove and list devices stored in ~/.recamera/devices.json with
validation and connectivity checks.

Refer to __all__ for the public API functions, COMMANDS and COMMAND_SCHEMAS for the CLI interface.
"""

from __future__ import annotations

import json
import os
import re
import sys
import tempfile
import http.client
import socket
import ssl
import urllib.error
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Dict, List, Optional, TypedDict


SCRIPTS_DIR = os.path.dirname(os.path.abspath(__file__))
if SCRIPTS_DIR not in sys.path:
    sys.path.append(SCRIPTS_DIR)

from rc_common import CONNECTION_TIMEOUT, print_json_stdout, validate_command_args  # noqa: E402


# MARK: Public API (Important)
__all__ = [
    "DeviceRecord",
    "detect_local_device",
    "get_device",
    "add_device",
    "update_device",
    "remove_device",
    "list_devices",
]


# MARK: Types (Important)


class DeviceRecord(TypedDict):
    name: str
    host: str
    token: str
    protocol: str  # one of "http" or "https", default to "http" if not specified
    allow_unsecured: (
        bool  # whether to allow self-signed certs when using HTTPS, default to True
    )


# MARK: Constants and globals
RECAMERA_DIR = Path.home() / ".recamera"
DEVICE_PROFILES_PATH = RECAMERA_DIR / "devices.json"
TOKEN_PATTERN = re.compile(r"^sk_[A-Za-z0-9_\-]+$")
TEST_LOCAL_PORT = 16384
TEST_PATH = "/api/v1/generate-204"


# MARK: Internal helpers


def _is_valid_host(host: str) -> bool:
    for family in (socket.AF_INET, socket.AF_INET6):
        try:
            socket.inet_pton(family, host)
            return True
        except OSError:
            continue
    hostname_re = re.compile(
        r"^[a-zA-Z0-9]([a-zA-Z0-9\-]*[a-zA-Z0-9])?"
        r"(\.[a-zA-Z0-9]([a-zA-Z0-9\-]*[a-zA-Z0-9])?)*$"
    )
    return hostname_re.match(host) is not None


def _ensure_dir() -> None:
    RECAMERA_DIR.mkdir(parents=True, exist_ok=True)


def _load_devices() -> Dict[str, Dict[str, Any]]:
    _ensure_dir()
    if not DEVICE_PROFILES_PATH.exists():
        _save_devices({})
        return {}

    try:
        with open(DEVICE_PROFILES_PATH, "r", encoding="utf-8") as fh:
            data = json.load(fh)
    except json.JSONDecodeError as e:
        raise RuntimeError(
            f"Credential store is corrupted: {DEVICE_PROFILES_PATH} contains invalid JSON. "
            "Please fix or replace the file and try again."
        ) from e
    except OSError as e:
        raise RuntimeError(
            f"Unable to read credential store {DEVICE_PROFILES_PATH}."
        ) from e

    if not isinstance(data, dict):
        raise RuntimeError(
            f"Credential store is invalid: {DEVICE_PROFILES_PATH} must contain a JSON object."
        )

    normalized: Dict[str, Dict[str, Any]] = {}
    for name, info in data.items():
        if not isinstance(name, str) or not isinstance(info, dict):
            continue
        host = info.get("host")
        token = info.get("token")
        if not isinstance(host, str) or not isinstance(token, str):
            continue
        entry: Dict[str, Any] = {"host": host.strip(), "token": token}
        protocol = info.get("protocol")
        if isinstance(protocol, str) and protocol in ("http", "https"):
            entry["protocol"] = protocol
        allow_unsecured = info.get("allow_unsecured")
        if isinstance(allow_unsecured, bool):
            entry["allow_unsecured"] = allow_unsecured
        normalized[name] = entry
    return normalized


def _save_devices(devices: Dict[str, Dict[str, Any]]) -> None:
    _ensure_dir()
    fd, tmp_path = tempfile.mkstemp(
        dir=str(RECAMERA_DIR), prefix=".devices_", suffix=".json"
    )
    try:
        with os.fdopen(fd, "w", encoding="utf-8") as fh:
            json.dump(devices, fh, indent=4, ensure_ascii=False)
            fh.write("\n")
        os.replace(tmp_path, str(DEVICE_PROFILES_PATH))
        try:
            os.chmod(DEVICE_PROFILES_PATH, 0o600)
        except OSError:
            pass
    except Exception as e:
        try:
            os.unlink(tmp_path)
        except OSError:
            pass
        raise RuntimeError(
            f"Failed to save credential store to {DEVICE_PROFILES_PATH}."
        ) from e


def _validate_name(name: str) -> Optional[str]:
    if not name or not isinstance(name, str):
        return "Device name must be a non-empty string."
    if not name.strip():
        return "Device name cannot be blank or whitespace only."
    return None


def _validate_host(host: str) -> Optional[str]:
    if not host or not isinstance(host, str):
        return "Host must be a non-empty string (IP address or hostname)."
    if not _is_valid_host(host.strip()):
        return (
            f"Invalid host format: '{host}'. "
            "Expected an IPv4 address (e.g. 192.168.1.100), "
            "IPv6 address (e.g. fe80::1), or a hostname (e.g. my-camera.local)."
        )
    return None


def _validate_token(token: str) -> Optional[str]:
    if not token or not isinstance(token, str):
        return "Token must be a non-empty string in the format 'sk_xxxxxxxxx'."
    if not TOKEN_PATTERN.match(token.strip()):
        return (
            f"Invalid token format: '{token}'. "
            "Expected format: 'sk_xxxxxxxxx' where 'sk_xxxxxxxxx' is your secret key."
        )
    return None


def _validate_protocol(protocol: str) -> Optional[str]:
    if protocol not in ("http", "https"):
        return f"Invalid protocol: '{protocol}'. Must be 'http' or 'https'."
    return None


def _test_connection(
    host: str,
    token: str,
    protocol: str = "http",
    allow_unsecured: bool = True,
) -> Optional[str]:
    try:
        connect_host = host.strip()
        try:
            socket.inet_pton(socket.AF_INET6, connect_host)
            connect_host = f"[{connect_host}]"
        except OSError:
            pass
        use_https = protocol == "https"
        port = 443 if use_https else 80
        if use_https:
            ssl_context = _build_ssl_context(allow_unsecured)
            conn = http.client.HTTPSConnection(
                connect_host, port, timeout=CONNECTION_TIMEOUT, context=ssl_context
            )
        else:
            conn = http.client.HTTPConnection(
                connect_host, port, timeout=CONNECTION_TIMEOUT
            )
        conn.request(
            "GET",
            TEST_PATH,
            headers={"Authorization": token.strip()},
        )
        resp = conn.getresponse()
        conn.close()
        if 200 <= resp.status < 300:
            return None  # NOTE: success
        if resp.status == 401 or resp.status == 403:
            return (
                f"Authentication failed (HTTP {resp.status}). "
                "Please verify the token and confirm it has not expired."
            )
        return (
            f"Unexpected response from device (HTTP {resp.status}). "
            "Please verify the host and confirm the device service is running correctly."
        )
    except socket.timeout:
        return (
            f"Connection to {host}:{port} timed out after {CONNECTION_TIMEOUT}s. "
            "Please verify the host and confirm the device is powered on and reachable."
        )
    except ConnectionRefusedError:
        return (
            f"Connection refused by {host}:{port}. "
            "Please verify the host and confirm the device API service is running."
        )
    except OSError as e:
        return (
            f"Unable to connect to {host}:{port} — {e}. "
            "Please check network settings and verify the host address."
        )


def _build_ssl_context(allow_unsecured: bool = True) -> ssl.SSLContext:
    """Build an SSL context for HTTPS connections."""
    if allow_unsecured:
        ctx = ssl.SSLContext(ssl.PROTOCOL_TLS_CLIENT)
        ctx.check_hostname = False
        ctx.verify_mode = ssl.CERT_NONE
        return ctx
    return ssl.create_default_context()


# MARK: Internal API functions


def get_device_ssl_context(device: DeviceRecord) -> Optional[ssl.SSLContext]:
    """Return an SSL context if the device uses HTTPS, otherwise None."""
    protocol = device.get("protocol", "http")
    if protocol != "https":
        return None
    return _build_ssl_context(device.get("allow_unsecured", True))


def get_device_api_url(device: DeviceRecord, endpoint: str) -> str:
    host = str(device["host"]).strip()
    protocol = device.get("protocol", "http")
    port = None
    if isinstance(device, dict):
        port = device.get("port")
    if port is None or str(port).strip() == "":
        return f"{protocol}://{host}{endpoint}"
    return f"{protocol}://{host}:{int(port)}{endpoint}"


def get_device_api_headers(device: DeviceRecord) -> Dict[str, str]:
    return {"Authorization": f"{device['token']}"}


def resolve_device_from_args(args: Dict[str, Any]) -> DeviceRecord:
    if "device_name" in args and "device" in args:
        raise ValueError("Provide either 'device_name' or 'device', not both.")

    if "device_name" in args:
        device_name = args["device_name"]
        if not isinstance(device_name, str) or not device_name.strip():
            raise ValueError("'device_name' must be a non-empty string.")
        record = get_device(device_name.strip())
        if record is None:
            raise LookupError(f"Device '{device_name}' not found. Add it first.")
        return record

    if "device" in args:
        raw = args["device"]
        if not isinstance(raw, dict):
            raise ValueError("'device' must be an object.")
        name = raw.get("name", "inline-device")
        host = raw.get("host")
        token = raw.get("token")
        if not isinstance(host, str) or not host.strip():
            raise ValueError("'device.host' must be a non-empty string.")
        if not isinstance(token, str) or not token.strip():
            raise ValueError("'device.token' must be a non-empty string.")
        resolved = DeviceRecord(
            name=str(name),
            host=host.strip(),
            token=token.strip(),
            protocol=str(raw.get("protocol", "http")),
            allow_unsecured=bool(raw.get("allow_unsecured", True)),
        )
        if "port" in raw:
            resolved["port"] = int(raw["port"])
        return resolved

    raise ValueError("Missing device reference. Provide 'device_name' or 'device'.")


def fetch_file(device: DeviceRecord, remote_path: str) -> bytes:
    request = urllib.request.Request(
        f"{get_device_api_url(device, '/api/v1/file')}?{urllib.parse.urlencode({'path': remote_path})}",
        headers=get_device_api_headers(device),
        method="GET",
    )
    ssl_context = get_device_ssl_context(device)
    try:
        with urllib.request.urlopen(
            request, timeout=CONNECTION_TIMEOUT, context=ssl_context
        ) as response:
            content = response.read()
            content_type = response.headers.get("Content-Type", "")
            if "application/json" in content_type.lower():
                try:
                    error_data = json.loads(content.decode("utf-8"))
                    error_message = error_data.get("error", "Unknown error")
                except (json.JSONDecodeError, UnicodeDecodeError):
                    error_message = content.decode("utf-8", errors="replace")
                raise RuntimeError(f"Failed to fetch file: {error_message}")
            return content
    except urllib.error.HTTPError as e:
        detail = ""
        if e.fp is not None:
            body = e.fp.read().decode("utf-8", errors="replace").strip()
            if body:
                detail = f" — {body}"
        raise RuntimeError(
            f"Failed to fetch file '{remote_path}': HTTP {e.code} {e.reason}{detail}"
        ) from e
    except urllib.error.URLError as e:
        raise RuntimeError(f"Failed to fetch file '{remote_path}': {e.reason}") from e
    except TimeoutError as e:
        raise RuntimeError(
            f"Failed to fetch file '{remote_path}': request timed out after {CONNECTION_TIMEOUT}s"
        ) from e


# MARK: Public API functions (Important)


def detect_local_device(host: str = "127.0.0.1") -> Optional[str]:
    """
    Detect camera on the local network (localhost), use for quick setup or testing.

    Return the detected *host* if found, otherwise return None.
    """
    conn = http.client.HTTPConnection(host, TEST_LOCAL_PORT, timeout=CONNECTION_TIMEOUT)
    try:
        conn.request("GET", TEST_PATH)
        resp = conn.getresponse()
        if 200 <= resp.status < 300:
            return host  # NOTE: only confirm presence of a device, not credentials
    except Exception:
        pass
    finally:
        try:
            conn.close()
        except Exception:
            pass


def get_device(name: str) -> Optional[DeviceRecord]:
    """
    Get the connection credentials of the camera identified by *name*.

    Return the *DeviceRecord* if found, otherwise return None.
    """
    devices = _load_devices()
    if name not in devices:
        return None
    entry = devices[name]
    return DeviceRecord(
        name=name,
        host=entry["host"],
        token=entry["token"],
        protocol=entry.get("protocol", "http"),
        allow_unsecured=entry.get("allow_unsecured", True),
    )


def add_device(
    name: str,
    host: str,
    token: str,
    protocol: str = "http",
    allow_unsecured: bool = True,
) -> None:
    """
    Add a new camera with the given *name*, *host*, and *token*.

    The name should be unique, the host and token are validated for format,
    and a connectivity test is performed before the camera is saved.

    Return None on success, otherwise raises when exceptions occur.
    """
    err = _validate_name(name)
    if err:
        raise ValueError(err)
    err = _validate_host(host)
    if err:
        raise ValueError(err)
    token = token.strip()
    err = _validate_token(token)
    if err:
        raise ValueError(err)
    err = _validate_protocol(protocol)
    if err:
        raise ValueError(err)
    err = _test_connection(
        host, token, protocol=protocol, allow_unsecured=allow_unsecured
    )
    if err:
        raise ConnectionError(err)
    devices = _load_devices()
    devices[name] = {
        "host": host.strip(),
        "token": token.strip(),
        "protocol": protocol,
        "allow_unsecured": allow_unsecured,
    }
    _save_devices(devices)


def update_device(
    name: str,
    host: Optional[str] = None,
    token: Optional[str] = None,
    protocol: Optional[str] = None,
    allow_unsecured: Optional[bool] = None,
) -> None:
    """
    Update the *host*, *token*, *protocol*, and/or *allow_unsecured* of an existing camera.

    At least one of the optional fields must be provided. A connectivity test is
    performed with the resulting credentials before saving.

    Return None on success, otherwise raises when exceptions occur.
    """
    err = _validate_name(name)
    if err:
        raise ValueError(err)
    if host is None and token is None and protocol is None and allow_unsecured is None:
        raise ValueError("Nothing to update — provide at least one field to change.")
    if host is not None:
        err = _validate_host(host)
        if err:
            raise ValueError(err)
    if token is not None:
        token = token.strip()
        err = _validate_token(token)
        if err:
            raise ValueError(err)
    if protocol is not None:
        err = _validate_protocol(protocol)
        if err:
            raise ValueError(err)
    devices = _load_devices()
    if name not in devices:
        raise ValueError(
            f"Device '{name}' not found. Use list_devices to view saved devices."
        )
    new_host = host if host is not None else devices[name]["host"]
    new_token = token if token is not None else devices[name]["token"]
    new_protocol = (
        protocol if protocol is not None else devices[name].get("protocol", "http")
    )
    new_allow_unsecured = (
        allow_unsecured
        if allow_unsecured is not None
        else devices[name].get("allow_unsecured", True)
    )
    err = _test_connection(
        new_host, new_token, protocol=new_protocol, allow_unsecured=new_allow_unsecured
    )
    if err:
        raise ConnectionError(err)
    devices[name]["host"] = new_host.strip()
    devices[name]["token"] = new_token.strip()
    devices[name]["protocol"] = new_protocol
    devices[name]["allow_unsecured"] = new_allow_unsecured
    _save_devices(devices)


def remove_device(name: str) -> bool:
    """
    Remove the camera identified by *name*.

    Return True when successfully removed, otherwise return False if the camera is not found
    or exceptions occur.
    """
    err = _validate_name(name)
    if err:
        return False
    devices = _load_devices()
    if name not in devices:
        return False
    del devices[name]
    _save_devices(devices)
    return True


def list_devices() -> List[DeviceRecord]:
    """
    List all cameras with their connection credentials, sorted by name.

    Returns a list of *DeviceRecord* objects on success, may be empty if no devices are saved.
    """
    devices = _load_devices()
    items: List[DeviceRecord] = [
        DeviceRecord(
            name=name,
            host=info.get("host", ""),
            token=info.get("token", ""),
            protocol=info.get("protocol", "http"),
            allow_unsecured=info.get("allow_unsecured", True),
        )
        for name, info in sorted(devices.items(), key=lambda item: item[0].lower())
    ]
    return items


# MARK: CLI interface (Important)
COMMANDS = {
    "detect_local_device": detect_local_device,
    "get_device": get_device,
    "add_device": add_device,
    "update_device": update_device,
    "remove_device": remove_device,
    "list_devices": list_devices,
}
COMMAND_SCHEMAS = {
    "detect_local_device": {"required": set(), "optional": {"host"}},
    "get_device": {"required": {"name"}, "optional": set()},
    "add_device": {
        "required": {"name", "host", "token"},
        "optional": {"protocol", "allow_unsecured"},
    },
    "update_device": {
        "required": {"name"},
        "optional": {"host", "token", "protocol", "allow_unsecured"},
    },
    "remove_device": {"required": {"name"}, "optional": set()},
    "list_devices": {"required": set(), "optional": set()},
}


def _usage() -> str:
    return (
        "Usage: python3 rc_device.py <command> [json-args]\n\n"
        "Commands:\n"
        "  detect_local_device\n"
        '  get_device          \'{"name": "..."}\'\n'
        '  add_device          \'{"name": "...", "host": "...", "token": "..."}\'\n'
        '                      \'{"name": "...", "host": "...", "token": "...", "protocol": "https", "allow_unsecured": true}\' \n'
        '  update_device       \'{"name": "...", "host": "...", "token": "..."}\' \n'
        '                      \'{"name": "...", "protocol": "https", "allow_unsecured": false}\' \n'
        '  remove_device       \'{"name": "..."}\'\n'
        "  list_devices\n\n"
    )


def _build_call_kwargs(command: str, args: dict) -> dict:
    return args


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
