"""数据源注册表、唯一优先级链与取数失败分类。

这一层只回答三个问题：本机有哪些源、这次运行该用哪一个、失败属于哪一类。
它不取数、不落盘，也不 import 任何第三方数据包——模块是否可用一律用
``importlib.util.find_spec`` 探测，缺件时抛带 ``pip install`` 建议的错误。

优先级链只有一条（方案 D12）：

    --source > 项目清单 data.source > QIANXING_DATA_SOURCE > 调用方给的默认值

``auto`` 是唯一允许"按固定顺序挑第一个可用"的写法；其余任何一步失败都停下报错，
不静默回落到下一个源。
"""

from __future__ import annotations

import importlib
import importlib.metadata
import importlib.util
import json
import os
from dataclasses import dataclass
from pathlib import Path
from typing import Any, Mapping

DATA_SOURCE_ENV = "QIANXING_DATA_SOURCE"
DATA_CACHE_ENV = "QIANXING_DATA_CACHE"
DATA_RETRIES_ENV = "QIANXING_DATA_RETRIES"

SOURCE_CHAIN_ORIGINS = ("flag", "manifest", "env", "default")
AUTO_SOURCE = "auto"
SAMPLE_SOURCE = "sample"
AUTO_ORDER = ("akshare", "baostock", "easy_tdx", "mootdx")

#: 新环境变量必须同时出现在这张表里，否则它会变成第二套隐式配置（方案 D12 风险项）。
DATA_ENVIRONMENT = (
    (DATA_SOURCE_ENV, "data.source", "默认数据源名：注册表里的名字或 auto"),
    (DATA_CACHE_ENV, "data.cache_root", "内容寻址缓存根目录；--offline 只读它"),
    (DATA_RETRIES_ENV, "data.retries", "网络类失败的最大重试次数（默认 2，0 表示不重试）"),
)

#: 固定失败面（方案 D16）：category → 退出码。同一故障在两个入口使用同一 category。
DATA_ERROR_EXIT_CODES = {
    "invalid-request": 2,
    "credentials": 3,
    "missing-extra": 3,
    "network": 4,
    "partial": 5,
    "unsupported": 6,
    "cache-miss": 7,
    "provider": 8,
}


class AshareProviderError(RuntimeError):
    """数据源不可用、返回结构异常或无法安全标准化时抛出。"""

    category = "provider"
    retryable = False
    suggestion = ""
    source = ""

    def __init__(
        self,
        message: str,
        *,
        category: str | None = None,
        suggestion: str | None = None,
        source: str | None = None,
        retryable: bool | None = None,
    ) -> None:
        super().__init__(message)
        if category is not None:
            self.category = category
        if suggestion is not None:
            self.suggestion = suggestion
        if source is not None:
            self.source = source
        if retryable is not None:
            self.retryable = retryable

    @property
    def exit_code(self) -> int:
        return exit_code_for(self.category)

    def to_json(self) -> str:
        return json.dumps(
            {
                "category": self.category,
                "message": str(self),
                "suggestion": self.suggestion,
                "source": self.source,
                "retryable": self.retryable,
            },
            ensure_ascii=False,
        )


def exit_code_for(category: str) -> int:
    return DATA_ERROR_EXIT_CODES.get(category, DATA_ERROR_EXIT_CODES["provider"])


@dataclass(frozen=True)
class DataSource:
    """注册表里的一行：源名、依赖面、能力位与许可标签。"""

    name: str
    module: str | None
    distribution: str | None
    extra: str | None
    markets: tuple[str, ...]
    frequencies: tuple[str, ...]
    adjustments: tuple[str, ...]
    corporate_actions: bool
    trading_calendar: bool
    credentials: str
    license: str
    builtin: bool
    fetchable: bool
    description: str

    def install_hint(self) -> str:
        if self.extra is None:
            return ""
        return f"pip install 'qianxing[{self.extra}]'"

    def capability_row(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "module": self.module,
            "extra": self.extra,
            "markets": list(self.markets),
            "frequencies": list(self.frequencies),
            "adjustments": list(self.adjustments),
            "corporate_actions": self.corporate_actions,
            "trading_calendar": self.trading_calendar,
            "credentials": self.credentials,
            "license": self.license,
            "builtin": self.builtin,
            "fetchable": self.fetchable,
            "description": self.description,
            "install": self.install_hint(),
        }


SOURCE_REGISTRY: tuple[DataSource, ...] = (
    DataSource(
        name="akshare",
        module="akshare",
        distribution="akshare",
        extra="a-share-akshare",
        markets=("SSE", "SZSE", "BJSE"),
        frequencies=("daily", "weekly", "monthly", "1m", "5m", "15m", "30m", "60m"),
        adjustments=("none", "qfq", "hfq"),
        corporate_actions=True,
        trading_calendar=False,
        credentials="none",
        license="上游发行包许可；行情仅本地研究，不再分发",
        builtin=False,
        fetchable=True,
        description="A 股日线/分钟线与公司行为，接口面最宽",
    ),
    DataSource(
        name="baostock",
        module="baostock",
        distribution="baostock",
        extra="a-share-baostock",
        markets=("SSE", "SZSE", "BJSE"),
        frequencies=("daily", "weekly", "monthly", "5m", "15m", "30m", "60m"),
        adjustments=("none", "qfq", "hfq"),
        corporate_actions=True,
        trading_calendar=False,
        credentials="none",
        license="上游发行包许可；行情仅本地研究，不再分发",
        builtin=False,
        fetchable=True,
        description="证券宝查询接口，复权与分红数据稳定",
    ),
    DataSource(
        name="easy_tdx",
        module="easy_tdx",
        distribution="easy-tdx",
        extra="a-share-easy-tdx",
        markets=("SSE", "SZSE", "BJSE"),
        frequencies=("daily", "weekly", "monthly", "1m", "5m", "15m", "30m", "60m"),
        adjustments=("none", "qfq", "hfq"),
        corporate_actions=True,
        trading_calendar=False,
        credentials="none",
        license="通达信协议读取；无官方 SLA",
        builtin=False,
        fetchable=True,
        description="通达信行情服务器，速度快、字段少",
    ),
    DataSource(
        name="mootdx",
        module="mootdx",
        distribution="mootdx",
        extra="a-share-mootdx",
        markets=("SSE", "SZSE", "BJSE"),
        frequencies=("daily", "weekly", "monthly", "1m", "5m", "15m", "30m", "60m"),
        adjustments=("none",),
        corporate_actions=False,
        trading_calendar=False,
        credentials="none",
        license="通达信协议读取；无官方 SLA",
        builtin=False,
        fetchable=True,
        description="通达信协议的另一实现，只提供不复权行情",
    ),
    DataSource(
        name="ccxt",
        module="ccxt",
        distribution="ccxt",
        extra="ccxt",
        markets=("CRYPTO",),
        frequencies=("1m", "5m", "15m", "30m", "1h", "4h", "1d"),
        adjustments=(),
        corporate_actions=False,
        trading_calendar=False,
        credentials="optional",
        license="CCXT 许可；交易所 API 条款",
        builtin=False,
        fetchable=False,
        description="独立适配面（qianxing.ccxt），不经 data fetch 路由",
    ),
    DataSource(
        name=SAMPLE_SOURCE,
        module=None,
        distribution=None,
        extra=None,
        markets=("SYNTHETIC",),
        frequencies=("daily",),
        adjustments=("none",),
        corporate_actions=False,
        trading_calendar=False,
        credentials="none",
        license="内置合成样例：不是历史行情，不可用于结论",
        builtin=True,
        fetchable=False,
        description="离线合成样例，只用于零配置首跑与门禁",
    ),
)

_BY_NAME = {entry.name: entry for entry in SOURCE_REGISTRY}


def registry() -> tuple[DataSource, ...]:
    return SOURCE_REGISTRY


def source_names() -> tuple[str, ...]:
    return tuple(_BY_NAME)


def source(name: str) -> DataSource:
    normalized = name.strip().lower().replace("-", "_")
    if normalized not in _BY_NAME:
        raise AshareProviderError(
            f"未知数据源 {name!r}；可用：{', '.join(source_names())}",
            category="invalid-request",
            suggestion="qianxing data sources",
        )
    return _BY_NAME[normalized]


def module_available(name: str) -> bool:
    entry = _BY_NAME.get(name)
    if entry is None or entry.module is None:
        return False
    try:
        return importlib.util.find_spec(entry.module) is not None
    except (ImportError, ValueError):
        return False


def provider_version(name: str) -> str | None:
    """已安装时返回版本号；只读元数据，不 import 第三方包。"""
    entry = _BY_NAME.get(name)
    if entry is None or entry.distribution is None:
        return None
    try:
        return importlib.metadata.version(entry.distribution)
    except importlib.metadata.PackageNotFoundError:
        pass
    return "unknown" if module_available(name) else None


def provider_status(name: str) -> dict[str, Any]:
    entry = source(name)
    installed = module_available(name)
    row = entry.capability_row()
    row.update(
        {
            "installed": installed,
            "version": provider_version(name) if installed else None,
            "ready": installed and entry.fetchable,
            "action": "" if installed or entry.extra is None else entry.install_hint(),
        }
    )
    return row


@dataclass(frozen=True)
class SourceResolution:
    """一次运行最终生效的源，以及它是从优先级链哪一环来的。"""

    name: str
    origin: str
    requested: str
    candidates: tuple[str, ...] = ()

    def to_dict(self) -> dict[str, Any]:
        return {
            "name": self.name,
            "origin": self.origin,
            "requested": self.requested,
            "candidates": list(self.candidates),
        }


def _available_candidates() -> tuple[str, ...]:
    return tuple(name for name in AUTO_ORDER if module_available(name))


def resolve_source(
    *,
    explicit: str | None = None,
    manifest: str | None = None,
    environ: Mapping[str, str] | None = None,
    default: str = AUTO_SOURCE,
) -> SourceResolution:
    """按唯一优先级链选出本次运行的源；``auto`` 只在这里解析一次。"""
    environment = os.environ if environ is None else environ
    if explicit not in (None, ""):
        requested, origin = str(explicit), "flag"
    elif manifest not in (None, ""):
        requested, origin = str(manifest), "manifest"
    elif environment.get(DATA_SOURCE_ENV):
        requested, origin = str(environment[DATA_SOURCE_ENV]), "env"
    else:
        requested, origin = default, "default"
    normalized = requested.strip().lower().replace("-", "_")
    if normalized != AUTO_SOURCE:
        return SourceResolution(source(normalized).name, origin, requested)
    candidates = _available_candidates()
    if not candidates:
        hints = "、".join(
            entry.install_hint()
            for entry in SOURCE_REGISTRY
            if entry.extra and entry.fetchable
        )
        raise AshareProviderError(
            "auto 未找到可用 A 股数据源：候选 "
            + "/".join(AUTO_ORDER)
            + " 均未安装",
            category="missing-extra",
            suggestion=f"任选其一安装：{hints}；或用 --source 指定已装源",
        )
    return SourceResolution(candidates[0], origin, requested, candidates)


def _validate_retries(value: Any, *, allow_text: bool = False) -> int:
    if allow_text and isinstance(value, str):
        try:
            value = int(value)
        except ValueError:
            pass
    if isinstance(value, bool) or not isinstance(value, int) or not 0 <= value <= 9:
        raise AshareProviderError(
            "data.retries must be an integer in 0..9", category="invalid-request"
        )
    return value


def resolve_data_config(
    *,
    explicit: str | None = None,
    manifest: Mapping[str, Any] | None = None,
    project_root: Path | None = None,
    cache_directory: Path | str | None = None,
    retries: int | None = None,
    environ: Mapping[str, str] | None = None,
    default: str = SAMPLE_SOURCE,
) -> dict[str, Any]:
    """Resolve the data section without loading providers or creating cache files."""
    from . import cache

    environment = os.environ if environ is None else environ
    if manifest is not None and not isinstance(manifest, Mapping):
        raise AshareProviderError("project data must be an object", category="invalid-request")
    data = manifest if manifest is not None else {}
    resolution = resolve_source(
        explicit=explicit, manifest=data.get("source"), environ=environment, default=default
    )

    def chain(flag: Any, project: Any, env: Any, fallback: Any) -> list[dict[str, Any]]:
        return [
            {"value": value, "source": origin}
            for value, origin in zip((flag, project, env, fallback), SOURCE_CHAIN_ORIGINS)
            if value is not None and value != ""
        ]

    def explain(value: Any, origin: str, entries: list[dict[str, Any]]) -> dict[str, Any]:
        # Lower-priority values are evidence, not inputs to validation/selection.
        # Path and other caller-supplied values must still be JSON serializable.
        overridden = [
            item for item in entries
            if SOURCE_CHAIN_ORIGINS.index(item["source"]) > SOURCE_CHAIN_ORIGINS.index(origin)
        ]
        return {"value": value, "source": origin,
                "overridden": json.loads(json.dumps(overridden, default=str))}

    # Remove only the data-cache override to describe the actual platform default.
    fallback_cache = cache.cache_root({key: value for key, value in environment.items()
                                      if key != DATA_CACHE_ENV})
    cache_chain = chain(cache_directory, data.get("cache_root"),
                        environment.get(DATA_CACHE_ENV), fallback_cache)
    cache_value, cache_origin = cache_chain[0]["value"], cache_chain[0]["source"]
    if not isinstance(cache_value, (str, Path)):
        raise AshareProviderError("data.cache_root must be a path", category="invalid-request")
    root = Path(cache_value).expanduser()
    if not root.is_absolute():
        base = Path(project_root) if cache_origin == "manifest" and project_root is not None else Path.cwd()
        root = base / root
    root = root.absolute()

    retry_chain = chain(retries, data.get("retries"), environment.get(DATA_RETRIES_ENV), 2)
    retry_origin = retry_chain[0]["source"]
    retry_count = _validate_retries(retry_chain[0]["value"], allow_text=retry_origin == "env")
    source_explanation = explain(
        resolution.name, resolution.origin,
        chain(explicit, data.get("source"), environment.get(DATA_SOURCE_ENV), default),
    )
    source_explanation.update(requested=resolution.requested, candidates=list(resolution.candidates))
    return {
        "source": resolution,
        "cache_directory": root,
        "retries": retry_count,
        "explanation": {
            "data.source": source_explanation,
            "data.cache_root": explain(str(root), cache_origin, cache_chain),
            "data.retries": explain(retry_count, retry_origin, retry_chain),
        },
    }


def describe_chain(resolution: SourceResolution) -> dict[str, Any]:
    """给 `plan --explain` / `data doctor` 用的单行解释。"""
    return {
        "data.source": resolution.name,
        "data.source_origin": resolution.origin,
        "data.source_requested": resolution.requested,
    }
