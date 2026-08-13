"""Unit tests for pc_connect: JSON parsing + session routing with mocked subprocess.

Everything here is stdlib-only (unittest). No `pc` binary and no
`prime-agent` are required: subprocess.Popen is replaced by a scripted fake
that serves canned JSON-RPC frames and records what the client writes.
"""

import io
import json
import os
import sys
import tempfile
import time
import unittest
from unittest import mock

sys.path.insert(0, os.path.dirname(os.path.dirname(os.path.abspath(__file__))))
import pc_connect
from pc_connect import (ConnectCli, PcClient, PcError, bridge, build_prompt,
                        dispatch_to_agent, find_connect_binary, message_text,
                        parse_event_message, session_file_for)


# ---------------------------------------------------------------------------
# Fakes
# ---------------------------------------------------------------------------

class CapturedStdin:
    """Text-mode stdin stand-in that keeps everything the client wrote."""

    def __init__(self):
        self._chunks = []

    def write(self, chunk):
        self._chunks.append(chunk)
        return len(chunk)

    def flush(self):
        pass

    def close(self):
        pass

    def getvalue(self):
        return "".join(self._chunks)


class FakeProc:
    """Minimal Popen stand-in: scripted stdout + captured stdin + stubs."""

    def __init__(self, stdout_lines, exit_code=0):
        self.stdout = io.StringIO("".join(line + "\n" for line in stdout_lines))
        self.stdin = CapturedStdin()
        self.exit_code = exit_code
        self.returncode = exit_code
        self.killed = False
        self.closed = False

    def wait(self, timeout=None):
        self.closed = True
        return self.exit_code

    def communicate(self, timeout=None):
        self.closed = True
        return (self.stdout.getvalue(), "")

    def kill(self):
        self.killed = True

    def terminate(self):
        self.killed = True

    @property
    def written(self):
        return self.stdin.getvalue()


class BlockingStdout:
    """Fake stdout that blocks on readline until released (for timeout tests)."""

    def __init__(self):
        self._lines = []
        self._closed = False

    def write(self, line):
        self._lines.append(line)

    def __iter__(self):
        return self

    def __next__(self):
        while True:
            if self._lines:
                return self._lines.pop(0)
            if self._closed:
                raise StopIteration
            time.sleep(0.005)

    def close(self):
        self._closed = True


class ScriptedPopen:
    """Popen factory keyed on argv: each matching script serves canned lines.

    scripts: list of (match_fn(cmd) -> bool, [stdout lines...]). The first
    matching script wins; its FakeProc is recorded in `calls` for assertions.
    """

    def __init__(self, scripts):
        # script entries: (match_fn, stdout_lines, exit_code=0)
        self.scripts = scripts
        self.calls = []  # (cmd, FakeProc)

    def __call__(self, cmd, **kwargs):
        for entry in self.scripts:
            match = entry[0]
            lines = entry[1]
            exit_code = entry[2] if len(entry) > 2 else 0
            if match(cmd):
                proc = FakeProc(lines, exit_code=exit_code)
                self.calls.append((list(cmd), proc))
                return proc
        raise AssertionError("unexpected Popen call: {}".format(cmd))


def frame_response(req_id, result):
    return json.dumps({"jsonrpc": "2.0", "id": req_id, "result": result})


def frame_error(req_id, code, message):
    return json.dumps({"jsonrpc": "2.0", "id": req_id,
                       "error": {"code": code, "message": message}})


def frame_notification(method, params):
    return json.dumps({"jsonrpc": "2.0", "method": method, "params": params})


class ScriptBuilder:
    """Builds canned sidecar frames with request-order-correct response ids."""

    def __init__(self):
        self.lines = []
        self.next_id = 1

    def init(self, result):
        self.lines.append(frame_response(self.next_id, result))
        self.next_id += 1
        return self

    def listen(self, result):
        self.lines.append(frame_response(self.next_id, result))
        self.next_id += 1
        return self

    def send(self, result):
        self.lines.append(frame_response(self.next_id, result))
        self.next_id += 1
        return self

    def notify(self, method, params):
        self.lines.append(frame_notification(method, params))
        return self

    def error(self, code, message, extra=None):
        err = {"code": code, "message": message}
        if extra:
            err["data"] = extra
        self.lines.append(frame_error(self.next_id, code, message))
        self.next_id += 1
        return self

    def build(self):
        return list(self.lines)


# ---------------------------------------------------------------------------
# Parsing helpers
# ---------------------------------------------------------------------------

def sample_message(overrides=None):
    msg = {
        "id": "m1",
        "channel": "telegram",
        "channel_id": "-100123",
        "sender": {"id": "u1", "name": "Alice", "username": "alice"},
        "reply_target": "-100123",
        "content": [{"Text": "hello"}, {"Text": "world"}],
        "thread_ts": None,
        "attachments": [],
        "explicitly_addressed": True,
        "ts": 1700000000000,
        "raw": None,
    }
    if overrides:
        msg.update(overrides)
    return msg


class TestParsing(unittest.TestCase):
    def test_parse_event_message_unwraps(self):
        notif = frame_notification("event.message", {"message": sample_message()})
        parsed = parse_event_message(json.loads(notif))
        self.assertEqual(parsed["id"], "m1")
        self.assertEqual(parsed["channel_id"], "-100123")

    def test_parse_event_message_malformed(self):
        bad = json.loads(frame_notification("event.message", {"nope": 1}))
        with self.assertRaises(PcError):
            parse_event_message(bad)

    def test_message_text_concatenates_parts(self):
        self.assertEqual(message_text(sample_message()), "hello\nworld")

    def test_message_text_media_marker(self):
        msg = sample_message({"content": [
            {"Text": "look"},
            {"Media": {"kind": "Image", "mime": "image/png", "caption": "diagram"}},
        ]})
        self.assertEqual(message_text(msg), "look\n[image] diagram")

    def test_build_prompt_includes_context(self):
        prompt = build_prompt(sample_message())
        self.assertIn("telegram", prompt)
        self.assertIn("-100123", prompt)
        self.assertIn("Alice", prompt)
        self.assertIn("hello\nworld", prompt)

    def test_build_prompt_template(self):
        prompt = build_prompt(sample_message(), template="{sender_name}: {text}")
        self.assertEqual(prompt, "Alice: hello\nworld")


class TestSessionRouting(unittest.TestCase):
    def test_session_file_is_stable_per_chat(self):
        a = session_file_for("telegram", "-100123", "/tmp/sd")
        b = session_file_for("telegram", "-100123", "/tmp/sd")
        self.assertEqual(a, b)
        self.assertTrue(a.endswith("pc-telegram--100123.jsonl"), a)

    def test_session_file_differs_per_chat_and_provider(self):
        a = session_file_for("telegram", "-100123", "/tmp/sd")
        b = session_file_for("telegram", "-100456", "/tmp/sd")
        c = session_file_for("discord", "-100123", "/tmp/sd")
        self.assertNotEqual(a, b)
        self.assertNotEqual(a, c)

    def test_session_file_sanitizes_weird_ids(self):
        f = session_file_for("telegram", "a/b c:d?", "/tmp/sd")
        self.assertTrue(f.endswith("pc-telegram-a_b_c_d.jsonl"), f)

    def test_session_file_default_dir(self):
        f = session_file_for("telegram", "1")
        self.assertTrue(f.startswith(os.path.expanduser("~/.prime/agent/sessions")))


# ---------------------------------------------------------------------------
# PcClient against scripted sidecar
# ---------------------------------------------------------------------------

class TestPcClient(unittest.TestCase):
    def test_check_returns_capabilities(self):
        caps = {"protocolVersion": "0.1.0", "providers": ["demo", "telegram"],
                "methods": ["initialize"], "features": ["send"]}
        # initialize (id 1) + capabilities (id 2) return the same shape.
        script = ScriptBuilder().init(caps).build()
        script.append(frame_response(2, caps))
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), script)])
        with PcClient(pc_bin="/fake/pc", popen=fake) as client:
            result = client.check()
        self.assertEqual(result, caps)
        cmd, proc = fake.calls[0]
        self.assertEqual(cmd, ["/fake/pc"])
        frames = [json.loads(l) for l in proc.written.splitlines()]
        self.assertEqual([f["method"] for f in frames], ["initialize", "capabilities", "shutdown"])
        self.assertEqual(frames[0]["id"], 1)
        self.assertEqual(frames[1]["id"], 2)

    def test_send_wires_params_and_receipt(self):
        receipt = {"message_id": "demo-1", "ts": 1700000000001}
        script = ScriptBuilder().init({}).send(receipt).build()
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), script)])
        with PcClient(pc_bin="/fake/pc", popen=fake) as client:
            result = client.send("telegram", "c1", "hi there", reply_to="m0")
        self.assertEqual(result, receipt)
        cmd, proc = fake.calls[0]
        frames = [json.loads(l) for l in proc.written.splitlines()]
        self.assertEqual([f["method"] for f in frames], ["initialize", "send", "shutdown"])
        self.assertEqual(frames[1]["params"], {
            "provider": "telegram",
            "message": {"channel_id": "c1", "text": "hi there", "reply_to": "m0"},
        })

    def test_send_auto_starts_provider_on_not_started(self):
        # First send attempt -> -32004 "provider not started"; client must
        # call listen (start the provider) and retry the send.
        lines = (ScriptBuilder().init({})
                 .error(-32004, "provider not started: telegram (call listen first)")
                 .listen({"started": ["telegram"]})
                 .send({"message_id": "demo-9", "ts": 9}).build())
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), lines)])
        with PcClient(pc_bin="/fake/pc", popen=fake) as client:
            receipt = client.send("telegram", "c1", "hi")
        self.assertEqual(receipt["message_id"], "demo-9")
        frames = [json.loads(l) for l in fake.calls[0][1].written.splitlines()]
        methods = [f["method"] for f in frames]
        self.assertEqual(methods, ["initialize", "send", "listen", "send", "shutdown"])
        self.assertEqual(frames[1]["params"]["provider"], "telegram")
        self.assertEqual(frames[3]["params"]["message"]["text"], "hi")

    def test_send_omits_reply_to_when_absent(self):
        script = ScriptBuilder().init({}).send({}).build()
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), script)])
        with PcClient(pc_bin="/fake/pc", popen=fake) as client:
            client.send("demo", "c1", "hi")
        frames = [json.loads(l) for l in fake.calls[0][1].written.splitlines()]
        self.assertNotIn("reply_to", frames[1]["params"]["message"])

    def test_send_error_response_raises_with_code(self):
        script = ScriptBuilder().init({}).error(-32004, "unknown provider").build()
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), script)])
        client = PcClient(pc_bin="/fake/pc", popen=fake)
        try:
            with self.assertRaises(PcError) as ctx:
                client.send("nope", "c1", "hi")
            self.assertEqual(ctx.exception.code, -32004)
            self.assertIn("unknown provider", str(ctx.exception))
        finally:
            client.close(shutdown=False)

    def test_listen_collects_messages_until_timeout(self):
        msg = sample_message()
        lines = (ScriptBuilder().init({}).listen({"started": ["telegram"]})
                 .notify("event.message", {"message": msg}).build())
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), lines)])
        with PcClient(pc_bin="/fake/pc", popen=fake) as client:
            result = client.listen(providers=["telegram"], timeout_secs=1.0)
        self.assertEqual(result["started"], ["telegram"])
        self.assertEqual(len(result["messages"]), 1)
        self.assertEqual(result["messages"][0]["id"], "m1")
        frames = [json.loads(l) for l in fake.calls[0][1].written.splitlines()]
        self.assertEqual(frames[1]["params"], {"providers": ["telegram"]})

    def test_listen_once_stops_after_first_message(self):
        msg = sample_message()
        lines = (ScriptBuilder().init({}).listen({"started": ["demo"]})
                 .notify("event.message", {"message": msg}).build())
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), lines)])
        with PcClient(pc_bin="/fake/pc", popen=fake) as client:
            result = client.listen(timeout_secs=5.0, once=True)
        self.assertEqual(len(result["messages"]), 1)

    def test_listen_captures_event_errors(self):
        lines = (ScriptBuilder().init({}).listen({"started": ["telegram"]})
                 .notify("event.error",
                         {"provider": "telegram", "code": -32005,
                          "message": "network down"}).build())
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), lines)])
        with PcClient(pc_bin="/fake/pc", popen=fake) as client:
            result = client.listen(timeout_secs=0.5)
        self.assertEqual(len(result["errors"]), 1)
        self.assertEqual(result["errors"][0]["code"], -32005)

    def test_request_timeout(self):
        # Sidecar never answers and stays alive: request must time out.
        blocking = BlockingStdout()

        class BlockingProc(FakeProc):
            def __init__(self):
                self.stdout = blocking
                self.stdin = CapturedStdin()
                self.exit_code = 0
                self.killed = False
                self.closed = False

        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), None)])  # never matches
        # Popen factory that always returns the blocking proc
        def factory(cmd, **kwargs):
            return BlockingProc()

        client = PcClient(pc_bin="/fake/pc", popen=factory, timeout=0.2)
        try:
            with self.assertRaises(PcError) as ctx:
                client.check()
            self.assertIn("timeout", str(ctx.exception))
        finally:
            blocking.close()
            client.close(shutdown=False)

    def test_process_exit_before_response(self):
        # EOF on stdout with no response -> "exited before responding".
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc"), [])])
        client = PcClient(pc_bin="/fake/pc", popen=fake, timeout=5.0)
        try:
            with self.assertRaises(PcError) as ctx:
                client.request("listen")
            self.assertIn("exited", str(ctx.exception))
        finally:
            client.close(shutdown=False)


# ---------------------------------------------------------------------------
# Agent dispatch + bridge
# ---------------------------------------------------------------------------

AGENT_END = json.dumps({
    "type": "agent_end",
    "messages": [
        {"role": "user", "content": "Incoming message..."},
        {"role": "assistant",
         "content": [{"type": "text", "text": "Reply line 1"},
                     {"type": "thinking", "thinking": "..."},
                     {"type": "text", "text": "Reply line 2"}]},
    ],
})


class TestDispatch(unittest.TestCase):
    def test_dispatch_sends_prompt_and_returns_assistant_text(self):
        lines = [json.dumps({"type": "response", "command": "prompt", "success": True}),
                 AGENT_END]
        fake = ScriptedPopen([(lambda c: c[0].endswith("prime-agent"), lines)])
        reply = dispatch_to_agent("hello", "/tmp/sd/pc-telegram-c1.jsonl",
                                  cwd="/work", popen=fake, timeout=10.0)
        self.assertEqual(reply, "Reply line 1\nReply line 2")
        cmd, proc = fake.calls[0]
        self.assertEqual(cmd, ["prime-agent", "--mode", "rpc", "--resume",
                               "/tmp/sd/pc-telegram-c1.jsonl", "--cwd", "/work"])
        frame = json.loads(proc.written.splitlines()[0])
        self.assertEqual(frame["type"], "prompt")
        self.assertEqual(frame["message"], "hello")
        self.assertEqual(frame["id"], "pc-connect")

    def test_dispatch_rejected_prompt_raises(self):
        lines = [json.dumps({"type": "response", "command": "prompt", "success": False,
                             "error": "model not configured"})]
        fake = ScriptedPopen([(lambda c: c[0].endswith("prime-agent"), lines)])
        with self.assertRaises(PcError) as ctx:
            dispatch_to_agent("hello", "/tmp/sd/s.jsonl", popen=fake, timeout=10.0)
        self.assertIn("rejected", str(ctx.exception))

    def test_dispatch_no_agent_end_raises(self):
        lines = [json.dumps({"type": "response", "command": "prompt", "success": True})]
        fake = ScriptedPopen([(lambda c: c[0].endswith("prime-agent"), lines)])
        with self.assertRaises(PcError) as ctx:
            dispatch_to_agent("hello", "/tmp/sd/s.jsonl", popen=fake, timeout=10.0)
        self.assertIn("agent_end", str(ctx.exception))


class TestBridge(unittest.TestCase):
    def test_bridge_routes_to_per_chat_session_and_replies(self):
        inbound = sample_message()
        lines = (ScriptBuilder().init({}).listen({"started": ["telegram"]})
                 .notify("event.message", {"message": inbound})
                 .send({"message_id": "demo-2", "ts": 1}).build())
        fake = ScriptedPopen([
            (lambda c: c[0].endswith("pc"), lines),
            (lambda c: c[0].endswith("prime-agent"),
             [json.dumps({"type": "response", "command": "prompt", "success": True}),
              AGENT_END]),
        ])
        result = bridge(provider="telegram", session_dir="/tmp/sd", popen=fake,
                        listen_timeout_secs=5.0, once=True)
        self.assertEqual(len(result["replies"]), 1)
        entry = result["replies"][0]
        self.assertEqual(entry["message"], "m1")
        self.assertTrue(entry["session"].endswith("pc-telegram--100123.jsonl"), entry["session"])
        self.assertEqual(entry["reply"], "Reply line 1\nReply line 2")
        self.assertEqual(entry["receipt"]["message_id"], "demo-2")

        # pc saw: initialize, listen, send with reply_to = inbound id
        pc_cmd, pc_proc = fake.calls[0]
        pc_frames = [json.loads(l) for l in pc_proc.written.splitlines()]
        self.assertEqual([f["method"] for f in pc_frames],
                         ["initialize", "listen", "send", "shutdown"])
        send_params = pc_frames[2]["params"]
        self.assertEqual(send_params["provider"], "telegram")
        self.assertEqual(send_params["message"]["channel_id"], "-100123")
        self.assertEqual(send_params["message"]["reply_to"], "m1")
        self.assertEqual(send_params["message"]["text"], "Reply line 1\nReply line 2")

        # prime-agent was opened on the per-chat session file
        agent_cmd, _ = fake.calls[1]
        self.assertIn("--resume", agent_cmd)
        self.assertTrue(agent_cmd[agent_cmd.index("--resume") + 1].endswith(
            "pc-telegram--100123.jsonl"))

    def test_bridge_skips_other_chat_when_filtered(self):
        inbound = sample_message()
        lines = (ScriptBuilder().init({}).listen({"started": ["telegram"]})
                 .notify("event.message", {"message": inbound}).build())
        fake = ScriptedPopen([
            (lambda c: c[0].endswith("pc"), lines),
            (lambda c: c[0].endswith("prime-agent"), [AGENT_END]),
        ])
        result = bridge(provider="telegram", channel_id="other-chat", session_dir="/tmp/sd",
                        popen=fake, listen_timeout_secs=1.0, once=True)
        self.assertEqual(result["replies"], [])
        self.assertEqual(len(fake.calls), 1)  # prime-agent never spawned

    def test_bridge_no_text_reply_skips_send(self):
        inbound = sample_message()
        no_text_end = json.dumps({"type": "agent_end", "messages": [
            {"role": "assistant", "content": [{"type": "thinking", "thinking": "..."}]}]})
        lines = (ScriptBuilder().init({}).listen({"started": ["telegram"]})
                 .notify("event.message", {"message": inbound}).build())
        fake = ScriptedPopen([
            (lambda c: c[0].endswith("pc"), lines),
            (lambda c: c[0].endswith("prime-agent"),
             [json.dumps({"type": "response", "command": "prompt", "success": True}),
              no_text_end]),
        ])
        result = bridge(provider="telegram", session_dir="/tmp/sd", popen=fake,
                        listen_timeout_secs=5.0, once=True)
        self.assertEqual(result["replies"][0]["reply"], "")
        self.assertIsNone(result["replies"][0]["receipt"])
        pc_frames = [json.loads(l) for l in fake.calls[0][1].written.splitlines()]
        self.assertEqual([f["method"] for f in pc_frames], ["initialize", "listen", "shutdown"])

# ---------------------------------------------------------------------------
# pc-connect CLI delegation (preferred backend for one-shot ops)
# ---------------------------------------------------------------------------

class TestConnectCli(unittest.TestCase):
    def test_send_parses_receipt(self):
        receipt = {"message_id": "demo-9", "ts": 1700000000009}
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc-connect"), [json.dumps(receipt)])])
        cli = ConnectCli("/fake/pc-connect", popen=fake)
        result = cli.send("demo", "c1", "hi")
        self.assertEqual(result, receipt)
        cmd, proc = fake.calls[0]
        self.assertEqual(cmd, ["/fake/pc-connect", "send", "--provider", "demo",
                               "--chat", "c1", "--json", "--text", "hi"])
        self.assertEqual(proc.written, "")  # short text goes on argv, not stdin

    def test_send_long_text_uses_stdin(self):
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc-connect"), [json.dumps({})])])
        cli = ConnectCli("/fake/pc-connect", popen=fake)
        cli.send("demo", "c1", "line1\nline2")
        cmd, proc = fake.calls[0]
        self.assertIn("--text-file", cmd)
        self.assertIn("-", cmd)
        self.assertIn("line1\nline2", proc.written)

    def test_send_reply_to_unsupported_raises(self):
        cli = ConnectCli("/fake/pc-connect", popen=ScriptedPopen([]))
        with self.assertRaises(PcError) as ctx:
            cli.send("demo", "c1", "hi", reply_to="m0")
        self.assertIn("reply-to", str(ctx.exception))

    def test_send_error_output_raises_with_code(self):
        err = {"error": {"code": -32004, "message": "provider not started"}}
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc-connect"), [json.dumps(err)], 2)])
        cli = ConnectCli("/fake/pc-connect", popen=fake)
        with self.assertRaises(PcError) as ctx:
            cli.send("demo", "c1", "hi")
        self.assertEqual(ctx.exception.code, -32004)
        self.assertIn("not started", str(ctx.exception))

    def test_listen_parses_event_lines(self):
        msg = sample_message()
        err = {"provider": "telegram", "code": -32005, "message": "network down"}
        lines = [json.dumps({"event": "message", "message": msg}),
                 json.dumps({"event": "error", "error": err})]
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc-connect"), lines)])
        cli = ConnectCli("/fake/pc-connect", popen=fake)
        result = cli.listen(providers=["telegram"], timeout_secs=5, once=True)
        self.assertEqual(result["started"], ["telegram"])
        self.assertEqual(len(result["messages"]), 1)
        self.assertEqual(result["messages"][0]["id"], "m1")
        self.assertEqual(len(result["errors"]), 1)
        self.assertEqual(result["errors"][0]["code"], -32005)
        cmd, _ = fake.calls[0]
        self.assertEqual(cmd, ["/fake/pc-connect", "listen", "--json",
                               "--providers", "telegram", "--timeout", "5", "--once"])

    def test_check_parses_report(self):
        report = {"ok": True, "protocolVersion": "0.1.0",
                  "providers": [{"provider": "demo", "ok": True, "detail": "ok", "code": None}]}
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc-connect"), [json.dumps(report)])])
        cli = ConnectCli("/fake/pc-connect", popen=fake)
        self.assertEqual(cli.check(), report)
        cmd, _ = fake.calls[0]
        self.assertEqual(cmd, ["/fake/pc-connect", "check", "--json"])

    def test_check_with_provider_filter(self):
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc-connect"), [json.dumps({"ok": True})])])
        ConnectCli("/fake/pc-connect", popen=fake).check(provider="telegram")
        cmd, _ = fake.calls[0]
        self.assertIn("--provider", cmd)
        self.assertEqual(cmd[cmd.index("--provider") + 1], "telegram")

    def test_check_error_raises(self):
        err = {"error": {"code": -32001, "message": "bad config"}}
        fake = ScriptedPopen([(lambda c: c[0].endswith("pc-connect"), [json.dumps(err)], 2)])
        cli = ConnectCli("/fake/pc-connect", popen=fake)
        with self.assertRaises(PcError) as ctx:
            cli.check()
        self.assertEqual(ctx.exception.code, -32001)


class TestFindConnectBinary(unittest.TestCase):
    def test_env_override(self):
        with mock.patch.dict(os.environ, {"PC_CONNECT_BIN": "/opt/pc-connect"}):
            self.assertEqual(find_connect_binary(), "/opt/pc-connect")

    def test_found_on_path(self):
        with tempfile.TemporaryDirectory() as d:
            path = os.path.join(d, "pc-connect")
            with open(path, "w") as f:
                f.write("#!/bin/sh\n")
            os.chmod(path, 0o755)
            with mock.patch.dict(os.environ, {"PATH": d}, clear=False):
                # Patch out repo/home candidates (they exist in a real
                # checkout with a built cli) so PATH resolution is the
                # only source — deterministic regardless of build state.
                with mock.patch("pc_connect.os.path.isfile", side_effect=lambda c: c == path), \
                        mock.patch("pc_connect.os.path.isdir", return_value=False), \
                        mock.patch("pc_connect.os.access", side_effect=lambda c, m: c == path):
                    self.assertEqual(find_connect_binary(), path)

    def test_not_found_returns_none(self):
        with tempfile.TemporaryDirectory() as d, \
                mock.patch.dict(os.environ, {"PATH": d}, clear=False):
            # Home-spot and repo candidates are patched out so the result is
            # deterministic regardless of the checkout state.
            with mock.patch("pc_connect.os.path.isfile", return_value=False), \
                    mock.patch("pc_connect.os.path.isdir", return_value=False), \
                    mock.patch("pc_connect.os.access", return_value=False):
                self.assertIsNone(find_connect_binary())


if __name__ == "__main__":
    unittest.main(verbosity=2)
