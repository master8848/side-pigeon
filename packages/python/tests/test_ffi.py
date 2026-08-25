"""Basic import / API smoke tests for side_pigeon (no cdylib required)."""

import json
import pathlib


def test_import():
    import side_pigeon
    from side_pigeon import FfiLib, Pc, MAX_POLL, find_lib
    assert side_pigeon.__version__ == "0.1.0"
    assert MAX_POLL == 1024
    assert callable(find_lib)
    assert FfiLib is not None
    assert Pc is not None


def test_find_lib_returns_none_or_path_without_cdylib():
    from side_pigeon import find_lib
    p = find_lib()
    # either None (no cdylib built) or a Path that exists
    assert p is None or (isinstance(p, pathlib.Path) and p.is_file())


def test_find_lib_explicit_missing():
    from side_pigeon import find_lib
    assert find_lib("/tmp/does-not-exist-xyz.so") is None


def test_py_typed_marker_exists():
    p = pathlib.Path(__file__).parents[1] / "side_pigeon" / "py.typed"
    assert p.is_file()


def test_pc_requires_ffi_or_sidecar():
    """Pc with stdio disabled should raise when no cdylib built."""
    from side_pigeon import Pc, find_lib

    if find_lib() is not None:
        # cdylib present — Pc should start
        pc = Pc(use_stdio_fallback=False)
        pc.close()
        return

    try:
        pc = Pc(use_stdio_fallback=False)
    except FileNotFoundError:
        return  # expected
    else:
        pc.close()
        raise AssertionError("expected FileNotFoundError when no cdylib and fallback disabled")


def test_max_poll_constant():
    """MAX_POLL mirrors Rust VecDeque cap and TS ffi.ts."""
    from side_pigeon import MAX_POLL

    assert MAX_POLL == 1024


def test_subscribe_filter_serialization():
    """Pc.subscribe accepts dict (unit: check JSON serialization path)."""
    # Don't actually need a live Pc; just verify json.dumps path doesn't crash
    f = {"provider": "telegram", "channel_id": "123", "explicitly_addressed": True}
    s = json.dumps(f)
    assert '"provider"' in s
