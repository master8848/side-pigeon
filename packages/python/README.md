# side-pigeon (Python)

Python binding for `provider-ffi` cdylib — ctypes fast path (~5µs `poll`) with stdio JSON-RPC fallback.

```python
from side_pigeon import Pc
import json

# ctypes cdylib when available, else spawns `pc` sidecar over NDJSON
with Pc(cfg='{"providers":[{"id":"demo"}]}') as pc:
    pc.send("demo", "demo-room", "hello from python")
    js = pc.poll()           # str JSON or None
    if js:
        print(json.loads(js))

# subscribe filter (like pc_subscribe)
with Pc() as pc:
    pc.subscribe({"provider": "telegram", "channel_id": "123"})
    for js in pc.poll_many():  # drains up to MAX_POLL=1024
        print(js)

# background callback
def on_msg(js: str) -> None:
    print("event:", json.loads(js))

pc = Pc(on_message=on_msg)  # background poll thread (5ms tick, MAX_POLL drain)
# ... work ...
pc.close()
```

## Install

```sh
pip install -e packages/python        # from repo root
# or
pip install side-pigeon
```

No compiled extension required. `ctypes` loads `libprovider_ffi.so` / `.dylib` / `.dll`.

## Library discovery

`find_lib()` order:

1. explicit `lib_path` arg
2. env `PC_FFI_LIB`
3. `libprovider_ffi.so` / `.dylib` / `.dll` in bare, `./`, `target/debug`, `target/release`, `crates/provider-ffi/target/...`, `../target/...`

Build the cdylib:

```sh
cargo build -p provider-ffi
# -> target/debug/libprovider_ffi.so (.dylib on macOS)
```

## stdio fallback

If the cdylib is not found and `use_stdio_fallback=True` (default), `Pc` spawns the `pc` sidecar via subprocess over NDJSON JSON-RPC (like `plugins/agent-skill/pc_msg.py`):

```python
pc = Pc(use_stdio_fallback=True)   # default: try ffi, fallback to stdio
pc = Pc(use_stdio_fallback=False)  # raise FileNotFoundError if cdylib missing
```

Sidecar discovery for fallback: `PC_BIN` env, `which pc`, `target/release/pc`, `target/debug/pc`.

`MAX_POLL=1024` matches `crates/provider-ffi/src/lib.rs` VecDeque cap and `packages/core/src/ffi.ts`.
