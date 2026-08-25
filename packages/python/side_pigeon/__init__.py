"""side_pigeon — Python binding for provider-ffi cdylib."""

from .ffi import FfiLib, Pc, PcError, find_lib, find_pc, MAX_POLL

__all__ = ["Pc", "FfiLib", "PcError", "find_lib", "find_pc", "MAX_POLL"]
__version__ = "0.1.0"
