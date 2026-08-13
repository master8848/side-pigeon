#!/usr/bin/env python3
"""pc_connect.py - stdlib-only provider-connect client for Prime Agent.

Drives the provider-connect `pc` sidecar (Rust, JSON-RPC 2.0 over stdio,
one JSON document per line) as a subprocess. Providers (Telegram, Discord,
demo) stay in Rust; this module only speaks the wire protocol and routes
inbound messages to per-chat Prime Agent sessions.

Public surface:
  PcClient                 JSON-RPC client over a `pc` subprocess
  check / send / listen    high-level operations
  session_file_for         per-chat session routing (stable file path)
  dispatch_to_agent        deliver a message to a Prime Agent session (RPC mode)
  bridge                   listen -> dispatch -> reply loop
  main()                   CLI: check | send | listen | session | dispatch | bridge

Python 3.8+ stdlib only (subprocess, threading, queue, json, argparse).
"""

from __future__ import annotations

import argparse
import json
import os
import queue
import re
import subprocess
import sys
import threading
import time

# Indirection point for tests: tests replace pc_connect.POPEN with a fake
# subprocess factory. Production code never changes this.
POPEN = subprocess.Popen

# Prime Agent's default session directory (docs/sessions.md).
DEFAULT_SESSION_DIR = os.path.join(os.path.expanduser("~"), ".prime", "agent", "sessions")

# JSON-RPC error codes shared with crates/provider-transport (jsonrpc.rs).
ERROR_CODES = {
    -32700: "parse error",
    -32600: "invalid request",
    -32601: "method not found",
    -32602: "invalid params",
    -32603: "internal error",
    -32001: "provider configuration error",
    -32002: "provider auth error",
    -32003: "provider rate limit",
    -32004: "provider protocol error",
    -32005: "provider network error",
}


class PcError(Exception):
    """A JSON-RPC error response from `pc`, or a client-side failure.

    `code` is the JSON-RPC error code (negative for protocol errors, see
    ERROR_CODES); `data` is the optional structured payload from the error.
    """

    def __init__(self, code=None, message="pc error", data=None):
        self.code = code
        self.data = data
        super().__init__(message)


def find_pc_binary():
    """Locate the `pc` sidecar binary.

    Order: $PC_BIN, the provider-connect repo's target/ build (when running
    from the repo), common user install spots, then PATH.
    """
    env_bin = os.environ.get("PC_BIN")
    if env_bin:
        return env_bin
    candidates = []
    # This file lives at <repo>/pi-plugin/pc_connect.py (or the skill copy at
    # ~/.prime/agent/skills/provider-connect/); the repo check is best-effort.
    repo = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    candidates.append(os.path.join(repo, "target", "release", "pc"))
    candidates.append(os.path.join(repo, "target", "debug", "pc"))
    home = os.path.expanduser("~")
    candidates += [
        os.path.join(home, ".local", "bin", "pc"),
        os.path.join(home, ".cargo", "bin", "pc"),
        "/opt/homebrew/bin/pc",
        "/usr/local/bin/pc",
    ]
    for candidate in candidates:
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return "pc"  # fall back to PATH


def find_connect_binary():
    """Locate the `pc-connect` CLI (preferred for one-shot operations).

    Order: $PC_CONNECT_BIN, the provider-connect repo's target/ build, common
    user install spots, then PATH. Returns None when not found (the caller
    falls back to the JSON-RPC `pc` sidecar).
    """
    env_bin = os.environ.get("PC_CONNECT_BIN")
    if env_bin:
        return env_bin
    candidates = []
    repo = os.path.abspath(os.path.join(os.path.dirname(os.path.abspath(__file__)), ".."))
    candidates.append(os.path.join(repo, "target", "release", "pc-connect"))
    candidates.append(os.path.join(repo, "target", "debug", "pc-connect"))
    home = os.path.expanduser("~")
    candidates += [
        os.path.join(home, ".local", "bin", "pc-connect"),
        os.path.join(home, ".cargo", "bin", "pc-connect"),
        "/opt/homebrew/bin/pc-connect",
        "/usr/local/bin/pc-connect",
    ]
    for candidate in candidates:
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    for directory in os.environ.get("PATH", "").split(os.pathsep):
        candidate = os.path.join(directory or ".", "pc-connect")
        if os.path.isfile(candidate) and os.access(candidate, os.X_OK):
            return candidate
    return None


def sanitize_component(value, max_len=80):
    """Sanitize a chat/provider id into a filesystem-safe token."""
    safe = re.sub(r"[^A-Za-z0-9._-]", "_", str(value)).strip("._")
    return (safe[:max_len] or "default")


def session_file_for(provider, channel_id, session_dir=None):
    """Stable per-chat Prime Agent session file.

    Convention: <session_dir>/pc-<provider>-<sanitized channel id>.jsonl,
    with session_dir defaulting to Prime Agent's default session directory
    (~/.prime/agent/sessions). Pass this path to `prime-agent --resume <path>`
    (or to dispatch_to_agent): Prime Agent creates the file on first contact
    and resumes the same conversation afterwards.
    """
    base = os.path.abspath(os.path.expanduser(session_dir or DEFAULT_SESSION_DIR))
    name = "pc-{}-{}.jsonl".format(sanitize_component(provider), sanitize_component(channel_id))
    return os.path.join(base, name)


def message_text(message):
    """Extract the textual body of a ChannelMessage.

    content is a list of serde-externally-tagged parts: {"Text": "..."} or
    {"Media": {...}} (crates/provider-core/src/schema.rs).
    """
    parts = []
    for part in message.get("content") or []:
        if isinstance(part, dict):
            if "Text" in part and isinstance(part["Text"], str):
                parts.append(part["Text"])
            elif "Media" in part:
                media = part["Media"] if isinstance(part["Media"], dict) else {}
                kind = str(media.get("kind", "file")).lower()
                caption = media.get("caption")
                parts.append("[{}]".format(kind) if not caption else "[{}] {}".format(kind, caption))
        elif isinstance(part, str):
            parts.append(part)
    return "\n".join(parts)


def format_message(message):
    """One-line human summary of a ChannelMessage."""
    sender = message.get("sender") or {}
    who = sender.get("name") or sender.get("username") or sender.get("id") or "?"
    text = message_text(message).replace("\n", " ")
    return "{channel} chat={channel_id} sender={sender} ts={ts} id={id}: {text}".format(
        channel=message.get("channel", "?"),
        channel_id=message.get("channel_id", "?"),
        sender=who,
        ts=message.get("ts", "?"),
        id=message.get("id", "?"),
        text=text[:200],
    )


def parse_event_message(notification):
    """Unwrap an event.message notification into the ChannelMessage dict.

    Wire shape (crates/provider-transport/src/jsonrpc.rs): the notification
    params are {"message": ChannelMessage}.
    """
    params = notification.get("params") or {}
    if isinstance(params, dict) and isinstance(params.get("message"), dict):
        return params["message"]
    if isinstance(params, dict) and "id" in params:
        return params
    raise PcError(None, "malformed event.message notification: {!r}".format(notification))


def build_prompt(message, template=None):
    """Compose the user prompt delivered to the agent session for a message.

    `template` may be a str.format() template using the keys below.
    """
    sender = message.get("sender") or {}
    ctx = {
        "channel": message.get("channel", "?"),
        "channel_id": message.get("channel_id", "?"),
        "sender_id": sender.get("id", "?"),
        "sender_name": sender.get("name") or sender.get("username") or sender.get("id") or "?",
        "text": message_text(message),
        "reply_target": message.get("reply_target") or "",
        "thread_ts": message.get("thread_ts") or "",
        "explicitly_addressed": bool(message.get("explicitly_addressed")),
        "ts": message.get("ts", 0),
    }
    if template:
        try:
            return template.format(**ctx)
        except (KeyError, IndexError, ValueError) as exc:
            raise PcError(None, "bad prompt template: {}".format(exc)) from exc
    addressed = " (addressed to you)" if ctx["explicitly_addressed"] else ""
    return (
        "Incoming message on {channel} in chat {channel_id}{addressed} from {sender_name} "
        "({sender_id}):\n\n{text}\n\n"
        "Reply to the sender in the same chat. Be concise; the reply is delivered as-is."
    ).format(addressed=addressed, **ctx)


class PcClient:
    """JSON-RPC 2.0 client over a `pc` subprocess (NDJSON framing on stdio).

    All logging from `pc` goes to stderr; stdout carries the protocol. A
    background thread reads stdout and routes responses (matched by id) and
    notifications (event.message / event.error) to separate queues.
    """

    def __init__(self, pc_bin=None, config_path=None, env=None, popen=None,
                 timeout=30.0, stderr=None):
        self.pc_bin = pc_bin or find_pc_binary()
        self._timeout = timeout
        self._popen = popen or POPEN
        command = [self.pc_bin]
        if config_path:
            command += ["-c", config_path]
        proc_env = dict(os.environ)
        if env:
            proc_env.update(env)
        try:
            self._proc = self._popen(
                command,
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=stderr,
                env=proc_env,
                text=True,
                bufsize=1,
                encoding="utf-8",
                errors="replace",
            )
        except OSError as exc:
            raise PcError(None, "failed to spawn pc binary {}: {}".format(self.pc_bin, exc)) from exc
        self._responses = queue.Queue()
        self._notifications = queue.Queue()
        self._next_id = 1  # first request id is 1 (matches pc e2e convention)
        self._id_lock = threading.Lock()
        self._initialized = False
        self._closed = False
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()

    # -- protocol plumbing -------------------------------------------------

    def _read_loop(self):
        try:
            for line in self._proc.stdout:
                line = line.strip()
                if not line:
                    continue
                try:
                    msg = json.loads(line)
                except ValueError:
                    continue  # stdout must be NDJSON; ignore junk defensively
                if not isinstance(msg, dict):
                    continue
                if "id" in msg and "method" not in msg:
                    self._responses.put(msg)
                elif "method" in msg:
                    self._notifications.put(msg)
        finally:
            # EOF sentinels: request()/notification readers treat None as exit.
            self._responses.put(None)
            self._notifications.put(None)

    def request(self, method, params=None, timeout=None):
        """Send one request and wait for the matching response (or error)."""
        with self._id_lock:
            req_id = self._next_id
            self._next_id += 1
        frame = {"jsonrpc": "2.0", "id": req_id, "method": method}
        if params is not None:
            frame["params"] = params
        try:
            self._proc.stdin.write(json.dumps(frame) + "\n")
            self._proc.stdin.flush()
        except (BrokenPipeError, OSError) as exc:
            raise PcError(None, "pc process not writable: {}".format(exc)) from exc
        deadline = time.monotonic() + (timeout if timeout is not None else self._timeout)
        while True:
            remaining = deadline - time.monotonic()
            if remaining <= 0:
                raise PcError(None, "timeout waiting for '{}' response from pc".format(method))
            try:
                resp = self._responses.get(timeout=remaining)
            except queue.Empty:
                continue
            if resp is None:
                raise PcError(None, "pc process exited before responding to '{}'".format(method))
            if resp.get("id") != req_id:
                continue  # stale response; drop (client is single-flight)
            if resp.get("error"):
                err = resp["error"]
                code = err.get("code")
                message = err.get("message") or ERROR_CODES.get(code, "pc error")
                raise PcError(code, message, err.get("data"))
            return resp.get("result")

    def _next_notification(self, deadline=None):
        """Yield the next notification, or (None, timed_out).

        Returns (notification, False) on a frame, (None, True) when the
        deadline passed with nothing new, and (None, False) on EOF.
        """
        while True:
            remaining = None if deadline is None else deadline - time.monotonic()
            if remaining is not None and remaining <= 0:
                return None, True
            try:
                notif = self._notifications.get(timeout=remaining)
            except queue.Empty:
                continue
            if notif is None:
                return None, False
            return notif, False

    # -- high-level operations ---------------------------------------------

    def _ensure_initialized(self):
        """Send `initialize` once per sidecar process (idempotent)."""
        if not self._initialized:
            self.request("initialize")
            self._initialized = True

    def check(self):
        """initialize -> capabilities object (providers, methods, features)."""
        self._ensure_initialized()
        # Re-query capabilities so a later `check` reflects current state.
        return self.request("capabilities")

    def send(self, provider, channel_id, text, reply_to=None):
        """Send a text message; returns the SendReceipt dict.

        The pc sidecar requires a provider to be started before sending
        ("provider not started: <id> (call listen first)"); on that error we
        start the provider with `listen` and retry once.
        """
        self._ensure_initialized()
        message = {"channel_id": channel_id, "text": text}
        if reply_to is not None:
            message["reply_to"] = reply_to
        try:
            return self.request("send", {"provider": provider, "message": message})
        except PcError as exc:
            if exc.code == -32004 and "not started" in str(exc):
                # Start the provider, then retry the send once.
                self.listen(providers=[provider], timeout_secs=0.5)
                return self.request("send", {"provider": provider, "message": message})
            raise

    def listen(self, providers=None, timeout_secs=None, once=False):
        """Start providers and collect inbound messages.

        Returns {"started": [...], "messages": [ChannelMessage...],
        "errors": [ErrorEvent...]}. With once=True returns after the first
        event.message; with timeout_secs set, stops at the deadline (None =
        wait forever).
        """
        self._ensure_initialized()
        params = {"providers": providers} if providers else None
        listen_result = self.request("listen", params) or {}
        started = listen_result.get("started", listen_result)
        messages, errors = [], []
        deadline = None if timeout_secs is None else time.monotonic() + timeout_secs
        while True:
            if once and messages:
                break
            notif, timed_out = self._next_notification(deadline)
            if notif is None:
                break
            method = notif.get("method")
            if method == "event.message":
                try:
                    messages.append(parse_event_message(notif))
                except PcError:
                    continue
            elif method == "event.error":
                errors.append(notif.get("params"))
        return {"started": started, "messages": messages, "errors": errors}

    def close(self, shutdown=True):
        """Stop the subprocess: request shutdown (best effort), then reap."""
        if self._closed:
            return
        self._closed = True
        if shutdown:
            try:
                self.request("shutdown", timeout=5.0)
            except PcError:
                pass
        try:
            self._proc.stdin.close()
        except (OSError, ValueError):
            pass
        try:
            self._proc.wait(timeout=10)
        except subprocess.TimeoutExpired:
            try:
                self._proc.kill()
            except OSError:
                pass
            self._proc.wait()

    def __enter__(self):
        return self

    def __exit__(self, *exc_info):
        self.close()
        return False


class ConnectCli:
    """Wrapper for the `pc-connect` CLI (cli/, subcommands send/listen/check).

    Preferred for one-shot operations when the binary is available: it embeds
    the same provider logic as the `pc` sidecar in a single process. Output
    contracts (cli/src/ops.rs):
      send   -> {"message_id", "ts"} on stdout ({"error": {...}} + exit != 0)
      listen -> NDJSON {"event":"message","message":{...}} /
                {"event":"error","error":{...}} per line
      check  -> {"ok", "protocolVersion", "providers": [{provider, ok, detail, code}]}
    """

    def __init__(self, binary, popen=None):
        self.binary = binary
        self._popen = popen or POPEN

    def _spawn(self, args):
        return self._popen(
            [self.binary] + args,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
            env=dict(os.environ),
            text=True,
            bufsize=1,
            encoding="utf-8",
            errors="replace",
        )

    def send(self, provider, channel_id, text, reply_to=None, timeout=60.0):
        """One-shot send. reply_to is not supported by pc-connect: raise."""
        if reply_to is not None:
            raise PcError(None, "pc-connect send has no --reply-to; use the pc sidecar for reply threading")
        args = ["send", "--provider", provider, "--chat", channel_id, "--json"]
        use_stdin = "\n" in text or len(text) > 4000
        if use_stdin:
            args += ["--text-file", "-"]
        else:
            args += ["--text", text]
        proc = self._spawn(args)
        try:
            if use_stdin:
                proc.stdin.write(text + "\n")
                proc.stdin.flush()
            proc.stdin.close()
        except (BrokenPipeError, OSError):
            pass
        out, _err = proc.communicate(timeout=timeout)
        if proc.returncode != 0:
            raise self._error_from_output(out)
        return json.loads(out)

    def listen(self, providers=None, timeout_secs=None, once=False, timeout=300.0):
        """One-shot listen; returns the same shape as PcClient.listen()."""
        args = ["listen", "--json"]
        if providers:
            args += ["--providers", ",".join(providers)]
        if timeout_secs is not None:
            args += ["--timeout", str(int(timeout_secs))]
        if once:
            args += ["--once"]
        proc = self._spawn(args)
        try:
            proc.stdin.close()
        except (BrokenPipeError, OSError):
            pass
        messages, errors = [], []
        for line in proc.stdout:
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if not isinstance(event, dict):
                continue
            if event.get("event") == "message" and isinstance(event.get("message"), dict):
                messages.append(event["message"])
            elif event.get("event") == "error":
                errors.append(event.get("error"))
        proc.wait(timeout=timeout)
        return {"started": list(providers or []), "messages": messages, "errors": errors}

    def check(self, provider=None, timeout=120.0):
        """One-shot connectivity check; returns the pc-connect report dict."""
        args = ["check", "--json"]
        if provider:
            args += ["--provider", provider]
        proc = self._spawn(args)
        try:
            proc.stdin.close()
        except (BrokenPipeError, OSError):
            pass
        out, _err = proc.communicate(timeout=timeout)
        if proc.returncode != 0:
            raise self._error_from_output(out)
        return json.loads(out)

    @staticmethod
    def _error_from_output(out):
        try:
            payload = json.loads(out)
            err = payload.get("error") or {}
            return PcError(err.get("code"), err.get("message") or out.strip(), err.get("data"))
        except ValueError:
            return PcError(None, "pc-connect failed: {}".format(out.strip() or "unknown error"))


def dispatch_to_agent(text, session_file, cwd=None, prime_agent_bin=None, timeout=600.0,
                      popen=None, env=None):
    """Deliver `text` into the Prime Agent session at `session_file`.

    Handoff: spawns `prime-agent --mode rpc --resume <session_file>` (the
    documented headless protocol, docs/rpc.md), sends one `prompt` command,
    and waits for the `agent_end` event. Returns the concatenated text of the
    final assistant message ("" if the agent produced no text).

    The session file is created by Prime Agent on first use and resumed
    afterwards, giving every chat one stable conversation.
    """
    binary = prime_agent_bin or os.environ.get("PRIME_AGENT_BIN") or "prime-agent"
    command = [binary, "--mode", "rpc", "--resume", os.path.abspath(session_file)]
    if cwd:
        command += ["--cwd", str(cwd)]
    proc_env = dict(os.environ)
    if env:
        proc_env.update(env)
    proc = (popen or POPEN)(
        command,
        stdin=subprocess.PIPE,
        stdout=subprocess.PIPE,
        stderr=None,
        env=proc_env,
        text=True,
        bufsize=1,
        encoding="utf-8",
        errors="replace",
    )
    frame = {"type": "prompt", "message": text, "id": "pc-connect"}
    try:
        proc.stdin.write(json.dumps(frame) + "\n")
        proc.stdin.flush()
    except (BrokenPipeError, OSError) as exc:
        proc.kill()
        raise PcError(None, "prime-agent not writable: {}".format(exc)) from exc
    deadline = time.monotonic() + timeout
    parts = []
    saw_agent_end = False
    try:
        for line in proc.stdout:
            if time.monotonic() > deadline:
                raise PcError(None, "timeout after {}s waiting for agent reply".format(timeout))
            line = line.strip()
            if not line:
                continue
            try:
                event = json.loads(line)
            except ValueError:
                continue
            if not isinstance(event, dict):
                continue
            if event.get("type") == "response" and event.get("command") == "prompt":
                if event.get("success") is False:
                    raise PcError(None, "prime-agent rejected prompt: {}".format(
                        event.get("error") or event))
            elif event.get("type") == "agent_end":
                saw_agent_end = True
                for message in event.get("messages") or []:
                    if not isinstance(message, dict) or message.get("role") != "assistant":
                        continue
                    for part in message.get("content") or []:
                        if isinstance(part, dict) and part.get("type") == "text":
                            parts.append(str(part.get("text", "")))
                break
    finally:
        try:
            proc.stdin.close()
        except OSError:
            pass
        try:
            proc.wait(timeout=30)
        except subprocess.TimeoutExpired:
            proc.kill()
            proc.wait()
    if not saw_agent_end:
        raise PcError(None, "prime-agent exited without agent_end (model error?)")
    return "\n".join(part for part in parts if part)


def bridge(provider=None, channel_id=None, config_path=None, pc_bin=None,
           session_dir=None, cwd=None, prime_agent_bin=None, listen_timeout_secs=None,
           once=True, dispatch_timeout=600.0, prompt_template=None, popen=None, env=None):
    """Listen on `pc` and route each inbound message through the agent.

    For every event.message: pick the per-chat session
    (session_file_for(channel, channel_id)), dispatch the message via
    dispatch_to_agent, and send the agent's reply back with reply_to set to
    the inbound message id.

    Returns {"started": [...], "replies": [{message, session, reply, receipt}]}.
    """
    client = PcClient(pc_bin=pc_bin, config_path=config_path, env=env, popen=popen)
    replies = []
    try:
        client.request("initialize")
        params = {"providers": [provider]} if provider else None
        listen_result = client.request("listen", params) or {}
        started = listen_result.get("started", listen_result)
        deadline = None if listen_timeout_secs is None else time.monotonic() + listen_timeout_secs
        while True:
            if once and replies:
                break
            notif, timed_out = client._next_notification(deadline)
            if notif is None:
                break
            if notif.get("method") != "event.message":
                continue
            try:
                message = parse_event_message(notif)
            except PcError:
                continue
            chat = message.get("channel") or provider or "unknown"
            chat_id = message.get("channel_id") or ""
            if channel_id and chat_id != channel_id:
                continue
            session_file = session_file_for(chat, chat_id, session_dir)
            prompt_text = build_prompt(message, prompt_template)
            reply = dispatch_to_agent(prompt_text, session_file, cwd=cwd,
                                      prime_agent_bin=prime_agent_bin,
                                      timeout=dispatch_timeout, popen=popen, env=env)
            receipt = None
            if reply:
                receipt = client.request("send", {
                    "provider": chat,
                    "message": {
                        "channel_id": chat_id,
                        "text": reply,
                        "reply_to": message.get("id"),
                    },
                })
            replies.append({
                "message": message.get("id"),
                "session": session_file,
                "reply": reply,
                "receipt": receipt,
            })
        return {"started": started, "replies": replies}
    finally:
        client.close()


# ---------------------------------------------------------------------------
# CLI
# ---------------------------------------------------------------------------

def _connect_client(args):
    """Return (backend_label, client) preferring pc-connect for one-shots."""
    binary = args.connect_bin
    if binary is None:
        binary = find_connect_binary()
    if binary is not None:
        return ("pc-connect", ConnectCli(binary))
    return ("pc", PcClient(pc_bin=args.pc, config_path=args.config))


def cmd_check(args):
    label, client = _connect_client(args)
    if label == "pc-connect":
        report = client.check(provider=args.provider)
        if args.json:
            print(json.dumps(report, indent=2, default=str))
        else:
            print("backend: pc-connect")
            print("protocolVersion: {}".format(report.get("protocolVersion", "?")))
            for entry in report.get("providers", []):
                status = "OK" if entry.get("ok") else "FAIL"
                print("provider {}: {} {}".format(entry.get("provider"), status, entry.get("detail", "")))
        return 0 if report.get("ok") else 1
    with client as client:
        caps = client.check()
    if args.provider:
        caps = dict(caps, providers=[p for p in caps.get("providers", []) if p == args.provider])
    if args.json:
        print(json.dumps(caps, indent=2, default=str))
    else:
        print("protocolVersion: {}".format(caps.get("protocolVersion", "?")))
        print("transport: {}".format(", ".join(caps.get("transport", []))))
        print("providers: {}".format(", ".join(caps.get("providers", [])) or "(none)"))
        print("methods: {}".format(", ".join(caps.get("methods", []))))
        print("notifications: {}".format(", ".join(caps.get("notifications", []))))
        print("features: {}".format(", ".join(caps.get("features", []))))
    return 0


def cmd_send(args):
    text = args.text
    if text is None:
        text = sys.stdin.read().strip()
    if not text:
        print("pc_connect: send requires --text or piped stdin", file=sys.stderr)
        return 2
    # pc-connect has no --reply-to; fall back to the pc sidecar for replies.
    if args.reply_to is None:
        label, client = _connect_client(args)
        if label == "pc-connect":
            receipt = client.send(args.provider, args.chat, text)
            if args.json:
                print(json.dumps(receipt, indent=2, default=str))
            else:
                print("sent message_id={} ts={} (via pc-connect)".format(
                    receipt.get("message_id"), receipt.get("ts")))
            return 0
    with PcClient(pc_bin=args.pc, config_path=args.config) as client:
        receipt = client.send(args.provider, args.chat, text, reply_to=args.reply_to)
    if args.json:
        print(json.dumps(receipt, indent=2, default=str))
    else:
        print("sent message_id={} ts={}".format(receipt.get("message_id"), receipt.get("ts")))
    return 0


def cmd_listen(args):
    label, client = _connect_client(args)
    if label == "pc-connect":
        result = client.listen(providers=[args.provider] if args.provider else None,
                               timeout_secs=args.timeout, once=args.once)
    else:
        with client as client:
            result = client.listen(providers=[args.provider] if args.provider else None,
                                   timeout_secs=args.timeout, once=args.once)
    if args.json:
        print(json.dumps(result, indent=2, default=str))
    else:
        for message in result["messages"]:
            print(format_message(message))
        for error in result["errors"]:
            print("event.error: {}".format(error), file=sys.stderr)
    return 0


def cmd_session(args):
    print(session_file_for(args.provider, args.chat, args.session_dir))
    return 0


def cmd_dispatch(args):
    text = args.text
    if text is None:
        text = sys.stdin.read().strip()
    reply = dispatch_to_agent(text, args.session, cwd=args.cwd, timeout=args.timeout)
    if args.json:
        print(json.dumps({"reply": reply}, indent=2, default=str))
    else:
        print(reply)
    return 0


def cmd_bridge(args):
    result = bridge(provider=args.provider, channel_id=args.chat, config_path=args.config,
                    pc_bin=args.pc, session_dir=args.session_dir, cwd=args.cwd,
                    listen_timeout_secs=args.timeout, once=args.once, env=None)
    if args.json:
        print(json.dumps(result, indent=2, default=str))
    else:
        for reply in result["replies"]:
            print("message={} session={}".format(reply["message"], reply["session"]))
            print("reply: {}".format((reply["reply"] or "(no text)").replace("\n", "\n       ")))
    return 0


def build_arg_parser():
    parser = argparse.ArgumentParser(
        prog="pc_connect.py",
        description="provider-connect client for Prime Agent (drives the `pc` sidecar).")
    parser.add_argument("--pc", default=None, help="path to the pc sidecar binary (default: $PC_BIN, repo target/, PATH)")
    parser.add_argument("--config", default=None, help="path to a pc JSON config file (default: $PC_CONFIG)")
    parser.add_argument("--json", action="store_true", help="machine-readable JSON output")
    sub = parser.add_subparsers(dest="command", required=True)

    # Common flags available on every subcommand (parent parser).
    common = argparse.ArgumentParser(add_help=False)
    common.add_argument("--pc", default=None, dest="pc",
                        help="path to the pc sidecar binary (default: $PC_BIN, repo target/, PATH)")
    common.add_argument("--config", default=None, dest="config",
                        help="path to a pc JSON config file (default: $PC_CONFIG)")
    common.add_argument("--json", action="store_true", dest="json",
                        help="machine-readable JSON output")

    p = sub.add_parser("check", parents=[common], help="query provider status / capabilities")
    p.add_argument("--provider", default=None, help="only report this provider id")
    p.set_defaults(func=cmd_check)

    p = sub.add_parser("send", parents=[common], help="send a message to a chat")
    p.add_argument("--provider", required=True, help="provider id (telegram, discord, demo)")
    p.add_argument("--chat", required=True, help="chat/room id")
    p.add_argument("--text", default=None, help="message text (default: read stdin)")
    p.add_argument("--reply-to", default=None, help="provider message id this replies to")
    p.set_defaults(func=cmd_send)

    p = sub.add_parser("listen", parents=[common], help="poll for inbound messages")
    p.add_argument("--provider", default=None, help="only start this provider")
    p.add_argument("--timeout", type=float, default=30.0, help="seconds to listen (default 30)")
    p.add_argument("--once", action="store_true", help="stop after the first message")
    p.set_defaults(func=cmd_listen)

    p = sub.add_parser("session", parents=[common], help="print the Prime Agent session file for a chat")
    p.add_argument("--provider", required=True)
    p.add_argument("--chat", required=True)
    p.add_argument("--session-dir", default=None, help="session directory (default ~/.prime/agent/sessions)")
    p.set_defaults(func=cmd_session)

    p = sub.add_parser("dispatch", parents=[common], help="deliver text to a Prime Agent session and print the reply")
    p.add_argument("--session", required=True, help="session file path (see `session`)")
    p.add_argument("--text", default=None, help="prompt text (default: read stdin)")
    p.add_argument("--cwd", default=None, help="working directory for the agent")
    p.add_argument("--timeout", type=float, default=600.0, help="seconds to wait for the reply")
    p.set_defaults(func=cmd_dispatch)

    p = sub.add_parser("bridge", parents=[common], help="listen, route to per-chat sessions, and reply")
    p.add_argument("--provider", default=None, help="provider id to listen on")
    p.add_argument("--chat", default=None, help="only handle this chat id")
    p.add_argument("--timeout", type=float, default=None, help="seconds to listen (default: until Ctrl-C / once)")
    p.add_argument("--once", action="store_true", help="handle one message then stop")
    p.add_argument("--session-dir", default=None)
    p.add_argument("--cwd", default=None)
    p.set_defaults(func=cmd_bridge)
    return parser


def main(argv=None):
    args = build_arg_parser().parse_args(argv)
    try:
        return args.func(args)
    except PcError as exc:
        print("pc_connect: {}".format(exc), file=sys.stderr)
        return 1
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
