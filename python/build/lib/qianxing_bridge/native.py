"""Optional Rust/PyO3 acceleration and Arrow C Data Interface bridge.

The pure-Python JSON implementation remains the portable fallback.  When the
``_qianxing_native`` extension is installed, Arrow columns are imported through
the standard ``__arrow_c_array__`` protocol and retain Rust-owned release
semantics until pyarrow consumes them.
"""

from __future__ import annotations

from importlib import import_module
from typing import Any


def _extension() -> Any:
    errors: list[ImportError] = []
    for module_name in ("_qianxing_native", "qianxing_bridge._qianxing_native"):
        try:
            return import_module(module_name)
        except ImportError as error:
            errors.append(error)
    raise RuntimeError(
        "_qianxing_native is not installed; build the qx-python wheel first"
    ) from errors[-1]


def available() -> bool:
    try:
        _extension()
    except RuntimeError:
        return False
    return True


def frame_digest(payload: str) -> int:
    return int(_extension().frame_digest(payload))


def frame_to_json(payload: str) -> str:
    return str(_extension().frame_to_json(payload))


def to_pyarrow_columns(payload: str) -> tuple[Any, ...]:
    import pyarrow as pa

    native = _extension()
    return tuple(
        pa.array(native.owned_arrow_array(payload, column))
        for column in range(6)
    )
