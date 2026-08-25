"""Python binding for provider-ffi cdylib via ctypes with stdio JSON-RPC fallback.

C ABI (crates/provider-ffi/src/lib.rs):

    pc_init(cfg_json:*const c_char) -> *mut PcHandle
    pc_poll(*mut PcHandle) -> *mut c_char   // heap-allocated JSON, free with pc_free_string
    pc_send(*mut PcHandle, provider, chat, text) -> i32  // 0 ok, -1 err
    pc_subscribe(*mut PcHandle, filter_json) -> i32       // 0 ok, -1 err, null/empty = no-op
    pc_free(*mut PcHandle)
    pc_free_string(*mut c_char)

ctypes poll ~5us; stdio fallback spawns `pc` sidecar over NDJSON JSON-RPC.
"""

from __future__ import annotations

import ctypes
import json
import os
import platform
import queue
import shutil
import subprocess
import sys
import threading
import time
from pathlib import Path
from typing import Callable, Optional, Union

__all__ = ["MAX_POLL", "find_lib", "find_pc", "FfiLib", "Pc", "PcError"]

MAX_POLL = 1024


class PcError(RuntimeError):
    """Raised for ffi/send/subscribe/init failures."""


# ---------------------------------------------------------------------------
# Library / binary discovery
# ---------------------------------------------------------------------------

def _lib_name() -> str:
    s = sys.platform
    if s == "win32":
        return "provider_ffi.dll"
    if s == "darwin":
        return "libprovider_ffi.dylib"
    return "libprovider_ffi.so"


def _repo_root(start: Optional[Path] = None) -> Optional[Path]:
    here = (start or Path(__file__).resolve()).parent
    for p in [here] + list(here.parents):
        if (p / "Cargo.toml").is_file() and (p / "crates").is_dir():
            return p
        # also handle being under packages/python/side_pigeon -> parents[3] is repo root
        # fallback: check two-three levels up for Cargo.toml existence
    # fallback: cwd
    cwd = Path.cwd()
    for p in [cwd] + list(cwd.parents):
        if (p / "Cargo.toml").is_file() and (p / "crates").is_dir():
            return p
    return None


def find_lib(lib_path: Union[str, Path, None] = None) -> Optional[Path]:
    """Locate libprovider_ffi cdylib.

    Order:
      1. explicit ``lib_path`` argument
      2. env ``PC_FFI_LIB``
      3. platform default names: libprovider_ffi.so / .dylib / .dll
         searched in: bare, ./, target/debug, target/release,
         crates/provider-ffi/target/..., ../target/...

    Returns Path if found on disk, else None.
    """
    if lib_path is not None:
        p = Path(lib_path)
        if p.is_file():
            return p
        # explicit path given but not found -> not found
        return None

    env = os.environ.get("PC_FFI_LIB", "").strip()
    candidates: list[Path] = []
    if env:
        candidates.append(Path(env))

    base = _lib_name()
    # relative / bare candidates (Path will be relative to cwd)
    rels = [
        base,
        f"./{base}",
        f"target/debug/{base}",
        f"target/debug/{base}",  # intentional duplicate for clarity
        f"target/debug/{base}".replace("target", "target"),
        f"target/release/{base}",
        f"./target/debug/{base}",
        f"./target/release/{base}",
        f"crates/provider-ffi/target/debug/{base}",
        f"crates/provider-ffi/target/release/{base}",
        f"../target/debug/{base}",
        f"../target/release/{base}",
    ]
    # dedup while preserving order
    seen: set[str] = set()
    uniq_rels: list[str] = []
    for r in rels:
        if r not in seen:
            seen.add(r)
            uniq_rels.append(r)

    # also try relative to repo root if we can find it
    root = _repo_root()
    if root is not None:
        for r in uniq_rels:
            # r may be absolute or relative; handle accordingly
            if os.path.isabs(r):
                candidates.append(Path(r))
            else:
                # bare base name -> try repo root + base and cwd + r
                # we handle bare names separately via filesystem search
                candidates.append(root / r)
                candidates.append(Path.cwd() / r)
                candidates.append(Path(r))
    else:
        for r in uniq_rels:
            candidates.append(Path(r))

    # also try bare name via filesystem check (will only match if file exists in cwd)
    for c in candidates:
        try:
            if c.is_file():
                return c
        except Exception:
            continue

    # last resort: check target dirs relative to this file's repo root explicitly
    if root is not None:
        for sub in [f"target/debug/{base}", f"target/release/{base}"]:
            q = root / sub
            if q.is_file():
                return q

    return None


def find_pc(bin_path: Union[str, Path, None] = None) -> Optional[str]:
    """Locate ``pc`` JSON-RPC sidecar binary for stdio fallback.

    Order: explicit ``bin_path``, env ``PC_BIN``, ``which pc``, repo
    ``target/{release,debug}/pc``.
    """
    if bin_path is not None:
        p = Path(bin_path)
        if p.is_file():
            return str(p)
        return None
    env = os.environ.get("PC_BIN", "").strip()
    if env:
        return env
    w = shutil.which("pc")
    if w:
        return w
    root = _repo_root()
    if root is not None:
        for rel in ("target/release/pc", "target/debug/pc"):
            cand = root / rel
            if cand.is_file():
                return str(cand)
    return None


# ---------------------------------------------------------------------------
# ctypes wrapper
# ---------------------------------------------------------------------------

class FfiLib:
    """ctypes wrapper around libprovider_ffi cdylib (lazy-loading).

    Methods mirror the C ABI and handle null / free_string correctly.
    Errors raise :class:`PcError` where appropriate (init); poll returns
    ``str | None`` with free_string called internally; send/subscribe return
    ``int`` (0 ok, -1 err) and high-level :class:`Pc` converts to exceptions.
    """

    def __init__(self, lib_path: Union[str, Path, None] = None) -> None:
        resolved = find_lib(lib_path)
        if resolved is None:
            # allow explicit lib_path string to be tried as dlopen name (system path)
            if lib_path is not None:
                resolved = Path(str(lib_path))
            elif os.environ.get("PC_FFI_LIB"):
                resolved = Path(os.environ["PC_FFI_LIB"])
            else:
                raise FileNotFoundError(
                    "libprovider_ffi not found (tried env PC_FFI_LIB, "
                    f"{_lib_name()}, target/debug, target/release, ./). "
                    "Build with `cargo build -p provider-ffi` or set PC_FFI_LIB."
                )
        # If resolved exists on disk, use it; otherwise let CDLL search system paths
        load_path = str(resolved)
        # On Windows, use WinDLL? CDLL works for cdecl; provider-ffi uses C ABI (cdecl)
        try:
            self._lib = ctypes.CDLL(load_path)
        except OSError as e:
            raise FileNotFoundError(f"failed to load {load_path}: {e}") from e

        # Configure signatures. Use c_void_p for opaque handles / string ptrs
        # so we can check null before decoding.
        try:
            self._lib.pc_init.argtypes = [ctypes.c_char_p]
            self._lib.pc_init.restype = ctypes.c_void_p
            self._lib.pc_poll.argtypes = [ctypes.c_void_p]
            self._lib.pc_poll.restype = ctypes.c_void_p
            self._lib.pc_send.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_char_p, ctypes.c_char_p]
            self._lib.pc_send.restype = ctypes.c_int
            self._lib.pc_subscribe.argtypes = [ctypes.c_void_p, ctypes.c_char_p]
            self._lib.pc_subscribe.restype = ctypes.c_int
            self._lib.pc_free.argtypes = [ctypes.c_void_p]
            self._lib.pc_free.restype = None
            self._lib.pc_free_string.argtypes = [ctypes.c_void_p]
            self._lib.pc_free_string.restype = None
        except AttributeError as e:
            raise PcError(f"cdylib missing expected symbol: {e}") from e

        self.path: Path = Path(load_path)

    # -- high-level wrappers ------------------------------------------------

    def init(self, cfg_json: Optional[str] = None) -> int:
        """Call pc_init. Returns handle as int (c_void_p). Raises PcError on null."""
        arg = cfg_json.encode("utf-8") if cfg_json is not None else None
        handle = self._lib.pc_init(arg)
        if not handle:
            raise PcError("pc_init returned null")
        return int(handle)

    def poll(self, handle: int) -> Optional[str]:
        """Call pc_poll. Returns JSON str or None. Always frees C string."""
        if not handle:
            return None
        ptr = self._lib.pc_poll(ctypes.c_void_p(handle))
        if not ptr:
            return None
        try:
            # string_at reads NUL-terminated bytes
            raw = ctypes.string_at(ptr)
            return raw.decode("utf-8")
        finally:
            try:
                self._lib.pc_free_string(ctypes.c_void_p(ptr))
            except Exception:
                pass

    def send(self, handle: int, provider: str, chat: str, text: str) -> int:
        """Call pc_send. Returns 0 ok, -1 err."""
        if not handle:
            return -1
        return int(
            self._lib.pc_send(
                ctypes.c_void_p(handle),
                provider.encode("utf-8"),
                chat.encode("utf-8"),
                text.encode("utf-8"),
            )
        )

    def subscribe(self, handle: int, filter_json: Optional[str] = None) -> int:
        """Call pc_subscribe. filter_json may be None/empty (no-op). Returns 0/-1."""
        if not handle:
            return -1
        arg = filter_json.encode("utf-8") if filter_json is not None else None
        return int(self._lib.pc_subscribe(ctypes.c_void_p(handle), arg))

    def free(self, handle: int) -> None:
        """Call pc_free (no-op on null/0)."""
        if not handle:
            return
        try:
            self._lib.pc_free(ctypes.c_void_p(handle))
        except Exception:
            pass

    def free_string(self, ptr: int) -> None:
        """Call pc_free_string (no-op on null/0). Exposed for completeness."""
        if not ptr:
            return
        try:
            self._lib.pc_free_string(ctypes.c_void_p(ptr))
        except Exception:
            pass

    def close(self) -> None:
        """Unload hook (no-op for ctypes; present for symmetry)."""
        # ctypes has no dlclose wrapper reliably across platforms; keep no-op.
        pass


# ---------------------------------------------------------------------------
# stdio fallback (minimal JSON-RPC 2.0 over NDJSON)
# ---------------------------------------------------------------------------

class _RpcClient:
    """Minimal JSON-RPC 2.0 client over child stdio (NDJSON framing).

    Mirrors plugins/agent-skill/pc_msg.py RpcClient but kept minimal for the
    Python binding. Notifications go to an event queue; responses matched by id.
    """

    def __init__(self, proc: subprocess.Popen) -> None:
        self.proc = proc
        self._next_id = 1
        self._cond = threading.Condition()
        self._responses: dict[int, Optional[dict]] = {}
        self._events: queue.Queue[Optional[dict]] = queue.Queue()
        self._eof = False
        self._reader = threading.Thread(target=self._read_loop, daemon=True)
        self._reader.start()

    def _read_loop(self) -> None:
        try:
            assert self.proc.stdout is not None
            for raw in iter(self.proc.stdout.readline, b""):
                line = raw.strip()
                if not line:
                    continue
                try:
                    obj = json.loads(line.decode("utf-8", errors="replace"))
                except Exception:
                    continue
                if not isinstance(obj, dict):
                    continue
                if obj.get("method"):
                    self._events.put(obj)
                    continue
                if "id" in obj:
                    with self._cond:
                        self._responses[int(obj["id"])] = obj
                        self._cond.notify_all()
        finally:
            self._events.put(None)  # EOF sentinel
            with self._cond:
                self._eof = True
                self._cond.notify_all()

    def request(self, method: str, params=None, timeout: float = 15.0):
        rid = self._next_id
        self._next_id += 1
        frame: dict = {"jsonrpc": "2.0", "id": rid, "method": method}
        if params is not None:
            frame["params"] = params
        with self._cond:
            self._responses[rid] = None
            try:
                assert self.proc.stdin is not None
                self.proc.stdin.write((json.dumps(frame) + "\n").encode("utf-8"))
                self.proc.stdin.flush()
            except OSError as e:
                self._responses.pop(rid, None)
                raise PcError(f"failed to write {method} request to pc: {e}") from e
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
                        raise PcError(f"pc sidecar exited (rc={rc}) before responding to {method}")
                    raise PcError(f"timeout waiting for {method} response from pc")
                self._cond.wait(remaining)
            resp = self._responses.pop(rid, None)
        if resp is None:
            rc = None
            try:
                rc = self.proc.poll()
            except Exception:
                rc = None
            raise PcError(f"pc sidecar exited (rc={rc}) before responding to {method}")
        if resp.get("error"):
            err = resp["error"]
            raise PcError(f"pc {method} failed: {err.get('code')} {err.get('message')}")
        return resp.get("result")

    def next_event(self, timeout: Optional[float] = None):
        """Next notification dict; None on EOF; ('timeout', None) on deadline."""
        if self._eof and self._events.empty():
            return None
        try:
            ev = self._events.get(timeout=timeout)
        except queue.Empty:
            return ("timeout", None)
        return ev


class _StdioFallback:
    """Minimal stdio transport that mimics FfiLib handle semantics.

    Spawns ``pc`` binary and speaks JSON-RPC 2.0 NDJSON. Provides:
      init -> handle (int, dummy 1)
      poll(handle) -> str | None  (JSON of ChannelMessage)
      send(handle, provider, chat, text) -> int
      subscribe(handle, filter_json) -> int
      free(handle)
    """

    def __init__(self, cfg_json: Optional[str] = None, bin_path: Optional[str] = None) -> None:
        bin_str = find_pc(bin_path)
        if not bin_str:
            raise FileNotFoundError(
                "pc sidecar binary not found for stdio fallback (tried env PC_BIN, "
                "which pc, target/release/pc, target/debug/pc). "
                "Build with `cargo build -p provider-ffi` or set PC_BIN."
            )
        env = dict(os.environ)
        # cfg_json for sidecar is passed via env or not needed; pc sidecar uses PC_CONFIG
        # Keep compatible with pc_msg.py: env PC_CONFIG
        if cfg_json:
            env["PC_CONFIG"] = cfg_json

        try:
            self.proc = subprocess.Popen(
                [bin_str],
                stdin=subprocess.PIPE,
                stdout=subprocess.PIPE,
                stderr=subprocess.DEVNULL,
                env=env,
                bufsize=0,
            )
        except OSError as e:
            raise PcError(f"failed to spawn pc sidecar {bin_str}: {e}") from e

        self.client = _RpcClient(self.proc)
        self._handle: int = 1
        self._closed = False
        # Track pending messages that arrived as notifications but haven't been polled
        self._pending: queue.Queue[str] = queue.Queue()

        # Initialize sidecar (best-effort)
        try:
            self.client.request("initialize", None, timeout=15.0)
        except Exception:
            # some sidecar versions may not require initialize
            pass
        try:
            self.client.request("listen", None, timeout=15.0)
        except Exception:
            pass

        # Background drain of notifications into _pending
        self._drain_thread = threading.Thread(target=self._drain_events, daemon=True)
        self._drain_thread.start()

    def _drain_events(self) -> None:
        while not self._closed:
            ev = self.client.next_event(timeout=0.1)
            if ev is None:
                break
            if ev == ("timeout", None):
                continue
            if not isinstance(ev, dict):
                continue
            method = ev.get("method")
            params = ev.get("params") or {}
            if method == "event.message":
                msg = params.get("message")
                if msg is not None:
                    try:
                        self._pending.put(json.dumps(msg, ensure_ascii=False))
                    except Exception:
                        self._pending.put(json.dumps({"message": msg}, ensure_ascii=False))
            elif method == "event.error":
                # surface as JSON with error field for poll caller to inspect
                try:
                    self._pending.put(json.dumps({"error": params}, ensure_ascii=False))
                except Exception:
                    pass
            else:
                # other events: enqueue raw params as JSON
                try:
                    self._pending.put(json.dumps(params, ensure_ascii=False))
                except Exception:
                    pass

    # -- FfiLib-compatible surface ----------------------------------------

    def init(self, cfg_json: Optional[str] = None) -> int:  # noqa: ARG002
        return self._handle

    def poll(self, handle: int) -> Optional[str]:  # noqa: ARG002
        try:
            return self._pending.get_nowait()
        except queue.Empty:
            return None

    def send(self, handle: int, provider: str, chat: str, text: str) -> int:  # noqa: ARG002
        try:
            self.client.request(
                "send",
                {"provider": provider, "message": {"channel_id": chat, "text": text}},
                timeout=30.0,
            )
            return 0
        except PcError:
            return -1
        except Exception:
            return -1

    def subscribe(self, handle: int, filter_json: Optional[str] = None) -> int:  # noqa: ARG002
        # Sidecar filtering is done via listen params; for minimal fallback we
        # accept any filter and filter client-side if needed. Treat as success.
        if filter_json is not None and filter_json.strip():
            try:
                json.loads(filter_json)
            except json.JSONDecodeError:
                return -1
        return 0

    def free(self, handle: int) -> None:  # noqa: ARG002
        self.close()

    def free_string(self, ptr: int) -> None:  # noqa: ARG002
        pass

    def close(self) -> None:
        if self._closed:
            return
        self._closed = True
        try:
            self.client.request("shutdown", None, timeout=2.0)
        except Exception:
            pass
        try:
            if self.proc.stdin is not None:
                self.proc.stdin.close()
        except Exception:
            pass
        try:
            self.proc.wait(timeout=5)
        except Exception:
            try:
                self.proc.terminate()
            except Exception:
                pass


# ---------------------------------------------------------------------------
# High-level Pc
# ---------------------------------------------------------------------------

class Pc:
    """High-level handle hiding FFI vs stdio transport.

    Example:
        pc = Pc(cfg='{"providers":[{"id":"demo"}]}')
        pc.send("demo", "demo-room", "hello")
        msg = pc.poll()  # str JSON or None
        pc.close()

        # context manager
        with Pc() as pc:
            pc.subscribe('{"provider":"telegram"}')
            for js in pc.poll_many():
                print(js)

        # background callback
        def on_msg(js: str) -> None:
            print(json.loads(js))
        pc = Pc(on_message=on_msg)
        time.sleep(1)
        pc.close()

    Args:
        cfg: optional SidecarConfig JSON (str or dict). None uses env/defaults.
        lib_path: optional explicit path to libprovider_ffi cdylib.
        use_stdio_fallback: if True (default) spawn ``pc`` sidecar when cdylib
            not found; if False raise FileNotFoundError instead.
        on_message: optional callback ``(json_str: str) -> None``; when set a
            background poll thread drains up to MAX_POLL per tick.
        poll_interval: seconds between background poll ticks (default 0.005).
    """

    def __init__(
        self,
        cfg: Union[str, dict, None] = None,
        lib_path: Union[str, Path, None] = None,
        use_stdio_fallback: bool = True,
        on_message: Optional[Callable[[str], None]] = None,
        poll_interval: float = 0.005,
    ) -> None:
        self._closed = False
        self._ffi: Optional[FfiLib] = None
        self._stdio: Optional[_StdioFallback] = None
        self._handle: Optional[int] = None
        self._on_message = on_message
        self._poll_interval = poll_interval
        self._stop = threading.Event()
        self._poll_thread: Optional[threading.Thread] = None

        if isinstance(cfg, dict):
            cfg_json: Optional[str] = json.dumps(cfg)
        elif isinstance(cfg, str):
            cfg_json = cfg
        else:
            cfg_json = None

        # Try FFI first
        ffi_err: Optional[Exception] = None
        try:
            # Only try FFI if lib is discoverable; FfiLib raises if not found
            probe = find_lib(lib_path)
            if probe is not None or lib_path is not None or os.environ.get("PC_FFI_LIB"):
                # attempt load (find_lib may be None when lib_path is explicit name on system path)
                # Try construct FfiLib anyway if env/explicit requested
                try:
                    self._ffi = FfiLib(lib_path)
                    self._handle = self._ffi.init(cfg_json)
                except FileNotFoundError as e:
                    ffi_err = e
                    self._ffi = None
                    self._handle = None
                except PcError as e:
                    ffi_err = e
                    self._ffi = None
                    self._handle = None
            else:
                # No lib on disk; try anyway only if we want to try system dlopen
                # Attempt FfiLib only if load might succeed via system library path
                # We skip to fallback to avoid noisy error when lib not built
                ffi_err = FileNotFoundError("libprovider_ffi not found on disk")
        except Exception as e:  # noqa: BLE001
            ffi_err = e
            self._ffi = None
            self._handle = None

        if self._handle is None:
            if use_stdio_fallback:
                try:
                    self._stdio = _StdioFallback(cfg_json)
                    self._handle = self._stdio.init(cfg_json)
                except FileNotFoundError as e:
                    # Neither FFI nor stdio available
                    raise FileNotFoundError(
                        f"neither libprovider_ffi nor pc sidecar found: ffi={ffi_err!r} stdio={e!r}. "
                        "Build with `cargo build -p provider-ffi` / `cargo build --bin pc` "
                        "or set PC_FFI_LIB / PC_BIN."
                    ) from e
            else:
                raise FileNotFoundError(f"libprovider_ffi not found and stdio fallback disabled: {ffi_err!r}") from ffi_err

        if on_message is not None:
            self._start_background()

    # -- background poll thread -------------------------------------------

    def _start_background(self) -> None:
        if self._poll_thread is not None:
            return
        self._stop.clear()
        self._poll_thread = threading.Thread(target=self._poll_loop, daemon=True)
        self._poll_thread.start()

    def _poll_loop(self) -> None:
        while not self._stop.is_set() and not self._closed:
            try:
                # Drain up to MAX_POLL per tick, matching TS ffi.ts drain loop
                for _ in range(MAX_POLL):
                    js = self.poll()
                    if js is None:
                        break
                    try:
                        if self._on_message is not None:
                            self._on_message(js)
                    except Exception:
                        # never kill poll thread on callback error
                        pass
            except Exception:
                pass
            # wait with early wake on stop
            self._stop.wait(self._poll_interval)

    # -- public API -------------------------------------------------------

    def poll(self) -> Optional[str]:
        """Poll one pending event as JSON string, or None if empty.

        Handles null returns, always frees C string (via FfiLib), and for
        stdio fallback dequeues from the JSON-RPC notification queue.
        """
        if self._closed:
            return None
        if self._stdio is not None:
            assert self._handle is not None
            return self._stdio.poll(self._handle)
        if self._ffi is not None and self._handle is not None:
            return self._ffi.poll(self._handle)
        return None

    def poll_many(self, max_items: int = MAX_POLL) -> list[str]:
        """Drain up to ``max_items`` (default MAX_POLL=1024) pending events.

        Mirrors the MAX_POLL drain in provider-ffi (VecDeque cap 1024) and
        packages/core/src/ffi.ts drain loop.
        """
        out: list[str] = []
        cap = min(max(max_items, 0), MAX_POLL)
        for _ in range(cap):
            js = self.poll()
            if js is None:
                break
            out.append(js)
        return out

    def send(self, provider: str, chat: str, text: str) -> int:
        """Send text via ``provider`` to ``chat``. Returns 0 ok, raises on error.

        Mirrors ``pc_send`` (0 ok, -1 err) but raises :class:`PcError` instead
        of returning -1.
        """
        if self._closed:
            raise PcError("Pc closed")
        rc: int
        if self._stdio is not None:
            assert self._handle is not None
            rc = self._stdio.send(self._handle, provider, chat, text)
        elif self._ffi is not None and self._handle is not None:
            rc = self._ffi.send(self._handle, provider, chat, text)
        else:
            raise PcError("no handle")
        if rc != 0:
            raise PcError(f"pc_send failed rc={rc} provider={provider} chat={chat}")
        return rc

    def subscribe(self, filter: Union[str, dict, None] = None) -> int:  # noqa: A002
        """Subscribe with optional JSON filter.

        ``filter`` may be a JSON string or dict like
        ``{"provider":"telegram","channel_id":"123","explicitly_addressed":true}``.
        Returns 0 ok, raises :class:`PcError` on invalid filter/handle.
        Null/empty filter is a no-op (returns 0).
        """
        if self._closed:
            raise PcError("Pc closed")
        if filter is None:
            filter_json: Optional[str] = None
        elif isinstance(filter, dict):
            filter_json = json.dumps(filter)
        else:
            filter_json = str(filter)
            if filter_json.strip() == "":
                filter_json = None

        rc: int
        if self._stdio is not None:
            assert self._handle is not None
            rc = self._stdio.subscribe(self._handle, filter_json)
        elif self._ffi is not None and self._handle is not None:
            rc = self._ffi.subscribe(self._handle, filter_json)
        else:
            raise PcError("no handle")
        if rc != 0:
            raise PcError(f"pc_subscribe failed rc={rc} filter={filter_json!r}")
        return rc

    def close(self) -> None:
        """Close handle and stop background thread. Idempotent."""
        if self._closed:
            return
        self._closed = True
        self._stop.set()
        if self._poll_thread is not None:
            try:
                self._poll_thread.join(timeout=2.0)
            except Exception:
                pass
            self._poll_thread = None
        if self._stdio is not None:
            try:
                self._stdio.close()
            except Exception:
                pass
            self._stdio = None
        if self._ffi is not None and self._handle is not None:
            try:
                self._ffi.free(self._handle)
            except Exception:
                pass
        self._handle = None
        self._ffi = None

    # -- context manager --------------------------------------------------

    def __enter__(self) -> "Pc":
        return self

    def __exit__(self, exc_type, exc, tb) -> None:  # noqa: ANN001
        self.close()

    def __del__(self) -> None:
        try:
            self.close()
        except Exception:
            pass
