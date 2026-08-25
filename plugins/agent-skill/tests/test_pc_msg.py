#!/usr/bin/env python3
"""Unit tests for the pc_msg skill script (stdlib unittest, no network).

Run:  python3 -m unittest discover -s plugins/agent-skill/tests
  or: python3 plugins/agent-skill/tests/test_pc_msg.py
"""

import io
import json
import os
import sys
import tempfile
import unittest
from pathlib import Path

sys.path.insert(0, str(Path(__file__).resolve().parent.parent))

import pc_msg
from pc_msg import (
    ConnectBackend,
    PcBackend,
    PcMsgError,
    RpcClient,
    build_handoff,
    find_pc,
    find_pc_connect,
    load_sessions,
    message_text,
    normalize_event,
    resolve_by_session,
    resolve_session,
    resolve_text_args,
)


# ---------------------------------------------------------------------------
# Fakes
# ---------------------------------------------------------------------------


class FakeResult:
    def __init__(self, returncode=0, stdout=b"", stderr=b""):
        self.returncode = returncode
        self.stdout = stdout
        self.stderr = stderr


class FakeStdout:
    """Iterable of lines, plus readline for RpcClient's reader thread."""

    def __init__(self, lines):
        self._lines = list(lines)
        self._i = 0
        self.closed = False

    def __iter__(self):
        return self

    def __next__(self):
        if self._i >= len(self._lines):
            raise StopIteration
        line = self._lines[self._i]
        self._i += 1
        return line

    def readline(self):
        try:
            return next(self)
        except StopIteration:
            return ""


class FakeStdin:
    def __init__(self):
        self.written = []
        self.closed = False

    def write(self, s):
        if isinstance(s, bytes):
            s = s.decode("utf-8")
        self.written.append(s)
        return len(s)

    def flush(self):
        pass

    def close(self):
        self.closed = True


class FakeProc:
    """Popen stand-in: records argv/env, serves canned stdout, captures stdin."""

    def __init__(self, argv, stdout_lines=(), returncode=0, env=None, stdin=None, stdout=None, stderr=None):
        self.argv = list(argv)
        self.env = env
        self.stdin = stdin if stdin is not None else FakeStdin()
        self.stdout = stdout if stdout is not None else FakeStdout(stdout_lines)
        self.stderr = stderr
        self.returncode = returncode
        self._terminated = False
        self._killed = False
        self._waited = False

    def poll(self):
        if self._terminated or self._killed:
            return -9
        if isinstance(self.stdout, FakeStdout) and self.stdout._i >= len(self.stdout._lines):
            return self.returncode
        return None

    def wait(self, timeout=None):
        self._waited = True
        return self.returncode

    def terminate(self):
        self._terminated = True

    def kill(self):
        self._killed = True


def jsonrpc_lines(frames):
    """Render response/notification frames as newline-delimited JSON."""
    return [json.dumps(f) + "\n" for f in frames]


# ---------------------------------------------------------------------------
# normalize_event / message_text
# ---------------------------------------------------------------------------


class TestNormalizeEvent(unittest.TestCase):
    def test_raw_notification_message(self):
        msg = {"id": "m1", "channel": "telegram", "channel_id": "42",
               "content": [{"Text": "hello"}], "ts": 1}
        line = json.dumps({"jsonrpc": "2.0", "method": "event.message",
                           "params": {"message": msg}})
        event, payload = normalize_event(line)
        self.assertEqual(event, "event.message")
        self.assertEqual(payload["message"]["channel_id"], "42")

    def test_flat_event_line(self):
        line = json.dumps({"event": "event.message", "message": {"channel_id": "9"}})
        event, payload = normalize_event(line)
        self.assertEqual(event, "event.message")
        self.assertEqual(payload["message"]["channel_id"], "9")

    def test_error_notification_normalized_to_error_key(self):
        line = json.dumps({"jsonrpc": "2.0", "method": "event.error",
                           "params": {"provider": "telegram", "code": -32005,
                                      "message": "boom", "data": None}})
        event, payload = normalize_event(line)
        self.assertEqual(event, "event.error")
        self.assertEqual(payload["error"]["code"], -32005)

    def test_flat_error_line(self):
        line = json.dumps({"event": "event.error", "error": {"code": -32005}})
        event, payload = normalize_event(line)
        self.assertEqual(event, "event.error")
        self.assertEqual(payload["error"]["code"], -32005)

    def test_response_line_is_not_an_event(self):
        line = json.dumps({"jsonrpc": "2.0", "id": 1, "result": {"providers": ["demo"]}})
        self.assertIsNone(normalize_event(line))

    def test_garbage_returns_none(self):
        self.assertIsNone(normalize_event("not json at all"))
        self.assertIsNone(normalize_event(""))
        self.assertIsNone(normalize_event("42"))

    def test_draft_and_choice_pass_through(self):
        line = json.dumps({"jsonrpc": "2.0", "method": "event.draft",
                           "params": {"channel": "telegram", "content": "x"}})
        event, payload = normalize_event(line)
        self.assertEqual(event, "event.draft")
        self.assertEqual(payload["content"], "x")


class TestMessageText(unittest.TestCase):
    def test_serde_text_parts(self):
        msg = {"content": [{"Text": "a"}, {"Media": {"kind": "Image"}}, {"Text": "b"}]}
        self.assertEqual(message_text(msg), "a b")

    def test_plain_string_content(self):
        self.assertEqual(message_text({"content": "hi"}), "hi")

    def test_empty(self):
        self.assertEqual(message_text({}), "")
        self.assertEqual(message_text(None), "")


# ---------------------------------------------------------------------------
# Session resolution
# ---------------------------------------------------------------------------


CONFIG = {
    "sessions": [
        {"id": "a", "provider": "telegram", "chat": "111", "agent": "opencode", "session": "sess-a"},
        {"id": "b", "provider": "discord", "chat": "222", "agent": "prime", "session": "agent-b"},
        {"id": "c", "chat": "333", "agent": "opencode", "session": "sess-c", "chats": ["333", "334"]},
    ]
}


def write_config(data):
    tmp = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
    json.dump(data, tmp)
    tmp.close()
    return tmp.name


class TestSessionResolution(unittest.TestCase):
    def setUp(self):
        self.path = write_config(CONFIG)

    def tearDown(self):
        os.unlink(self.path)

    def test_resolve_by_chat_and_provider(self):
        entry = resolve_session("111", "telegram", self.path)
        self.assertEqual(entry["session"], "sess-a")

    def test_provider_mismatch_excluded(self):
        entry = resolve_session("111", "discord", self.path)
        self.assertIsNone(entry)

    def test_entry_without_provider_matches_any(self):
        self.assertEqual(resolve_session("333", "telegram", self.path)["session"], "sess-c")
        self.assertEqual(resolve_session("334", "discord", self.path)["session"], "sess-c")

    def test_unknown_chat(self):
        self.assertIsNone(resolve_session("999", None, self.path))

    def test_resolve_by_session_id_or_label(self):
        self.assertEqual(resolve_by_session("sess-a", self.path)["id"], "a")
        self.assertEqual(resolve_by_session("b", self.path)["id"], "b")
        self.assertIsNone(resolve_by_session("nope", self.path))

    def test_missing_config_returns_empty(self):
        self.assertIsNone(resolve_session("111", None, "/nonexistent.json"))

    def test_malformed_config_raises(self):
        bad = tempfile.NamedTemporaryFile("w", suffix=".json", delete=False)
        bad.write("{not json")
        bad.close()
        try:
            with self.assertRaises(PcMsgError):
                load_sessions(bad.name)
        finally:
            os.unlink(bad.name)


# ---------------------------------------------------------------------------
# Handoff construction
# ---------------------------------------------------------------------------


class TestBuildHandoff(unittest.TestCase):
    def test_opencode_default_with_project(self):
        entry = {"agent": "opencode", "session": "s1", "project": "/repo"}
        self.assertEqual(
            build_handoff(entry, "hi there"),
            ["opencode", "run", "--session", "s1", "--dir", "/repo", "hi there"],
        )

    def test_opencode_without_project(self):
        entry = {"agent": "opencode", "session": "s1"}
        self.assertEqual(build_handoff(entry, "hi"), ["opencode", "run", "--session", "s1", "hi"])

    def test_prime_default(self):
        entry = {"agent": "prime", "session": "agent-b"}
        self.assertEqual(
            build_handoff(entry, "msg"), ["prime-agent", "send", "agent-b", "msg"]
        )

    def test_custom_handoff_template(self):
        entry = {
            "agent": "opencode",
            "session": "s1",
            "chat": "111",
            "provider": "telegram",
            "handoff": ["tool", "x", "{chat}", "{provider}", "{session}", "{text}"],
        }
        self.assertEqual(
            build_handoff(entry, "hello"),
            ["tool", "x", "111", "telegram", "s1", "hello"],
        )

    def test_custom_handoff_string_form(self):
        entry = {"agent": "opencode", "session": "s1", "handoff": "mycmd --session {session} {text}"}
        self.assertEqual(build_handoff(entry, "a b"), ["mycmd", "--session", "s1", "a b"])

    def test_prime_missing_session_raises(self):
        with self.assertRaises(PcMsgError):
            build_handoff({"agent": "prime"}, "x")

    def test_unknown_agent_raises(self):
        with self.assertRaises(PcMsgError):
            build_handoff({"agent": "wat", "session": "s"}, "x")


# ---------------------------------------------------------------------------
# resolve_text_args
# ---------------------------------------------------------------------------


class TestResolveTextArgs(unittest.TestCase):
    def test_text_wins(self):
        self.assertEqual(resolve_text_args("hi", None), ("hi", None))

    def test_both_raises(self):
        with self.assertRaises(PcMsgError):
            resolve_text_args("hi", "f.txt")

    def test_stdin_marker_passthrough(self):
        self.assertEqual(resolve_text_args(None, "-"), (None, "-"))

    def test_file_read(self):
        tmp = tempfile.NamedTemporaryFile("w", delete=False)
        tmp.write("from file")
        tmp.close()
        try:
            self.assertEqual(resolve_text_args(None, tmp.name), ("from file", None))
        finally:
            os.unlink(tmp.name)

    def test_neither_raises(self):
        with self.assertRaises(PcMsgError):
            resolve_text_args(None, None)


# ---------------------------------------------------------------------------
# Backend subprocess flows (mocked)
# ---------------------------------------------------------------------------


class TestConnectBackend(unittest.TestCase):
    def test_send_argv_and_receipt(self):
        calls = []

        def fake_run(argv, **kw):
            calls.append(argv)
            return FakeResult(0, b'{"message_id": "m1", "ts": 1}\n')

        backend = ConnectBackend("/bin/pc-connect", run=fake_run)
        out = io.StringIO()
        old = sys.stdout
        sys.stdout = out
        try:
            rc = backend.send("telegram", "42", "hello", None)
        finally:
            sys.stdout = old
        self.assertEqual(rc, 0)
        self.assertEqual(
            calls[0],
            ["/bin/pc-connect", "send", "--provider", "telegram", "--chat", "42", "--text", "hello"],
        )
        self.assertEqual(out.getvalue().strip(), '{"message_id": "m1", "ts": 1}')

    def test_send_text_file_passthrough_flag(self):
        calls = []

        def fake_run(argv, **kw):
            calls.append(argv)
            return FakeResult(0, b'{"message_id": "m2"}\n')

        backend = ConnectBackend("pc-connect", run=fake_run)
        out = io.StringIO()
        old = sys.stdout
        sys.stdout = out
        try:
            backend.send("demo", "room", None, "-")
        finally:
            sys.stdout = old
        self.assertEqual(calls[0][-2:], ["--text-file", "-"])

    def test_send_failure_raises(self):
        backend = ConnectBackend("pc-connect", run=lambda argv, **kw: FakeResult(1, b"", b"nope"))
        with self.assertRaises(PcMsgError):
            backend.send("telegram", "42", "x", None)

    def test_check_passthrough(self):
        backend = ConnectBackend("pc-connect", run=lambda argv, **kw: FakeResult(0))
        self.assertEqual(backend.check("telegram"), 0)
        backend2 = ConnectBackend("pc-connect", run=lambda argv, **kw: FakeResult(1))
        self.assertEqual(backend2.check("telegram"), 1)

    def test_listen_yields_normalized_events_and_stops(self):
        lines = [
            json.dumps({"jsonrpc": "2.0", "method": "event.message",
                        "params": {"message": {"channel_id": "42"}}}) + "\n",
            json.dumps({"event": "event.error", "error": {"code": -32005}}) + "\n",
        ]
        proc = FakeProc([], stdout_lines=lines)
        backend = ConnectBackend("pc-connect", popen=lambda argv, **kw: proc)
        events = list(backend.listen(providers=["demo"], timeout=5, once=True))
        self.assertEqual(len(events), 2)
        self.assertEqual(events[0][0], "event.message")
        self.assertEqual(events[1][0], "event.error")


class TestPcBackend(unittest.TestCase):
    def test_send_jsonrpc_flow(self):
        frames = [
            {"jsonrpc": "2.0", "id": 1, "result": {"providers": ["demo"]}},
            {"jsonrpc": "2.0", "id": 2, "result": {"started": ["demo"]}},
            {"jsonrpc": "2.0", "id": 3, "result": {"message_id": "m9", "ts": 5}},
            {"jsonrpc": "2.0", "id": 4, "result": None},
        ]
        proc = FakeProc([], stdout_lines=jsonrpc_lines(frames))
        backend = PcBackend("/bin/pc", popen=lambda argv, **kw: proc)
        out = io.StringIO()
        old = sys.stdout
        sys.stdout = out
        try:
            rc = backend.send("demo", "room", "hi", None)
        finally:
            sys.stdout = old
        self.assertEqual(rc, 0)
        written = "".join(proc.stdin.written)
        self.assertIn('"method": "initialize"', written)
        self.assertIn('"method": "send"', written)
        self.assertIn('"channel_id": "room"', written)
        self.assertIn('"text": "hi"', written)
        self.assertIn('"method": "shutdown"', written)
        self.assertEqual(json.loads(out.getvalue())["message_id"], "m9")

    def test_send_error_response_raises(self):
        frames = [
            {"jsonrpc": "2.0", "id": 1, "result": {}},
            {"jsonrpc": "2.0", "id": 2, "result": {"started": ["telegram"]}},
            {"jsonrpc": "2.0", "id": 3, "error": {"code": -32005, "message": "net down"}},
        ]
        proc = FakeProc([], stdout_lines=jsonrpc_lines(frames))
        backend = PcBackend("/bin/pc", popen=lambda argv, **kw: proc)
        with self.assertRaises(PcMsgError):
            backend.send("telegram", "42", "x", None)

    def test_listen_yields_events_and_shuts_down_on_once(self):
        frames = [
            {"jsonrpc": "2.0", "id": 1, "result": {}},
            {"jsonrpc": "2.0", "id": 2, "result": {"started": ["demo"]}},
            {"jsonrpc": "2.0", "method": "event.message",
             "params": {"message": {"channel_id": "demo-room", "channel": "demo"}}},
            {"jsonrpc": "2.0", "id": 3, "result": None},
        ]
        proc = FakeProc([], stdout_lines=jsonrpc_lines(frames))
        backend = PcBackend("/bin/pc", popen=lambda argv, **kw: proc)
        events = []
        for ev in backend.listen(providers=["demo"], timeout=10, once=True):
            events.append(ev)
            backend.stop()
            break
        self.assertEqual(len(events), 1)
        self.assertEqual(events[0][0], "event.message")
        self.assertTrue(proc.stdin.closed)
        written = "".join(proc.stdin.written)
        self.assertIn('"providers": ["demo"]', written)
        self.assertIn('"method": "listen"', written)

    def test_check_available(self):
        frames = [{"jsonrpc": "2.0", "id": 1, "result": {"providers": ["demo", "telegram"]}}]
        proc = FakeProc([], stdout_lines=jsonrpc_lines(frames))
        backend = PcBackend("/bin/pc", popen=lambda argv, **kw: proc)
        self.assertEqual(backend.check("telegram"), 0)

    def test_check_unavailable(self):
        frames = [{"jsonrpc": "2.0", "id": 1, "result": {"providers": ["demo"]}}]
        proc = FakeProc([], stdout_lines=jsonrpc_lines(frames))
        backend = PcBackend("/bin/pc", popen=lambda argv, **kw: proc)
        self.assertEqual(backend.check("telegram"), 1)

    def test_check_no_providers_any_fails(self):
        frames = [{"jsonrpc": "2.0", "id": 1, "result": {"providers": []}}]
        proc = FakeProc([], stdout_lines=jsonrpc_lines(frames))
        backend = PcBackend("/bin/pc", popen=lambda argv, **kw: proc)
        self.assertEqual(backend.check(), 1)


class TestRpcClient(unittest.TestCase):
    def test_request_response_matching(self):
        frames = [
            {"jsonrpc": "2.0", "id": 1, "result": "ok"},
            {"jsonrpc": "2.0", "method": "event.message", "params": {"message": {}}},
        ]
        proc = FakeProc([], stdout_lines=jsonrpc_lines(frames))
        client = RpcClient(proc)
        self.assertEqual(client.request("initialize"), "ok")
        ev = client.next_event(timeout=2)
        self.assertEqual(ev["method"], "event.message")
        self.assertIsNone(client.next_event(timeout=2))  # EOF sentinel

    def test_request_timeout(self):
        proc = FakeProc([], stdout_lines=[])
        client = RpcClient(proc)
        with self.assertRaises(PcMsgError):
            client.request("initialize", timeout=0.2)


# ---------------------------------------------------------------------------
# Binary discovery
# ---------------------------------------------------------------------------


class TestDiscovery(unittest.TestCase):
    def test_pc_connect_env_override(self):
        old = os.environ.get("PC_CONNECT_BIN")
        os.environ["PC_CONNECT_BIN"] = "/fake/pc-connect"
        try:
            self.assertEqual(find_pc_connect(), "/fake/pc-connect")
        finally:
            if old is None:
                del os.environ["PC_CONNECT_BIN"]
            else:
                os.environ["PC_CONNECT_BIN"] = old

    def test_pc_env_override(self):
        old = os.environ.get("PC_BIN")
        os.environ["PC_BIN"] = "/fake/pc"
        try:
            self.assertEqual(find_pc(), "/fake/pc")
        finally:
            if old is None:
                del os.environ["PC_BIN"]
            else:
                os.environ["PC_BIN"] = old


if __name__ == "__main__":
    unittest.main()
