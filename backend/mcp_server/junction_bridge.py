"""Authenticated client for the desktop application's local Junction bridge.

The bridge is deliberately not a general-purpose HTTP client.  Its address and
bearer credential are injected by the desktop process, and only a loopback
endpoint named ``/junction`` can be reached.
"""

from __future__ import annotations

import ipaddress
import json
import os
from typing import Any, Dict
from urllib.error import HTTPError, URLError
from urllib.parse import urlsplit, urlunsplit
from urllib.request import Request, urlopen


BRIDGE_URL_ENV = "PLUMDECK_JUNCTION_BRIDGE_URL"
BRIDGE_TOKEN_ENV = "PLUMDECK_JUNCTION_BRIDGE_TOKEN"
MAX_EXCHANGE_TEXT_BYTES = 128 * 1024
MAX_BRIDGE_RESPONSE_BYTES = 4 * 1024 * 1024
BRIDGE_TIMEOUT_SECONDS = 15


class JunctionBridgeError(RuntimeError):
    """Safe, user-facing failure while contacting the local desktop bridge."""


def bounded_exchange_text(text: str) -> str:
    """Validate manual exchange text without ever including it in an error."""
    if not isinstance(text, str):
        raise ValueError("exchange_text must be a string")
    value = text.strip()
    if not value:
        raise ValueError("exchange_text must not be empty")
    if len(value.encode("utf-8")) > MAX_EXCHANGE_TEXT_BYTES:
        raise ValueError(
            f"exchange_text must be at most {MAX_EXCHANGE_TEXT_BYTES} UTF-8 bytes"
        )
    return value


def _bridge_endpoint() -> tuple[str, str]:
    raw_url = os.environ.get(BRIDGE_URL_ENV, "").strip()
    token = os.environ.get(BRIDGE_TOKEN_ENV, "")
    if not raw_url or not token:
        raise JunctionBridgeError(
            "Junction bridge is unavailable; open plumdeck desktop and keep it running"
        )
    if len(token) > 8192 or "\r" in token or "\n" in token:
        raise JunctionBridgeError("Junction bridge credentials are invalid")

    try:
        parsed = urlsplit(raw_url)
        hostname = parsed.hostname or ""
        is_loopback = hostname.lower() == "localhost"
        if not is_loopback:
            is_loopback = ipaddress.ip_address(hostname).is_loopback
        # Accessing parsed.port also validates malformed/out-of-range ports.
        port = parsed.port
    except (ValueError, TypeError):
        raise JunctionBridgeError("Junction bridge address is invalid") from None

    if (
        parsed.scheme != "http"
        or not parsed.netloc
        or not is_loopback
        or parsed.username is not None
        or parsed.password is not None
        or parsed.query
        or parsed.fragment
        or parsed.path not in ("", "/")
    ):
        raise JunctionBridgeError("Junction bridge must use a loopback HTTP address")
    if port is None:
        raise JunctionBridgeError("Junction bridge address must include a port")

    return urlunsplit(("http", parsed.netloc, "/junction", "", "")), token


def post_junction_action(action: str, arguments: Dict[str, Any]) -> Dict[str, Any]:
    """POST one allowlisted semantic action and return the bridge JSON object.

    Action names are supplied only by the explicit MCP wrappers in ``tools``;
    callers never get a raw-operation MCP tool.
    """
    endpoint, token = _bridge_endpoint()
    body = json.dumps(
        {"action": action, "arguments": arguments},
        ensure_ascii=False,
        separators=(",", ":"),
    ).encode("utf-8")
    request = Request(
        endpoint,
        data=body,
        method="POST",
        headers={
            "Authorization": f"Bearer {token}",
            "Content-Type": "application/json; charset=utf-8",
            "Accept": "application/json",
        },
    )
    try:
        with urlopen(request, timeout=BRIDGE_TIMEOUT_SECONDS) as response:
            raw = response.read(MAX_BRIDGE_RESPONSE_BYTES + 1)
    except HTTPError as exc:
        # Never surface the body: it may contain an echoed exchange packet or
        # credential.  The status is sufficient for the desktop logs lookup.
        raise JunctionBridgeError(
            f"Junction bridge rejected the request (HTTP {exc.code})"
        ) from None
    except (URLError, TimeoutError, OSError):
        raise JunctionBridgeError(
            "Junction bridge is unavailable; open plumdeck desktop and keep it running"
        ) from None

    if len(raw) > MAX_BRIDGE_RESPONSE_BYTES:
        raise JunctionBridgeError("Junction bridge returned an oversized response")
    try:
        payload = json.loads(raw.decode("utf-8"))
    except (UnicodeDecodeError, json.JSONDecodeError):
        raise JunctionBridgeError("Junction bridge returned an invalid response") from None
    if not isinstance(payload, dict):
        raise JunctionBridgeError("Junction bridge returned an invalid response")
    return payload
