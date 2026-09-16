"""Isolated tests for the authenticated local Junction MCP bridge."""

from __future__ import annotations

import json
import threading
from contextlib import contextmanager
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

import pytest
from mcp.server.mcpserver.exceptions import ToolError

from mcp_server.junction_bridge import (
    BRIDGE_TOKEN_ENV,
    BRIDGE_URL_ENV,
    MAX_EXCHANGE_TEXT_BYTES,
    JunctionBridgeError,
    post_junction_action,
)
from mcp_server.tools import junction


@contextmanager
def _fake_bridge(response=None, status=200):
    calls = []
    response = response if response is not None else {"ok": True, "result": {"active": True}}

    class Handler(BaseHTTPRequestHandler):
        def do_POST(self):
            length = int(self.headers.get("Content-Length", "0"))
            body = self.rfile.read(length)
            calls.append({
                "path": self.path,
                "authorization": self.headers.get("Authorization"),
                "content_type": self.headers.get("Content-Type"),
                "body": json.loads(body),
            })
            encoded = json.dumps(response).encode("utf-8")
            self.send_response(status)
            self.send_header("Content-Type", "application/json")
            self.send_header("Content-Length", str(len(encoded)))
            self.end_headers()
            self.wfile.write(encoded)

        def log_message(self, _format, *_args):
            pass

    server = ThreadingHTTPServer(("127.0.0.1", 0), Handler)
    thread = threading.Thread(target=server.serve_forever, daemon=True)
    thread.start()
    try:
        yield f"http://127.0.0.1:{server.server_port}", calls
    finally:
        server.shutdown()
        server.server_close()
        thread.join(timeout=5)


def test_bridge_posts_authenticated_semantic_action(monkeypatch):
    envelope = {"ok": True, "result": {"active": True}, "revision": 7}
    with _fake_bridge(envelope) as (url, calls):
        monkeypatch.setenv(BRIDGE_URL_ENV, url)
        monkeypatch.setenv(BRIDGE_TOKEN_ENV, "test-only-bearer")
        assert junction.junction_start_session("peer-123") == envelope

    assert calls == [{
        "path": "/junction",
        "authorization": "Bearer test-only-bearer",
        "content_type": "application/json; charset=utf-8",
        "body": {
            "action": "session.start",
            "arguments": {"performerPeerId": "peer-123"},
        },
    }]


def test_bridge_returns_structured_error_envelope_unchanged(monkeypatch):
    envelope = {
        "ok": False,
        "error": {"code": "not_coordinator", "message": "host only", "retryable": False},
    }
    with _fake_bridge(envelope) as (url, _calls):
        monkeypatch.setenv(BRIDGE_URL_ENV, url)
        monkeypatch.setenv(BRIDGE_TOKEN_ENV, "token")
        assert junction.junction_end_session() == envelope


def test_prepare_engine_uses_dedicated_safe_action(monkeypatch):
    response = {"ok": True, "result": {"running": True, "reused": False}}
    with _fake_bridge(response) as (url, calls):
        monkeypatch.setenv(BRIDGE_URL_ENV, url)
        monkeypatch.setenv(BRIDGE_TOKEN_ENV, "token")
        assert junction.junction_prepare_engine() == response
    assert calls[0]["body"] == {"action": "prepare", "arguments": {}}


def test_tools_translate_typed_arguments(monkeypatch):
    monkeypatch.setattr(junction.time, "time", lambda: 1_700_000_000)
    with _fake_bridge() as (url, calls):
        monkeypatch.setenv(BRIDGE_URL_ENV, url)
        monkeypatch.setenv(BRIDGE_TOKEN_ENV, "token")
        junction.junction_configure_network(
            ["stun:stun.example.test"],
            save=False,
            turn_mode="temporary",
            turn_urls=["turn:relay.example.test"],
            turn_username="1700000060:dj",
            turn_credential="credential",
            turn_expires_at=1_700_000_060_000,
        )
        junction.junction_reject_participant("peer-456")
        junction.junction_attach_live_monitor("C", "a" * 64)
        junction.junction_set_microphone_enabled(True)
        junction.junction_configure_input(
            volume=0.75,
            assign="right",
            eq_low=0.8,
            eq_mid=1.0,
            eq_high=1.2,
            cue=True,
        )
        junction.junction_release_input()

        junction.junction_get_exchange_text("invite", "peer-789")

    assert [call["body"]["action"] for call in calls] == [
        "network.configure", "peer.reject", "live.attach", "mic.enabled",
        "input.set", "input.release",
        "exchange.export",
    ]
    assert calls[0]["body"]["arguments"] == {
        "stunUrls": ["stun:stun.example.test"],
        "save": False,
        "turn": {
            "mode": "temporary",
            "urls": ["turn:relay.example.test"],
            "username": "1700000060:dj",
            "credential": "credential",
            "expiresAt": 1_700_000_060_000,
        },
    }
    assert calls[1]["body"]["arguments"] == {"peerId": "peer-456"}
    assert calls[2]["body"]["arguments"] == {"deck": "C", "assetId": "a" * 64}
    assert calls[4]["body"]["arguments"] == {
        "volume": 0.75,
        "orientation": 2,
        "eqLow": 0.8,
        "eqMid": 1.0,
        "eqHigh": 1.2,
        "pfl": True,
    }
    assert calls[5]["body"]["arguments"] == {}
    assert calls[6]["body"]["arguments"] == {"kind": "invite", "peerId": "peer-789"}


def test_guest_response_exchange_export_may_omit_peer_id(monkeypatch):
    with _fake_bridge() as (url, calls):
        monkeypatch.setenv(BRIDGE_URL_ENV, url)
        monkeypatch.setenv(BRIDGE_TOKEN_ENV, "token")
        junction.junction_get_exchange_text("response")
    assert calls[0]["body"] == {
        "action": "exchange.export",
        "arguments": {"kind": "response"},
    }


def test_junction_input_requires_at_least_one_setting():
    with pytest.raises(ToolError, match="at least one"):
        junction.junction_configure_input()


@pytest.mark.parametrize(
    ("kwargs", "message"),
    [
        ({"stun_urls": ["stun:example.test"] * 9}, "at most 8"),
        ({
            "stun_urls": [], "turn_mode": "rest",
            "turn_urls": ["turn:example.test"] * 9, "turn_secret": "s" * 32,
        }, "1..8"),
        ({"stun_urls": ["stun:" + "界" * 340 + ".test"]}, "1024 UTF-8 bytes"),
        ({
            "stun_urls": [], "turn_mode": "rest",
            "turn_urls": ["turn:example.test"], "turn_secret": "s" * 31,
        }, "32..512 UTF-8 bytes"),
        ({
            "stun_urls": [], "turn_mode": "rest",
            "turn_urls": ["turn:example.test"], "turn_secret": "s" * 513,
        }, "32..512 UTF-8 bytes"),
        ({
            "stun_urls": [], "turn_mode": "temporary",
            "turn_urls": ["turn:example.test"], "turn_username": "u" * 257,
            "turn_credential": "credential", "turn_expires_at": 1_700_000_060_000,
        }, "256 UTF-8 bytes"),
        ({
            "stun_urls": [], "turn_mode": "temporary",
            "turn_urls": ["turn:example.test"], "turn_username": "1700000060:dj",
            "turn_credential": "c" * 1025, "turn_expires_at": 1_700_000_060_000,
        }, "1024 UTF-8 bytes"),
        ({
            "stun_urls": [], "turn_mode": "temporary",
            "turn_urls": ["turn:example.test"], "turn_username": "1700000060:dj",
            "turn_credential": "credential", "turn_expires_at": 1_700_086_400_001,
        }, "within 24 hours"),
        ({
            "stun_urls": [], "turn_mode": "temporary",
            "turn_urls": ["turn:example.test"], "turn_username": "not-an-expiry:dj",
            "turn_credential": "credential", "turn_expires_at": 1_700_000_060_000,
        }, "Unix expiry in seconds"),
    ],
)
def test_network_configuration_matches_native_bounds(monkeypatch, kwargs, message):
    monkeypatch.setattr(junction.time, "time", lambda: 1_700_000_000)
    with pytest.raises(ToolError, match=message):
        junction.junction_configure_network(**kwargs)


@pytest.mark.parametrize("missing", [BRIDGE_URL_ENV, BRIDGE_TOKEN_ENV])
def test_unavailable_bridge_is_a_safe_tool_error(monkeypatch, missing):
    monkeypatch.setenv(BRIDGE_URL_ENV, "http://127.0.0.1:9")
    monkeypatch.setenv(BRIDGE_TOKEN_ENV, "top-secret-token")
    monkeypatch.delenv(missing)
    with pytest.raises(ToolError, match="Junction bridge is unavailable") as caught:
        junction.junction_get_state()
    assert "top-secret-token" not in str(caught.value)


@pytest.mark.parametrize("url", [
    "http://192.168.3.5:1234",
    "https://127.0.0.1:1234",
    "http://" + "user:pass@" + "127.0.0.1:1234",
    "http://127.0.0.1:1234/unexpected",
])
def test_bridge_refuses_non_loopback_or_ambiguous_addresses(monkeypatch, url):
    monkeypatch.setenv(BRIDGE_URL_ENV, url)
    monkeypatch.setenv(BRIDGE_TOKEN_ENV, "token")
    with pytest.raises(JunctionBridgeError):
        post_junction_action("snapshot", {})


def test_http_error_redacts_bridge_and_exchange_secrets(monkeypatch):
    exchange = "PLUMDECK-JUNCTION-1.super-sensitive-packet"
    token = "super-sensitive-bearer"
    response = {"ok": False, "error": {"message": exchange, "token": token}}
    with _fake_bridge(response, status=401) as (url, _calls):
        monkeypatch.setenv(BRIDGE_URL_ENV, url)
        monkeypatch.setenv(BRIDGE_TOKEN_ENV, token)
        with pytest.raises(ToolError) as caught:
            junction.junction_import_exchange(exchange)
    rendered = str(caught.value)
    assert "HTTP 401" in rendered
    assert exchange not in rendered
    assert token not in rendered


def test_exchange_text_has_byte_bound_before_network(monkeypatch):
    monkeypatch.setenv(BRIDGE_URL_ENV, "http://127.0.0.1:9")
    monkeypatch.setenv(BRIDGE_TOKEN_ENV, "token")
    # Multibyte content proves the runtime byte cap is stricter than the schema's
    # character cap and is checked before attempting any bridge connection.
    too_large = "界" * (MAX_EXCHANGE_TEXT_BYTES // 3 + 1)
    with pytest.raises(ToolError, match="UTF-8 bytes"):
        junction.junction_inspect_exchange(too_large)


def test_junction_tool_schemas_are_specific_and_bounded():
    from mcp_server.instance import mcp
    import mcp_server.server  # noqa: F401 - registers every tool

    tools = {tool.name: tool for tool in mcp._tool_manager.list_tools()}
    assert len([name for name in tools if name.startswith("junction_")]) == 41
    exchange = tools["junction_inspect_exchange"].parameters["properties"]["exchange_text"]
    assert exchange["type"] == "string"
    assert exchange["minLength"] == 1
    assert exchange["maxLength"] == MAX_EXCHANGE_TEXT_BYTES
    deck = tools["junction_attach_live_monitor"].parameters["properties"]["deck"]
    assert deck == {"enum": ["A", "B", "C", "D"], "title": "Deck", "type": "string"}
    asset = tools["junction_attach_live_monitor"].parameters["properties"]["asset_id"]
    assert asset["pattern"] == "^[0-9a-f]{64}$"
    export = tools["junction_get_exchange_text"].parameters
    assert export["properties"]["kind"]["enum"] == ["invite", "response", "notice"]
    assert export["required"] == ["kind"]
    expires = tools["junction_configure_network"].parameters["properties"]["turn_expires_at"]
    assert "Unix epoch milliseconds" in expires["anyOf"][0]["description"]
    input_properties = tools["junction_configure_input"].parameters["properties"]
    assert input_properties["volume"]["anyOf"][0]["minimum"] == 0.0
    assert input_properties["volume"]["anyOf"][0]["maximum"] == 1.0
    assert input_properties["eq_low"]["anyOf"][0]["minimum"] == 0.0
    assert input_properties["eq_low"]["anyOf"][0]["maximum"] == 4.0
    assert input_properties["assign"]["anyOf"][0]["enum"] == ["left", "thru", "right"]
    assert tools["junction_prepare_engine"].parameters.get("properties") == {}
    assert "action" not in tools["junction_get_state"].parameters.get("properties", {})
