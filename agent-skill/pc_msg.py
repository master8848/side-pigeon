#!/usr/bin/env python3
"""pc_msg — out-of-process lean messaging for agents via provider-connect.

Any agent that can run shell commands gets provider messaging without a
plugin: this script drives the `pc-connect` CLI (the canonical contract) and
falls back to the `pc` JSON-RPC sidecar over stdio when pc-connect is absent.

Commands
--------
  pc_msg send     --provider X --chat Y [--text T | --text-file -]
                  Send one message; prints the receipt JSON on stdout.
  pc_msg poll     [--timeout N] [--once] [--providers a,b] [--json]
                  Receive messages; prints one normalized JSON line per
                  event (event.message / event.error). Human-readable hints
                  (resolved session + one-command handoff) go to stderr.
  pc_msg forward  --session ID | --chat Y [--text T | --text-file -]
                  Hand one message to the mapped agent session with a single
                  command (opencode run --session ... / prime-agent send ...).
  pc_msg resolve  --chat Y [--provider X] [--autodetect]
                  Print the sessions.json entry mapped to a chat id.
  pc_msg check    [--provider X]
                  Exit 0 if the provider/stack is available, 1 otherwise.
  pc_msg sessions
                  Print the configured session mappings as JSON.

Config
------
  sessions.json maps chat ids to agent sessions. Lookup order:
    1. --config PATH
    2. $PC_MSG_CONFIG
    3. <this script's dir>/sessions.json
    4. ~/.config/pc-msg/sessions.json
  See sessions.example.json in this directory and the README for the schema.

Backends
--------
  pc-connect (binary from the cli/ workspace) is used when found (env
  PC_CONNECT_BIN overrides PATH lookup). Otherwise the `pc` JSON-RPC sidecar
  is spawned (env PC_BIN, else target/{debug,release}/pc under the repo root,
  else PATH). Only the Python 3 standard library is used.
"""

import argparse
import json
import os
import queue
import shlex
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path

PROG = "pc_msg"
VERSION = "0.1.0"

# --------------------------------------------------------------------------
# Errors / config
# --------------------------------------------------------------------------


class PcMsgError(Exception):
    """Expected runtime failure with a stable exit code."""

    def __init__(self, message, exit_code=1):
        super().__init__(message)
        self.exit_code = exit_code


def default_config_paths():
    here = Path(__file__).resolve().parent
    return [
        here / "sessions.json",
        Path.home() / ".config" / "pc-msg" / "sessions.json",
    ]


def load_sessions(config_path=None):
    """Load the sessions.json document. Returns (data, path_or_None)."""
    if config_path:
        paths = [Path(config_path)]
    elif os.environ.get("PC_MSG_CONFIG"):
        paths = [Path(os.environ["PC_MSG_CONFIG"])]
    else:
        paths = default_config_paths()
    for p in paths:
        if not p.is_file():
            continue
        try:
            data = json.loads(p.read_text())
        except json.JSONDecodeError as e:
            raise PcMsgError("invalid sessions config {}: {}".format(p, e))
        if not isinstance(data, dict) or not isinstance(data.get("sessions"), list):
            raise PcMsgError(
                'sessions config {} must be an object with a "sessions" array'.format(p)
            )
        return data, p
    return {"sessions": []}, None


# --------------------------------------------------------------------------
# Binary discovery
# --------------------------------------------------------------------------


def find_pc_connect():
    """Locate the pc-connect CLI binary, or None."""
    env = os.environ.get("PC_CONNECT_BIN")
    if env:
        return env
    return shutil.which("pc-connect")


def _repo_root():
    """Find the provider-connect repo root (contains Cargo.toml + crates/)."""
    here = Path(__file__).resolve().parent
    for p in [here] + list(here.parents):
        if (p / "Cargo.toml").is_file() and (p / "crates").is_dir():
            return p
    return None


def find_pc():
    """Locate the `pc` JSON-RPC sidecar binary, or None."""
    env = os.environ.get("PC_BIN")
    if env:
        return env
    found = shutil.which("pc")
    if found:
        return found
    root = _repo_root()
    if root is not None:
        for rel in ("target/release/pc", "target/debug/pc"):
            cand = root / rel
            if cand.is_file():
                return str(cand)
    return None


def pick_backend(backend_arg):
    """Resolve --backend auto|pc-connect|pc into a Backend instance."""
    connect = find_pc_connect()
    pc = find_pc()
    if backend_arg == "pc-connect":
        if not connect:
            raise PcMsgError("--backend pc-connect requested but no pc-connect binary found")
        return ConnectBackend(connect)
    if backend_arg == "pc":
        if not pc:
            raise PcMsgError("--backend pc requested but no pc binary found")
        return PcBackend(pc)
    # auto
    if connect:
        return ConnectBackend(connect)
    if pc:
        return PcBackend(pc)
    raise PcMsgError(
        "neither pc-connect nor pc found; set PC_CONNECT_BIN / PC_BIN "
        "or install pc-connect (cli/ workspace) / build pc (bin/pc)"
    )


# --------------------------------------------------------------------------
# Event normalization
# --------------------------------------------------------------------------

EVENT_MESSAGE = "event.message"
EVENT_ERROR = "event.error"
EVENT_NAMES = (EVENT_MESSAGE, EVENT_ERROR, "event.draft", "event.choice")


def normalize_event(line):
    """Normalize one output line into (event_name, payload_dict) or None.

    Accepts both shapes the stack emits:
      * raw JSON-RPC notification:
        {"jsonrpc":"2.0","method":"event.message","params":{"message": {...}}}
      * flat line (pc-connect listen style):
        {"event":"event.message","message": {...}}
    Normalized payloads: event.message -> {"message": <ChannelMessage>},
    event.error -> {"error": <ErrorEvent>}, others pass params through.
    Non-event / non-JSON lines return None.
    """
    line = line.strip()
    if not line:
        return None
    try:
        obj = json.loads(line)
    except json.JSONDecodeError:
        return None
    if not isinstance(obj, dict):
        return None
    method = obj.get("method")
    if method in EVENT_NAMES:
        params = obj.get("params") or {}
        if method == EVENT_MESSAGE:
            return (method, {"message": params.get("message")})
        if method == EVENT_ERROR:
            return (method, {"error": params})
        return (method, params)
    event = obj.get("event")
    if isinstance(event, str) and event in EVENT_NAMES:
        payload = {k: v for k, v in obj.items() if k != "event"}
        if event == EVENT_MESSAGE and "message" not in payload:
            return (event, {"message": payload})
        if event == EVENT_ERROR and "error" not in payload:
            return (event, {"error": payload})
        return (event, payload)
    return None


def message_text(message):
    """Extract the human-readable text from a ChannelMessage."""
    if not isinstance(message, dict):
        return ""
    content = message.get("content")
    parts = []
    if isinstance(content, list):
        parts = content
    elif isinstance(content, str):
        return content
    out = []
    for p in parts:
        if isinstance(p, str):
            out.append(p)
        elif isinstance(p, dict):
            # serde externally-tagged enum: {"Text": "..."} | {"Media": {...}}
            text = p.get("Text")
            if isinstance(text, str):
                out.append(text)
    return " ".join(out).strip()


# --------------------------------------------------------------------------
# Session resolution + handoff
# --------------------------------------------------------------------------


def resolve_session(chat_id, provider=None, config_path=None):
    """Find the sessions.json entry mapping a chat id to an agent session."""
    data, _ = load_sessions(config_path)
    for entry in data.get("sessions", []):
        if not isinstance(entry, dict):
            continue
        chats = [entry.get("chat")]
        chats += list(entry.get("chats") or [])
        if chat_id in chats:
            if provider and entry.get("provider") and entry["provider"] != provider:
                continue
            return entry
    return None


def resolve_by_session(session_id, config_path=None):
    """Find a sessions.json entry by its session id or local label."""
    data, _ = load_sessions(config_path)
    for entry in data.get("sessions", []):
        if not isinstance(entry, dict):
            continue
        if session_id in (entry.get("session"), entry.get("id")):
            return entry
    return None


def autodetect_opencode_session():
    """Most-recent opencode session id (heuristic; sessions.json is authoritative).

    Tries `opencode session list --format json`, then falls back to scanning
    the on-disk session storage for the newest session directory.
    """
    try:
        r = subprocess.run(
            ["opencode", "session", "list", "--format", "json"],
            capture_output=True,
            text=True,
            timeout=20,
        )
    except Exception:
        r = None
    if r is not None and r.returncode == 0 and r.stdout.strip():
        try:
            data = json.loads(r.stdout)
        except json.JSONDecodeError:
            data = None
        if isinstance(data, list) and data:
            sid = data[0].get("id") if isinstance(data[0], dict) else None
            if sid:
                return str(sid)
    for base in (
        Path.home() / ".local/share/opencode/storage/session",
        Path.home() / ".config/opencode/storage/session",
    ):
        if not base.is_dir():
            continue
        dirs = [d for d in base.iterdir() if d.is_dir()]
        if not dirs:
            continue
        newest = max(dirs, key=lambda d: d.stat().st_mtime)
        return newest.name
    return None


def prime_session_dirs():
    """Session artifact dirs under ~/.prime/agent/session-artifacts (newest first)."""
    base = Path.home() / ".prime" / "agent" / "session-artifacts"
    if not base.is_dir():
        return []
    dirs = [d for d in base.iterdir() if d.is_dir()]
    dirs.sort(key=lambda d: d.stat().st_mtime, reverse=True)
    return [d.name for d in dirs]


def build_handoff(entry, text, session_override=None):
    """Build the argv for the one-command handoff to the mapped agent."""
    session = session_override or entry.get("session")
    handoff = entry.get("handoff")
    if handoff:
        if isinstance(handoff, str):
            handoff = shlex.split(handoff)
        if not isinstance(handoff, list):
            raise PcMsgError('handoff for session must be a command list or string')
        return [
            str(a)
            .replace("{text}", text)
            .replace("{session}", str(session or ""))
            .replace("{chat}", str(entry.get("chat", "")))
            .replace("{provider}", str(entry.get("provider", "")))
            for a in handoff
        ]
    agent = entry.get("agent", "opencode")
    if agent == "prime":
        if not session:
            raise PcMsgError("prime session entry needs a \"session\" field (the agent name from `prime-agent list`)")
        return ["prime-agent", "send", str(session), text]
    if agent == "opencode":
        if not session:
            raise PcMsgError("opencode session entry needs a \"session\" field (opencode session id)")
        argv = ["opencode", "run", "--session", str(session)]
        if entry.get("project"):
            argv += ["--dir", str(entry["project"])]
        argv.append(text)
        return argv
    raise PcMsgError('unknown agent {!r} (use "opencode", "prime", or a custom "handoff")'.format(agent))


# --------------------------------------------------------------------------
# pc-connect backend (canonical contract)
# --------------------------------------------------------------------------


class ConnectBackend:
    """Drives the pc-connect CLI binary per the cli/ contract."""

    name = "pc-connect"

    def __init__(self, binary, run=subprocess.run, popen=subprocess.Popen):
        self.binary = binary
        self._run = run
        self._popen = popen
        self._proc = None

    # -- send ---------------------------------------------------------------
    def send(self, provider, chat, text, text_file):
        """--text T -> --text T; --text-file - -> pass our stdin through."""
        argv = [self.binary, "send", "--provider", provider, "--chat", chat]
        if text is not None:
            argv += ["--text", text]
        else:
            argv += ["--text-file", text_file or "-"]
        r = self._run(argv)
        if r.returncode != 0:
            err = (getattr(r, "stderr", None) or b"").decode(errors="replace").strip()
            raise PcMsgError(
                "pc-connect send failed ({}): {}".format(r.returncode, err or "see stderr")
            )
        out = (getattr(r, "stdout", None) or b"").decode(errors="replace").strip()
        if out:
            sys.stdout.write(out + "\n")
            sys.stdout.flush()
        return r.returncode

    # -- listen -------------------------------------------------------------
    def listen(self, providers=None, timeout=0.0, once=False, json_mode=False):
        argv = [self.binary, "listen"]
        if providers:
            argv += ["--providers", ",".join(providers)]
        if timeout and timeout > 0:
            argv += ["--timeout", str(int(timeout))]
        if once:
            argv += ["--once"]
        if json_mode:
            argv += ["--json"]
        self._proc = self._popen(argv, stdout=subprocess.PIPE, stderr=None)
        try:
            for raw in self._proc.stdout:
                norm = normalize_event(raw)
                if norm:
                    yield norm
                elif raw.strip():
                    # unexpected non-event line: keep stdout clean, surface on stderr
                    sys.stderr.write("[pc_msg] {}: {}\n".format(self.name, raw.rstrip()))
        finally:
            self.stop()

    def stop(self):
        proc, self._proc = self._proc, None
        if proc is not None and proc.poll() is None:
            try:
                proc.terminate()
                proc.wait(timeout=5)
            except Exception:
                try:
                    proc.kill()
                except Exception:
                    pass

    # -- check --------------------------------------------------------------
    def check(self, provider=None):
        argv = [self.binary, "check"]
        if provider:
            argv += ["--provider", provider]
        r = self._run(argv)
        return r.returncode


# --------------------------------------------------------------------------
# pc sidecar backend (JSON-RPC 2.0 over stdio fallback)
# --------------------------------------------------------------------------


class RpcClient:
    """Minimal JSON-RPC 2.0 client over a child process's stdio (NDJSON).

    Responses are stored by id as they arrive, so a request that registers
    late (or a response that lands before the waiter is set up) still
    matches. Notifications go to an event queue.
    """

    def __init__(self, proc):
        self.proc = proc
        self._next_id = 1
        self._cond = threading.Condition()
        self._responses = {}  # id -> response object
        self._events = queue.Queue()
        self._eof = False
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()

    def _read_loop(self):
        try:
            for raw in self.proc.stdout:
                line = raw.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line)
                except json.JSONDecodeError:
                    continue
                if not isinstance(obj, dict):
                    continue
                if obj.get("method"):
                    self._events.put(obj)
                    continue
                if "id" in obj:
                    with self._cond:
                        self._responses[obj["id"]] = obj
                        self._cond.notify_all()
        finally:
            self._events.put(None)  # EOF sentinel
            with self._cond:
                self._eof = True
                self._cond.notify_all()

    def request(self, method, params=None, timeout=15.0):
        rid = self._next_id
        self._next_id += 1
        frame = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            frame["params"] = params
        with self._cond:
            self._responses.setdefault(rid, None)
            try:
                self.proc.stdin.write((json.dumps(frame) + "\n").encode("utf-8"))
                self.proc.stdin.flush()
            except OSError as e:
                self._responses.pop(rid, None)
                raise PcMsgError("failed to write {} request to pc: {}".format(method, e))
            deadline = time.monotonic() + timeout
            while self._responses.get(rid) is None and not self._eof:
                remaining = deadline - time.monotonic()
                if remaining <= 0:
                    self._responses.pop(rid, None)
                    rc = None
                    try:
                        rc = self.proc.poll()
                    except Exception:
                        rc = None
                    if rc is not None:
                        raise PcMsgError(
                            "pc sidecar exited (rc={}) before responding to {}".format(rc, method)
                        )
                    raise PcMsgError("timeout waiting for {} response from pc".format(method))
                self._cond.wait(remaining)
            resp = self._responses.pop(rid, None)
        if resp is None:
            rc = None
            try:
                rc = self.proc.poll()
            except Exception:
                rc = None
            raise PcMsgError(
                "pc sidecar exited (rc={}) before responding to {}".format(rc, method)
            )
        if resp.get("error"):
            err = resp["error"]
            raise PcMsgError(
                "pc {} failed: {} {}".format(method, err.get("code"), err.get("message"))
            )
        return resp.get("result")

    def next_event(self, timeout=None):
        """Next notification dict; None on EOF; ("timeout", None) on deadline."""
        if self._eof and self._events.empty():
            return None
        try:
            ev = self._events.get(timeout=timeout)
        except queue.Empty:
            return ("timeout", None)
        return ev


class PcBackend:
    """Drives the `pc` sidecar: JSON-RPC 2.0 over stdio, NDJSON framing."""

    name = "pc"

    def __init__(self, binary, pc_config=None, run=subprocess.run, popen=subprocess.Popen):
        self.binary = binary
        self.pc_config = pc_config
        self._run = run
        self._popen = popen
        self._proc = None
        self._client = None

    def _spawn(self, stderr_target):
        argv = [self.binary]
        env = dict(os.environ)
        if self.pc_config:
            env["PC_CONFIG"] = self.pc_config
        return self._popen(
            argv,
            stdin=subprocess.PIPE,
            stdout=subprocess.PIPE,
            stderr=stderr_target,
            env=env,
        )

    # -- send ---------------------------------------------------------------
    def send(self, provider, chat, text, text_file):
        if text is None:
            text = sys.stdin.read()
        proc = self._spawn(subprocess.PIPE)
        client = RpcClient(proc)
        try:
            client.request("initialize", None, timeout=15.0)
            # The sidecar starts providers lazily; `listen` starts them.
            client.request("listen", {"providers": [provider]}, timeout=15.0)
            result = client.request(
                "send",
                {"provider": provider, "message": {"channel_id": chat, "text": text}},
                timeout=30.0,
            )
        finally:
            self._shutdown(client, proc)
        sys.stdout.write(json.dumps(result, ensure_ascii=False) + "\n")
        sys.stdout.flush()
        return 0

    # -- listen -------------------------------------------------------------
    def listen(self, providers=None, timeout=0.0, once=False, json_mode=False):
        self._proc = self._spawn(None)  # sidecar logs stay visible on stderr
        client = RpcClient(self._proc)
        self._client = client
        try:
            client.request("initialize", None, timeout=15.0)
            params = {"providers": providers} if providers else None
            client.request("listen", params, timeout=15.0)
        except PcMsgError:
            self.stop()
            raise
        deadline = time.monotonic() + timeout if timeout and timeout > 0 else None
        while True:
            remaining = None
            if deadline is not None:
                remaining = max(0.0, deadline - time.monotonic())
            ev = client.next_event(remaining)
            if ev is None:
                break  # EOF
            if ev == ("timeout", None):
                break
            method = ev.get("method")
            if method == EVENT_MESSAGE:
                yield (EVENT_MESSAGE, {"message": (ev.get("params") or {}).get("message")})
            elif method == EVENT_ERROR:
                yield (EVENT_ERROR, {"error": ev.get("params")})
            elif method in ("event.draft", "event.choice"):
                yield (method, ev.get("params") or {})
            if once:
                break

    def stop(self):
        client, self._client = self._client, None
        proc, self._proc = self._proc, None
        if client is not None:
            self._shutdown(client, proc)
        elif proc is not None and proc.poll() is None:
            try:
                proc.terminate()
                proc.wait(timeout=5)
            except Exception:
                pass

    @staticmethod
    def _shutdown(client, proc):
        # Always attempt a graceful shutdown; writing to a dead child raises
        # OSError (caught below), and a short timeout bounds the wait.
        try:
            client.request("shutdown", None, timeout=2.0)
        except Exception:
            pass
        if proc is not None:
            try:
                if proc.stdin is not None:
                    proc.stdin.close()
            except Exception:
                pass
            try:
                proc.wait(timeout=5)
            except Exception:
                try:
                    proc.terminate()
                except Exception:
                    pass

    # -- check --------------------------------------------------------------
    def check(self, provider=None):
        proc = self._spawn(subprocess.PIPE)
        client = RpcClient(proc)
        try:
            caps = client.request("initialize", None, timeout=15.0)
        except PcMsgError:
            return 1
        finally:
            self._shutdown(client, proc)
        if not isinstance(caps, dict):
            return 1
        providers = caps.get("providers") or []
        if provider:
            return 0 if provider in providers else 1
        return 0 if providers else 1


# --------------------------------------------------------------------------
# Commands
# --------------------------------------------------------------------------


def resolve_text_args(text, text_file):
    """Return (text, text_file) with at most one set.

    --text-file - means: with pc-connect the child reads stdin itself; with
    the pc fallback pc_msg reads stdin. --text-file <path> is read here.
    """
    if text is not None and text_file:
        raise PcMsgError("use either --text or --text-file, not both")
    if text is not None:
        return text, None
    if text_file and text_file != "-":
        try:
            return Path(text_file).read_text(), None
        except OSError as e:
            raise PcMsgError("cannot read --text-file {}: {}".format(text_file, e))
    if text_file == "-":
        return None, "-"
    raise PcMsgError("missing --text or --text-file")


def cmd_send(args):
    text, text_file = resolve_text_args(args.text, args.text_file)
    backend = pick_backend(args.backend)
    return backend.send(args.provider, args.chat, text, text_file)


def emit_hint(message, config_path, json_mode):
    """Human-readable hint for one inbound message (stderr, never stdout)."""
    if json_mode:
        return
    chat = message.get("channel_id")
    provider = message.get("channel")
    sender = message.get("sender") or {}
    sender_name = sender.get("name") or sender.get("username") or sender.get("id") or "?"
    text = message_text(message) or "[non-text message]"
    sys.stderr.write(
        "[pc_msg] message from {} in {} chat {}: {}\n".format(sender_name, provider, chat, text)
    )
    entry = resolve_session(chat, provider, config_path)
    if entry is None:
        sys.stderr.write(
            "[pc_msg] no session mapped for {} chat {}; add an entry to sessions.json "
            "(see `pc_msg resolve --chat {} --provider {} --autodetect`)\n".format(
                provider, chat, chat, provider
            )
        )
        return
    session = entry.get("session")
    label = entry.get("id") or session
    sys.stderr.write(
        "[pc_msg] resolved session {} (agent={}) -> one-command handoff:\n".format(label, entry.get("agent", "opencode"))
    )
    sys.stderr.write(
        "  pc_msg forward --session {} --text {}\n".format(shlex.quote(str(session)), shlex.quote(text))
    )
    argv = build_handoff(entry, text)
    sys.stderr.write("  (runs: {})\n".format(" ".join(shlex.quote(a) for a in argv)))


def cmd_poll(args):
    backend = pick_backend(args.backend)
    providers = None
    if args.providers:
        providers = [p.strip() for p in args.providers.split(",") if p.strip()]
    try:
        for event, payload in backend.listen(
            providers=providers, timeout=args.timeout, once=args.once, json_mode=args.json
        ):
            out = {"event": event}
            out.update(payload)
            sys.stdout.write(json.dumps(out, ensure_ascii=False) + "\n")
            sys.stdout.flush()
            if event == EVENT_MESSAGE and isinstance(payload.get("message"), dict):
                emit_hint(payload["message"], args.config, args.json)
            if args.once and event == EVENT_MESSAGE:
                break
    finally:
        backend.stop()
    return 0


def cmd_forward(args):
    text, text_file = resolve_text_args(args.text, args.text_file)
    entry = None
    if args.session:
        entry = resolve_by_session(args.session, args.config)
        if entry is None:
            raise PcMsgError(
                "session {!r} is not in sessions.json; add an entry mapping it to a chat "
                "(see README, sessions.example.json)".format(args.session)
            )
    elif args.chat:
        entry = resolve_session(args.chat, args.provider, args.config)
        if entry is None:
            raise PcMsgError(
                "no session mapped for chat {!r}; add it to sessions.json".format(args.chat)
            )
    else:
        raise PcMsgError("forward needs --session <id> or --chat <chat-id>")
    if text is None:
        text = sys.stdin.read()
    argv = build_handoff(entry, text)
    sys.stderr.write("[pc_msg] running: {}\n".format(" ".join(shlex.quote(a) for a in argv)))
    r = subprocess.run(argv)
    return r.returncode


def cmd_resolve(args):
    out = {"chat": args.chat, "provider": args.provider, "session": None, "autodetect": {}}
    entry = resolve_session(args.chat, args.provider, args.config)
    if entry is not None:
        out["session"] = entry
    if args.autodetect:
        oc = autodetect_opencode_session()
        if oc:
            out["autodetect"]["opencode"] = oc
        prime = prime_session_dirs()
        if prime:
            out["autodetect"]["prime_session_dirs"] = prime[:5]
    sys.stdout.write(json.dumps(out, ensure_ascii=False, indent=2) + "\n")
    return 0


def cmd_check(args):
    backend = pick_backend(args.backend)
    rc = backend.check(args.provider)
    if rc == 0:
        sys.stderr.write("[pc_msg] ok\n")
    else:
        sys.stderr.write("[pc_msg] not available\n")
    return rc


def cmd_sessions(args):
    data, path = load_sessions(args.config)
    out = {"config": str(path) if path else None, "sessions": data.get("sessions", [])}
    sys.stdout.write(json.dumps(out, ensure_ascii=False, indent=2) + "\n")
    return 0


# --------------------------------------------------------------------------
# CLI
# --------------------------------------------------------------------------


def build_parser():
    p = argparse.ArgumentParser(
        prog=PROG,
        description=__doc__.split("\n")[1],
        formatter_class=argparse.RawDescriptionHelpFormatter,
    )
    p.add_argument("--config", metavar="PATH", help="sessions.json path (default: env PC_MSG_CONFIG, ./sessions.json, ~/.config/pc-msg/sessions.json)")
    p.add_argument("--backend", choices=["auto", "pc-connect", "pc"], default="auto", help="backend to use (default: auto)")
    p.add_argument("--version", action="version", version="{} {}".format(PROG, VERSION))
    sub = p.add_subparsers(dest="command", required=True)

    send = sub.add_parser("send", help="send one message (prints receipt JSON)")
    send.add_argument("--provider", required=True, help="provider id, e.g. telegram")
    send.add_argument("--chat", required=True, help="chat/room id")
    send.add_argument("--text", help="message text")
    send.add_argument("--text-file", metavar="PATH|-", help="read text from file or stdin (-)")

    poll = sub.add_parser("poll", help="receive messages (normalized JSON lines on stdout)")
    poll.add_argument("--timeout", type=float, default=0.0, help="stop after N seconds (0 = run until --once / Ctrl-C)")
    poll.add_argument("--once", action="store_true", help="stop after the first message")
    poll.add_argument("--providers", help="comma-separated provider ids (default: all)")
    poll.add_argument("--json", action="store_true", help="machine mode: suppress stderr hints")

    fwd = sub.add_parser("forward", help="hand one message to the mapped agent session")
    fwd.add_argument("--session", help="session id / agent name from sessions.json")
    fwd.add_argument("--chat", help="chat id to resolve the session")
    fwd.add_argument("--provider", help="provider id (with --chat)")
    fwd.add_argument("--text", help="message text")
    fwd.add_argument("--text-file", metavar="PATH|-", help="read text from file or stdin (-)")

    res = sub.add_parser("resolve", help="show the session mapped to a chat id")
    res.add_argument("--chat", required=True, help="chat/room id")
    res.add_argument("--provider", help="provider id")
    res.add_argument("--autodetect", action="store_true", help="also run session autodetection heuristics")

    check = sub.add_parser("check", help="exit 0 if the stack/provider is available")
    check.add_argument("--provider", help="provider id to check (default: any)")

    sub.add_parser("sessions", help="list configured session mappings")
    return p


def main(argv=None):
    parser = build_parser()
    args = parser.parse_args(argv)
    try:
        if args.command == "send":
            return cmd_send(args)
        if args.command == "poll":
            return cmd_poll(args)
        if args.command == "forward":
            return cmd_forward(args)
        if args.command == "resolve":
            return cmd_resolve(args)
        if args.command == "check":
            return cmd_check(args)
        if args.command == "sessions":
            return cmd_sessions(args)
        parser.error("unknown command {}".format(args.command))
    except PcMsgError as e:
        sys.stderr.write("{}: {}\n".format(PROG, e))
        return e.exit_code
    except KeyboardInterrupt:
        return 130


if __name__ == "__main__":
    sys.exit(main())
