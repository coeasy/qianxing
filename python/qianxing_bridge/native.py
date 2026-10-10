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


#: 应用层用例三条入口（T2-2）。扩展是构建产物，旧世代没有它们——所以"扩展能 import"
#: 与"这个世代有应用层入口"是两件事，探针必须按名字问，不能按 import 成功推断。
APP_ENTRYPOINTS = ("app_validate_dataset", "app_run_backtest", "app_verify_run")


def app_available() -> bool:
    """这个扩展是不是**带应用层用例**的那一代。

    ``available()`` 只回答"能 import"，而构建产物可能比源码旧（例如 `target/release` 里躺着
    上一次构建的 `.pyd`，`python/qianxing_bridge/` 里是新的一份）。按名字问一次，才能让
    "扩展没构建"与"扩展是旧世代"在调用方那里分得开。
    """
    try:
        extension = _extension()
    except RuntimeError:
        return False
    return all(hasattr(extension, name) for name in APP_ENTRYPOINTS)


def frame_digest(payload: str) -> int:
    return int(_extension().frame_digest(payload))


def frame_to_json(payload: str) -> str:
    return str(_extension().frame_to_json(payload))


def app_validate_dataset(spec_json: str) -> str:
    """应用层用例 ``ValidateDataset``：传 ``DatasetSpec`` JSON，换回 ``DatasetVerdict`` JSON。

    与 ``qx-cli app validate-dataset``、``POST /app/validate-dataset`` 调的是同一个
    ``qx-app`` 用例。失败抛 ``_qianxing_native.QxAppError``，异常文本就是 ``AppError`` 的
    JSON 文档（与另两个入口逐字节相同）。
    """
    return str(_extension().app_validate_dataset(spec_json))


def app_run_backtest(spec_json: str) -> str:
    """应用层用例 ``RunBacktest``：传 ``BacktestSpec`` JSON，换回 ``BacktestOutcome`` JSON。

    产物落在 spec 的 ``output_dir`` 下（与 CLI/HTTP 同一份实现、同一份产物形状）。
    """
    return str(_extension().app_run_backtest(spec_json))


def app_verify_run(outcome_json: str) -> str:
    """应用层用例 ``VerifyRun``：传 ``BacktestOutcome`` JSON，换回 ``VerificationResult`` JSON。

    ``outcome_json`` 直接取 ``app_run_backtest`` 的返回值——三个入口的交接面是同一份文档。
    """
    return str(_extension().app_verify_run(outcome_json))


def to_pyarrow_columns(payload: str) -> tuple[Any, ...]:
    import pyarrow as pa

    native = _extension()
    return tuple(
        pa.array(native.owned_arrow_array(payload, column))
        for column in range(6)
    )
