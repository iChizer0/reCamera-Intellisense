import json
import ssl
import urllib.parse
import urllib.request
from pathlib import Path
from typing import Any, Dict, Optional


# MARK: Constants and globals
CONNECTION_TIMEOUT = 3  # seconds


# MARK: Internal helpers


def _request_json(
    url: str,
    headers: Dict[str, str],
    *,
    method: str,
    error_prefix: str,
    params: Optional[Dict[str, str]] = None,
    payload: Optional[Dict[str, Any]] = None,
    ssl_context: Optional[ssl.SSLContext] = None,
) -> Any:
    target_url = url
    if params:
        target_url = f"{url}?{urllib.parse.urlencode(params)}"
    request_headers = dict(headers)
    body: Optional[bytes] = None
    if payload is not None:
        request_headers["Content-Type"] = "application/json"
        body = json.dumps(payload).encode("utf-8")
    request = urllib.request.Request(
        target_url,
        data=body,
        headers=request_headers,
        method=method,
    )
    try:
        with urllib.request.urlopen(
            request, timeout=CONNECTION_TIMEOUT, context=ssl_context
        ) as response:
            raw = response.read()
    except urllib.error.HTTPError as e:
        raise RuntimeError(f"{error_prefix}: HTTP {e.code} {e.reason}") from e
    except urllib.error.URLError as e:
        raise RuntimeError(f"{error_prefix}: {e.reason}") from e
    except TimeoutError as e:
        raise RuntimeError(
            f"{error_prefix}: request timed out after {CONNECTION_TIMEOUT}s"
        ) from e
    try:
        return json.loads(raw.decode("utf-8"))
    except (json.JSONDecodeError, UnicodeDecodeError) as e:
        raise RuntimeError(f"{error_prefix}: invalid JSON response") from e


# MARK: Internal API functions


def req_get_json(
    url: str,
    headers: Dict[str, str],
    *,
    error_prefix: str,
    params: Optional[Dict[str, str]] = None,
    ssl_context: Optional[ssl.SSLContext] = None,
) -> Any:
    return _request_json(
        url,
        headers,
        method="GET",
        error_prefix=error_prefix,
        params=params,
        ssl_context=ssl_context,
    )


def req_post_json(
    url: str,
    headers: Dict[str, str],
    *,
    error_prefix: str,
    params: Optional[Dict[str, str]] = None,
    payload: Optional[Dict[str, Any]] = None,
    ssl_context: Optional[ssl.SSLContext] = None,
) -> Any:
    return _request_json(
        url,
        headers,
        method="POST",
        error_prefix=error_prefix,
        params=params,
        payload=payload,
        ssl_context=ssl_context,
    )


def serialize_json(value: Any) -> Any:
    if isinstance(value, Path):
        return str(value)
    if isinstance(value, tuple):
        return [serialize_json(item) for item in value]
    if isinstance(value, list):
        return [serialize_json(item) for item in value]
    if isinstance(value, dict):
        return {str(k): serialize_json(v) for k, v in value.items()}
    if hasattr(value, "__dict__"):
        return serialize_json(vars(value))
    return value


def print_json_stdout(payload: Any) -> None:
    print(
        json.dumps(serialize_json(payload), separators=(",", ":"), ensure_ascii=False)
    )


def validate_command_args(
    command: str, args: Dict[str, Any], schemas: Dict[str, Any]
) -> None:
    schema = schemas[command]
    required = schema.get("required", set())
    required_one_of = schema.get("required_one_of", [])
    optional = schema.get("optional", set())

    allowed = set(required) | set(optional)
    for group in required_one_of:
        allowed |= set(group)

    unknown = sorted(set(args.keys()) - allowed)
    if unknown:
        raise ValueError(f"Unknown field(s): {', '.join(unknown)}")

    missing = sorted(set(required) - set(args.keys()))
    if missing:
        raise ValueError(f"Missing required field(s): {', '.join(missing)}")

    for group in required_one_of:
        present = [key for key in group if key in args]
        if len(present) == 0:
            pretty = " or ".join(f"'{field}'" for field in group)
            raise ValueError(f"Missing required field: provide {pretty}.")
        if len(present) > 1:
            pretty = ", ".join(f"'{field}'" for field in group)
            raise ValueError(f"Provide only one of: {pretty}.")
