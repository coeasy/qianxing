"""A 股数据源适配与标准化边界。

这个模块只负责三件事：把不同数据源的字段统一为 :class:`BarFrame`，保留
可审计的来源清单，以及对已经落盘的 BarFrame 做快速筛选。数据源依赖均为
可选依赖，核心包和离线测试不需要安装 AkShare、Baostock 或 easy_tdx。
"""

from __future__ import annotations

import importlib
import importlib.util
import hashlib
import json
from dataclasses import asdict, dataclass, replace
from datetime import date, datetime, time, timezone
from decimal import Decimal, InvalidOperation, ROUND_HALF_EVEN
from pathlib import Path
from typing import Any, Iterable, Mapping, Protocol, Sequence
from zoneinfo import ZoneInfo

from qianxing_bridge import BarFrame


SCALE = 1_000_000_000
_SHANGHAI = ZoneInfo("Asia/Shanghai")
_ADJUSTMENTS = {"none", "qfq", "hfq"}
_FREQUENCIES = {"daily", "weekly", "monthly", "1m", "5m", "15m", "30m", "60m"}
_CODE_REPLACEMENTS = str.maketrans({"．": ".", "　": " "})


class AshareProviderError(RuntimeError):
    """数据源不可用、返回结构异常或无法安全标准化时抛出。"""


_CORPORATE_ACTION_TYPES = {
    "cash_dividend",
    "bonus_share",
    "capital_transfer",
    "rights_issue",
    "rights_issue_expiry",
    "new_share_issue",
    "repurchase",
    "convertible_bond_issue",
    "convertible_bond_interest",
    "convertible_bond_redemption",
    "convertible_bond_call",
    "convertible_bond_put",
    "convertible_bond_conversion",
    "suspension",
    "capital_change",
    "unknown",
}


def _json_safe(value: Any) -> Any:
    """保留原始字段时将 DataFrame/Decimal 等值转换成稳定 JSON 值。"""

    if value is None or isinstance(value, (str, int, float, bool)):
        return value
    if isinstance(value, (date, datetime)):
        return value.isoformat()
    if isinstance(value, Decimal):
        return str(value)
    return str(value)


def _optional_pick(row: Mapping[str, Any], *aliases: str) -> Any:
    normalized = {str(key).strip().lower(): value for key, value in row.items()}
    for alias in aliases:
        if alias in row:
            return row[alias]
        if alias.lower() in normalized:
            return normalized[alias.lower()]
    return None


def _date_text(value: Any) -> str | None:
    if value is None or str(value).strip() in {"", "-", "--", "nan", "NaN", "None"}:
        return None
    if isinstance(value, datetime):
        return value.date().isoformat()
    if isinstance(value, date):
        return value.isoformat()
    text = str(value).strip().replace("/", "-").replace("年", "-").replace("月", "-").replace("日", "")
    if " " in text:
        text = text.split(" ", 1)[0]
    if len(text) == 8 and text.isdigit():
        text = f"{text[:4]}-{text[4:6]}-{text[6:]}"
    try:
        return date.fromisoformat(text).isoformat()
    except ValueError:
        return None


def _nonnegative_scaled(value: Any, field: str) -> int:
    if value is None or str(value).strip() in {"", "-", "--", "nan", "NaN", "None"}:
        return 0
    try:
        number = Decimal(str(value).replace(",", "").strip())
    except (InvalidOperation, ValueError) as exc:
        raise AshareProviderError(f"invalid {field} value: {value!r}") from exc
    if not number.is_finite() or number < 0:
        raise AshareProviderError(f"invalid {field} value: {value!r}")
    return int((number * SCALE).quantize(Decimal("1"), rounding=ROUND_HALF_EVEN))


def _nonnegative_raw(value: Any, field: str) -> int:
    """解析已经处于 Qianxing 定点单位的非负整数。"""

    if value is None or str(value).strip() in {"", "-", "--", "nan", "NaN", "None"}:
        return 0
    try:
        number = Decimal(str(value).replace(",", "").strip())
    except (InvalidOperation, ValueError) as exc:
        raise AshareProviderError(f"invalid {field} value: {value!r}") from exc
    if not number.is_finite() or number < 0 or number != number.to_integral_value():
        raise AshareProviderError(f"invalid {field} value: {value!r}")
    return int(number)


def _ratio_scaled(value: Any, field: str) -> tuple[int, int]:
    """将“每 10 股 X 股”或“每股 X 股”统一为 X/1。"""

    if value is None or str(value).strip() in {"", "-", "--", "nan", "NaN", "None"}:
        return 0, 1
    try:
        number = Decimal(str(value).replace(",", "").strip())
    except (InvalidOperation, ValueError) as exc:
        raise AshareProviderError(f"invalid {field} value: {value!r}") from exc
    if not number.is_finite() or number < 0:
        raise AshareProviderError(f"invalid {field} value: {value!r}")
    # Baostock/TDX 常见字段已经是“每股”或“每十股”，调用方可通过
    # *_per_ten 字段显式指定分母；普通字段按每股处理，避免隐式猜测。
    scaled = int((number * SCALE).quantize(Decimal("1"), rounding=ROUND_HALF_EVEN))
    return scaled, SCALE


def _canonical_action_type(row: Mapping[str, Any]) -> str:
    value = _optional_pick(row, "action_type", "type", "类别", "分红类型", "变动类型", "事件类型")
    text = str(value or "").strip().lower()
    if "除权除息" in text:
        # 该中文描述在不同数据源中既可能表示股本变更，也可能只是
        # 分红/送转的统称；只有明确给出股本快照时才进入 CapitalChange。
        if _optional_pick(
            row,
            "issuer_total_shares_raw",
            "issuer_total_shares",
            "total_shares",
            "总股本",
            "总股本数",
        ) is not None:
            return "capital_change"
        if _optional_pick(
            row,
            "cash_dividend_raw",
            "派息",
            "现金分红",
            "每股派息",
            "fenhong",
            "cash_dividend",
        ) is not None:
            return "cash_dividend"
        if _optional_pick(row, "送股", "送股比例", "bonus_share", "bonus_ratio", "songzhuangu") is not None:
            return "bonus_share"
        if _optional_pick(row, "转增", "转增比例", "capital_transfer", "transfer_ratio") is not None:
            return "capital_transfer"
        return "unknown"
    mapping = (
        ("convertible_bond_interest", "convertible_bond_interest"),
        ("bond_interest", "convertible_bond_interest"),
        ("可转债付息", "convertible_bond_interest"),
        ("可转债利息", "convertible_bond_interest"),
        ("付息", "convertible_bond_interest"),
        ("convertible_bond_call", "convertible_bond_call"),
        ("bond_call", "convertible_bond_call"),
        ("可转债强赎", "convertible_bond_call"),
        ("强制赎回", "convertible_bond_call"),
        ("强赎", "convertible_bond_call"),
        ("convertible_bond_redemption", "convertible_bond_redemption"),
        ("bond_redemption", "convertible_bond_redemption"),
        ("可转债赎回", "convertible_bond_redemption"),
        ("赎回", "convertible_bond_redemption"),
        ("convertible_bond_put", "convertible_bond_put"),
        ("bond_put", "convertible_bond_put"),
        ("可转债回售", "convertible_bond_put"),
        ("回售", "convertible_bond_put"),
        ("可转债转股", "convertible_bond_conversion"),
        ("转股", "convertible_bond_conversion"),
        ("convertible_bond_issue", "convertible_bond_issue"),
        ("可转债", "convertible_bond_issue"),
        ("配股失效", "rights_issue_expiry"),
        ("配股到期", "rights_issue_expiry"),
        ("rights expiry", "rights_issue_expiry"),
        ("rights_issue_expiry", "rights_issue_expiry"),
        ("配股", "rights_issue"),
        ("增发", "new_share_issue"),
        ("新股", "new_share_issue"),
        ("回购", "repurchase"),
        ("停牌", "suspension"),
        ("capital_change", "capital_change"),
        ("share_capital", "capital_change"),
        ("股本变更", "capital_change"),
        ("总股本变更", "capital_change"),
        ("送股", "bonus_share"),
        ("转增", "capital_transfer"),
        ("分红", "cash_dividend"),
        ("rights", "rights_issue"),
        ("bonus", "bonus_share"),
        ("transfer", "capital_transfer"),
        ("dividend", "cash_dividend"),
        ("convert", "convertible_bond_conversion"),
        ("repurchase", "repurchase"),
        ("suspend", "suspension"),
    )
    for marker, action_type in mapping:
        if marker in text:
            return action_type
    if _optional_pick(row, "配股价", "配股价格", "peigujia", "rights_issue_price") is not None:
        return "rights_issue"
    if _optional_pick(row, "转股价", "转股价格", "conversion_price") is not None:
        return "convertible_bond_conversion"
    if _optional_pick(row, "派息", "现金分红", "fenhong", "cash_dividend") is not None:
        return "cash_dividend"
    return "unknown"


def _bare_code(value: str) -> str:
    text = value.strip().translate(_CODE_REPLACEMENTS).lower()
    if "." in text:
        left, right = text.split(".", 1)
        text = right if left in {"sh", "sz", "bj"} else left
    if text.startswith(("sh", "sz", "bj")) and len(text) > 2:
        text = text[2:].lstrip("._")
    if not text.isdigit() or len(text) != 6:
        raise ValueError(f"invalid A-share code: {value!r}")
    return text


def normalize_instrument(value: str) -> str:
    """将常见的交易所代码统一成 Qianxing canonical instrument。

    例：``sh.600000`` -> ``600000.SSE``，``000001`` -> ``000001.SZSE``。
    """

    original = value.strip().translate(_CODE_REPLACEMENTS)
    code = _bare_code(original)
    lowered = original.lower()
    exchange = None
    if "." in lowered:
        left, right = lowered.split(".", 1)
        exchange = left if left in {"sh", "sz", "bj"} else right
    elif lowered.startswith("sh"):
        exchange = "sh"
    elif lowered.startswith("sz"):
        exchange = "sz"
    elif lowered.startswith("bj"):
        exchange = "bj"
    if exchange in {"sh", "sse", "上交所", "上海"} or code.startswith(("5", "6", "68")):
        suffix = "SSE"
    elif exchange in {"sz", "szse", "深交所", "深圳"} or code.startswith(("0", "2", "3")):
        suffix = "SZSE"
    elif exchange in {"bj", "bjse", "北交所", "北京"} or code.startswith(("4", "8")):
        suffix = "BJSE"
    else:
        raise ValueError(f"cannot infer A-share exchange: {value!r}")
    return f"{code}.{suffix}"


def _provider_code(instrument: str) -> str:
    code = _bare_code(instrument)
    exchange = normalize_instrument(instrument).rsplit(".", 1)[1]
    return {"SSE": f"sh.{code}", "SZSE": f"sz.{code}", "BJSE": f"bj.{code}"}[exchange]


@dataclass(frozen=True)
class AshareQuery:
    code: str
    start: str
    end: str
    frequency: str = "daily"
    adjustment: str = "none"

    def validate(self) -> None:
        normalize_instrument(self.code)
        if self.frequency not in _FREQUENCIES:
            raise ValueError(f"unsupported A-share frequency: {self.frequency}")
        if self.adjustment not in _ADJUSTMENTS:
            raise ValueError(f"unsupported adjustment: {self.adjustment}")
        try:
            start = datetime.strptime(self.start.replace("-", ""), "%Y%m%d").date()
            end = datetime.strptime(self.end.replace("-", ""), "%Y%m%d").date()
        except ValueError as exc:
            raise ValueError("start/end must be YYYYMMDD or YYYY-MM-DD") from exc
        if start > end:
            raise ValueError("start must not be after end")

    @property
    def start_compact(self) -> str:
        return self.start.replace("-", "")

    @property
    def end_compact(self) -> str:
        return self.end.replace("-", "")


@dataclass(frozen=True)
class AshareManifest:
    schema_version: int
    provider: str
    provider_version: str
    instrument: str
    frequency: str
    adjustment: str
    start: str
    end: str
    row_count: int
    source_hash: str
    received_at: str

    def to_json(self) -> str:
        return json.dumps(asdict(self), ensure_ascii=False, separators=(",", ":"))

    @classmethod
    def from_json(cls, payload: str) -> "AshareManifest":
        value = json.loads(payload)
        manifest = cls(**value)
        if manifest.schema_version != 1 or manifest.row_count < 1:
            raise ValueError("unsupported or invalid A-share manifest")
        return manifest


@dataclass(frozen=True)
class AshareActionQuery:
    code: str
    start: str
    end: str
    as_of: str | None = None

    def validate(self) -> None:
        normalize_instrument(self.code)
        start = _date_text(self.start)
        end = _date_text(self.end)
        if start is None or end is None or start > end:
            raise ValueError("公司行为 start/end 必须是有效日期且 start <= end")

    @property
    def start_date(self) -> str:
        return _date_text(self.start) or ""

    @property
    def end_date(self) -> str:
        return _date_text(self.end) or ""


@dataclass(frozen=True)
class AshareActionManifest:
    schema_version: int
    provider: str
    provider_version: str
    instrument: str
    start: str
    end: str
    row_count: int
    source_hash: str
    received_at: str

    def to_json(self) -> str:
        return json.dumps(asdict(self), ensure_ascii=False, separators=(",", ":"))

    @classmethod
    def from_json(cls, payload: str) -> "AshareActionManifest":
        value = json.loads(payload)
        manifest = cls(**value)
        if manifest.schema_version != 1 or manifest.row_count < 0:
            raise ValueError("unsupported or invalid A-share action manifest")
        return manifest


def _manifest_timestamp_ms(value: str) -> int:
    parsed = _date_text(value)
    if parsed is None:
        raise ValueError(f"invalid manifest date: {value}")
    day = date.fromisoformat(parsed)
    return int(datetime.combine(day, time.min, tzinfo=_SHANGHAI).timestamp() * 1000)


def _qx_data_bars_fingerprint(frame: BarFrame) -> str:
    """复现 Rust qx-data::fingerprint_bars 的稳定 FNV-1a 规则。"""

    frame.validate()
    value = 0xCBF29CE484222325

    def write_bytes(payload: bytes) -> None:
        nonlocal value
        for byte in payload:
            value = ((value ^ byte) * 0x0100000001B3) & ((1 << 64) - 1)

    def write_u64(number: int) -> None:
        write_bytes(int(number).to_bytes(8, "little", signed=False))

    def write_i128(number: int) -> None:
        write_bytes(int(number).to_bytes(16, "little", signed=True))

    def write_text(text: str) -> None:
        encoded = text.encode("utf-8")
        write_u64(len(encoded))
        write_bytes(encoded)

    write_u64(len(frame.ts))
    for row in zip(
        frame.ts,
        frame.open_raw,
        frame.high_raw,
        frame.low_raw,
        frame.close_raw,
        frame.volume_raw,
    ):
        write_text(frame.instrument)
        write_u64(row[0])
        for value_raw in row[1:]:
            write_i128(value_raw)
    return f"{value:016x}"


def build_dataset_bundle_manifest(
    bundle_id: str,
    version: str,
    source: str,
    bars: AshareManifest,
    actions: AshareActionManifest | None = None,
    calendar: "AshareTradingCalendar" | None = None,
    bars_frame: BarFrame | None = None,
) -> dict[str, Any]:
    """生成 Rust ``DatasetBundleManifest`` 可直接读取的组件清单。

    这里仅绑定已经通过各自 Provider 校验的 manifest，不把 DataFrame 或原始
    公司行为偷偷写入 Bundle。组件数据仍由 `dataset-ingest` 或专用存储管理。
    """

    if not bundle_id.strip() or not version.strip() or not source.strip():
        raise ValueError("bundle_id/version/source are required")
    if not isinstance(bars, AshareManifest) or bars.row_count < 1:
        raise ValueError("bars manifest must contain rows")

    if bars_frame is not None:
        if bars_frame.instrument != bars.instrument or len(bars_frame.ts) != bars.row_count:
            raise ValueError("bars frame does not match bars manifest")
        bars_fingerprint = _qx_data_bars_fingerprint(bars_frame)
    else:
        bars_fingerprint = bars.source_hash
    components: dict[str, dict[str, Any]] = {
        "bars": {
            "kind": "bars",
            "dataset_id": f"{bundle_id}.bars",
            "version": bars.provider_version,
            "source": bars.provider,
            "fingerprint": bars_fingerprint,
            "schema_version": bars.schema_version,
            "start_timestamp": _manifest_timestamp_ms(bars.start),
            "end_timestamp": _manifest_timestamp_ms(bars.end),
            "row_count": bars.row_count,
        }
    }
    if actions is not None:
        if not isinstance(actions, AshareActionManifest):
            raise ValueError("actions must be AshareActionManifest")
        if actions.row_count > 0:
            components["corporate_actions"] = {
                "kind": "corporate_actions",
                "dataset_id": f"{bundle_id}.corporate_actions",
                "version": actions.provider_version,
                "source": actions.provider,
                "fingerprint": actions.source_hash,
                "schema_version": actions.schema_version,
                "start_timestamp": _manifest_timestamp_ms(actions.start),
                "end_timestamp": _manifest_timestamp_ms(actions.end),
                "row_count": actions.row_count,
            }
    if calendar is not None:
        calendar.validate()
        calendar_payload = calendar.to_json().encode("utf-8")
        components["calendar"] = {
            "kind": "calendar",
            "dataset_id": f"{bundle_id}.calendar",
            "version": calendar.calendar_id,
            "source": source,
            "fingerprint": hashlib.sha256(calendar_payload).hexdigest(),
            "schema_version": 1,
            "start_timestamp": _manifest_timestamp_ms(calendar.trading_days[0]),
            "end_timestamp": _manifest_timestamp_ms(calendar.trading_days[-1]),
            "row_count": len(calendar.trading_days),
        }
    return {
        "bundle_id": bundle_id,
        "version": version,
        "source": source,
        "schema_version": 1,
        "components": components,
    }


@dataclass(frozen=True)
class AshareActionConflict:
    instrument: str
    ex_date: str
    action_type: str
    field: str
    left: Any
    right: Any
    left_source: str
    right_source: str


def reconcile_corporate_actions(
    *sources: Sequence["AshareCorporateAction"],
) -> tuple[tuple["AshareCorporateAction", ...], tuple[AshareActionConflict, ...]]:
    """按标的/生效日/类型合并多源事件，并显式返回字段冲突。"""

    merged: dict[tuple[str, str, str], AshareCorporateAction] = {}
    conflicts: list[AshareActionConflict] = []
    comparable = (
        "cash_dividend_raw",
        "share_ratio_num",
        "share_ratio_den",
        "record_date",
        "payment_date",
        "rights_issue_price_raw",
        "rights_issue_ratio_num",
        "rights_issue_ratio_den",
        "issue_price_raw",
        "conversion_price_raw",
        "conversion_ratio_num",
        "conversion_ratio_den",
        "rights_instrument",
        "subscription_qty_raw",
        "rights_expiry_qty_raw",
        "repurchase_qty_raw",
        "repurchase_price_raw",
        "convertible_bond_instrument",
        "conversion_target_instrument",
        "conversion_qty_raw",
        "conversion_target_qty_raw",
    )
    for source_actions in sources:
        for action in source_actions:
            action.validate()
            key = (action.instrument, action.ex_date, action.action_type)
            current = merged.get(key)
            if current is None:
                merged[key] = action
                continue
            updates: dict[str, Any] = {}
            for field_name in comparable:
                left = getattr(current, field_name)
                right = getattr(action, field_name)
                left_is_empty = left in {0, 1, None, ""} and right not in {0, 1, None, ""}
                right_is_empty = right in {0, 1, None, ""} and left not in {0, 1, None, ""}
                if left_is_empty:
                    updates[field_name] = right
                elif not right_is_empty and left != right:
                    conflicts.append(
                        AshareActionConflict(
                            instrument=action.instrument,
                            ex_date=action.ex_date,
                            action_type=action.action_type,
                            field=field_name,
                            left=left,
                            right=right,
                            left_source=current.source,
                            right_source=action.source,
                        )
                    )
            raw = dict(current.raw_payload or {})
            raw.update(action.raw_payload or {})
            source_name = "+".join(dict.fromkeys(filter(None, (current.source, action.source))))
            merged[key] = replace(current, source=source_name, raw_payload=raw, **updates)
    actions = tuple(sorted(merged.values(), key=lambda item: (item.instrument, item.ex_date, item.action_type)))
    return actions, tuple(conflicts)


class AshareDataProvider(Protocol):
    name: str

    def fetch(self, query: AshareQuery) -> tuple[BarFrame, AshareManifest]: ...

    def fetch_corporate_actions(
        self, query: AshareActionQuery
    ) -> tuple[tuple["AshareCorporateAction", ...], AshareActionManifest]: ...


_COLUMN_ALIASES: dict[str, tuple[str, ...]] = {
    "ts": ("ts", "timestamp", "datetime", "date", "time", "日期", "时间", "交易日期"),
    "open": ("open", "开盘", "开盘价"),
    "high": ("high", "最高", "最高价"),
    "low": ("low", "最低", "最低价"),
    "close": ("close", "收盘", "收盘价"),
    "volume": ("volume", "vol", "成交量", "成交量(股)", "成交量（股）"),
}


def _records(value: Any) -> list[Mapping[str, Any]]:
    if hasattr(value, "to_dict"):
        try:
            value = value.to_dict("records")
        except TypeError as exc:
            raise AshareProviderError("dataframe does not support records conversion") from exc
    if not isinstance(value, Iterable) or isinstance(value, (str, bytes, Mapping)):
        raise AshareProviderError("provider result must be an iterable of row mappings")
    result = []
    for row in value:
        if isinstance(row, Mapping):
            result.append(row)
        else:
            raise AshareProviderError("provider result contains a non-mapping row")
    return result


def _pick(row: Mapping[str, Any], field: str) -> Any:
    for alias in _COLUMN_ALIASES[field]:
        if alias in row:
            return row[alias]
    normalized = {str(key).strip().lower(): value for key, value in row.items()}
    for alias in _COLUMN_ALIASES[field]:
        if alias.lower() in normalized:
            return normalized[alias.lower()]
    raise AshareProviderError(f"missing {field} column; available={list(row)}")


def _timestamp(value: Any) -> int:
    if isinstance(value, bool):
        raise AshareProviderError("boolean timestamp is invalid")
    if isinstance(value, (int, float)):
        number = int(value)
        if number < 100_000_000_000:
            number *= 1000
        return number
    if isinstance(value, datetime):
        parsed = value
    elif isinstance(value, date):
        parsed = datetime.combine(value, time.min)
    else:
        text = str(value).strip().replace("/", "-")
        if text.endswith("Z"):
            text = text[:-1] + "+00:00"
        try:
            parsed = datetime.fromisoformat(text)
        except ValueError:
            for pattern in ("%Y-%m-%d", "%Y%m%d", "%Y-%m-%d %H:%M:%S", "%Y/%m/%d %H:%M:%S"):
                try:
                    parsed = datetime.strptime(text, pattern)
                    break
                except ValueError:
                    continue
            else:
                raise AshareProviderError(f"cannot parse timestamp: {value!r}")
    if parsed.tzinfo is None:
        parsed = parsed.replace(tzinfo=_SHANGHAI)
    return int(parsed.timestamp() * 1000)


def _scaled(value: Any, field: str) -> int:
    if value is None or str(value).strip() in {"", "-", "--", "nan", "NaN"}:
        raise AshareProviderError(f"empty {field} value")
    try:
        number = Decimal(str(value).replace(",", "").strip())
    except (InvalidOperation, ValueError) as exc:
        raise AshareProviderError(f"invalid {field} value: {value!r}") from exc
    if not number.is_finite() or (field == "volume" and number < 0) or (field != "volume" and number <= 0):
        raise AshareProviderError(f"invalid {field} value: {value!r}")
    return int((number * SCALE).quantize(Decimal("1"), rounding=ROUND_HALF_EVEN))


def normalize_bar_rows(
    rows: Any,
    *,
    code: str,
    source: str,
    start: str | None = None,
    end: str | None = None,
    frequency: str = "daily",
    adjustment: str = "none",
) -> BarFrame:
    """将 AkShare/Baostock/easy_tdx 的 DataFrame 或 rows 转成严格有序 BarFrame。"""

    instrument = normalize_instrument(code)
    lower_bound = _timestamp(start) if start else None
    upper_bound = _timestamp(end) + 86_399_999 if end else None
    dedup: dict[int, tuple[int, int, int, int, int]] = {}
    for row in _records(rows):
        timestamp_value = _pick(row, "ts")
        # Baostock 分钟结果通常同时提供 date/time；优先合并，避免所有分钟 K
        # 被错误压缩到交易日 00:00 并在去重时只剩一根。
        if "date" in row and "time" in row and "datetime" not in row and "timestamp" not in row:
            timestamp_value = f"{row['date']} {row['time']}"
        ts = _timestamp(timestamp_value)
        if lower_bound is not None and ts < lower_bound:
            continue
        if upper_bound is not None and ts > upper_bound:
            continue
        dedup[ts] = (
            _scaled(_pick(row, "open"), "open"),
            _scaled(_pick(row, "high"), "high"),
            _scaled(_pick(row, "low"), "low"),
            _scaled(_pick(row, "close"), "close"),
            _scaled(_pick(row, "volume"), "volume"),
        )
    if not dedup:
        raise AshareProviderError(f"provider returned no bars for {instrument}")
    ordered = sorted(dedup.items())
    frame = BarFrame(
        instrument=instrument,
        source=source,
        ts=tuple(item[0] for item in ordered),
        open_raw=tuple(item[1][0] for item in ordered),
        high_raw=tuple(item[1][1] for item in ordered),
        low_raw=tuple(item[1][2] for item in ordered),
        close_raw=tuple(item[1][3] for item in ordered),
        volume_raw=tuple(item[1][4] for item in ordered),
    )
    frame.validate()
    return frame


def _manifest(query: AshareQuery, frame: BarFrame, provider: str, version: str) -> AshareManifest:
    return AshareManifest(
        schema_version=1,
        provider=provider,
        provider_version=version,
        instrument=frame.instrument,
        frequency=query.frequency,
        adjustment=query.adjustment,
        start=query.start,
        end=query.end,
        row_count=len(frame.ts),
        source_hash=f"{frame.digest():016x}",
        received_at=datetime.now(timezone.utc).isoformat(),
    )


def _actions_manifest(
    query: AshareActionQuery,
    actions: Sequence["AshareCorporateAction"],
    provider: str,
    version: str,
) -> AshareActionManifest:
    payload = json.dumps(
        [asdict(action) for action in actions],
        ensure_ascii=False,
        sort_keys=True,
        separators=(",", ":"),
    ).encode("utf-8")
    return AshareActionManifest(
        schema_version=1,
        provider=provider,
        provider_version=version,
        instrument=normalize_instrument(query.code),
        start=query.start_date,
        end=query.end_date,
        row_count=len(actions),
        source_hash=hashlib.sha256(payload).hexdigest(),
        received_at=datetime.now(timezone.utc).isoformat(),
    )


def normalize_corporate_action_rows(
    rows: Any,
    *,
    code: str,
    source: str,
    start: str | None = None,
    end: str | None = None,
    as_of: str | None = None,
) -> tuple["AshareCorporateAction", ...]:
    """将三类数据源的公司行为行统一为可审计、PIT 安全的事件。"""

    instrument = normalize_instrument(code)
    start_date = _date_text(start) if start else None
    end_date = _date_text(end) if end else None
    result: dict[tuple[str, str, str], AshareCorporateAction] = {}
    for row in _records(rows):
        ex_date = _date_text(
            _optional_pick(row, "ex_date", "除权除息日", "除权日", "除息日", "effective_date", "日期", "date")
        )
        if ex_date is None or (start_date and ex_date < start_date) or (end_date and ex_date > end_date):
            continue
        published_at = _optional_pick(
            row, "published_at", "公告日期", "方案公告日期", "公告日", "notice_date", "publish_date"
        )
        published_date = _date_text(published_at) or ex_date
        published_iso = f"{published_date}T00:00:00+08:00"
        if as_of is not None and published_iso > as_of:
            continue
        action_type = _canonical_action_type(row)

        cash = _optional_pick(row, "cash_dividend_raw")
        if cash is None:
            cash = _optional_pick(row, "派息", "现金分红", "每股派息", "fenhong", "cash_dividend")
        cash_raw = _nonnegative_scaled(cash, "cash_dividend")

        bonus = _optional_pick(row, "送股", "送股比例", "bonus_share", "bonus_ratio", "songzhuangu")
        transfer = _optional_pick(row, "转增", "转增比例", "capital_transfer", "transfer_ratio")
        bonus_num, bonus_den = _ratio_scaled(bonus, "bonus_ratio")
        transfer_num, transfer_den = _ratio_scaled(transfer, "transfer_ratio")
        ratio_num, ratio_den = SCALE, SCALE
        for part_num, part_den in ((bonus_num, bonus_den), (transfer_num, transfer_den)):
            if part_num:
                ratio_num = ratio_num * (part_den + part_num) // part_den
        if not bonus_num and not transfer_num:
            direct_num = _optional_pick(row, "share_ratio_num")
            direct_den = _optional_pick(row, "share_ratio_den")
            if direct_num is not None and direct_den is not None:
                ratio_num, ratio_den = int(direct_num), int(direct_den)

        rights_price = _optional_pick(row, "配股价", "配股价格", "peigujia", "rights_issue_price", "rights_price")
        rights_num, rights_den = _ratio_scaled(
            _optional_pick(row, "配股比例", "配股数", "peigu", "rights_issue_ratio", "rights_ratio"),
            "rights_issue_ratio",
        )
        issue_price = _optional_pick(row, "增发价", "发行价", "增发价格", "issue_price", "new_issue_price")
        conversion_price = _optional_pick(row, "转股价", "转股价格", "conversion_price")
        conversion_num, conversion_den = _ratio_scaled(
            _optional_pick(row, "转股比例", "转股数", "conversion_ratio"), "conversion_ratio"
        )
        rights_instrument = _optional_pick(
            row, "rights_instrument", "配股权代码", "配股代码", "rights_symbol"
        )
        subscription_qty = _optional_pick(
            row, "subscription_qty", "subscription_quantity", "认购数量", "认购股数"
        )
        rights_expiry_qty = _optional_pick(
            row,
            "rights_expiry_qty",
            "rights_expiry_quantity",
            "配股失效数量",
            "配股到期数量",
        )
        repurchase_qty = _optional_pick(
            row, "repurchase_qty", "repurchase_quantity", "回购数量", "回购股数"
        )
        repurchase_price = _optional_pick(row, "repurchase_price", "回购价", "回购价格")
        bond_instrument = _optional_pick(
            row, "convertible_bond_instrument", "可转债代码", "债券代码", "bond_symbol"
        )
        target_instrument = _optional_pick(
            row, "conversion_target_instrument", "转股标的", "转股股票代码", "target_symbol"
        )
        conversion_qty = _optional_pick(
            row, "conversion_qty", "conversion_quantity", "转股数量", "转债数量"
        )
        target_qty = _optional_pick(
            row, "conversion_target_qty", "conversion_target_quantity", "转股所得数量"
        )
        interest_per_bond = _optional_pick(
            row,
            "interest_per_bond_raw",
            "interest_per_bond",
            "bond_interest",
            "每张利息",
            "每债利息",
            "利息",
        )
        settlement_qty = _optional_pick(
            row,
            "settlement_qty_raw",
            "settlement_qty",
            "settlement_quantity",
            "赎回数量",
            "回售数量",
            "结算数量",
        )
        settlement_price = _optional_pick(
            row,
            "settlement_price_raw",
            "settlement_price",
            "赎回价",
            "回售价",
            "结算价格",
        )
        issuer_total_raw_value = _optional_pick(row, "issuer_total_shares_raw")
        issuer_free_float_raw_value = _optional_pick(row, "issuer_free_float_shares_raw")
        issuer_total_shares = (
            _nonnegative_raw(issuer_total_raw_value, "issuer_total_shares_raw")
            if issuer_total_raw_value is not None
            else _nonnegative_scaled(
                _optional_pick(row, "issuer_total_shares", "total_shares", "总股本", "总股本数"),
                "issuer_total_shares",
            )
        )
        issuer_free_float_shares = (
            _nonnegative_raw(issuer_free_float_raw_value, "issuer_free_float_shares_raw")
            if issuer_free_float_raw_value is not None
            else _nonnegative_scaled(
                _optional_pick(
                    row,
                    "issuer_free_float_shares",
                    "free_float_shares",
                    "流通股本",
                    "流通股数",
                ),
                "issuer_free_float_shares",
            )
        )
        event = AshareCorporateAction(
            instrument=instrument,
            ex_date=ex_date,
            published_at=published_iso,
            cash_dividend_raw=cash_raw,
            share_ratio_num=ratio_num,
            share_ratio_den=ratio_den,
            source=source,
            action_type=action_type,
            announcement_date=_date_text(_optional_pick(row, "公告日期", "notice_date")),
            record_date=_date_text(_optional_pick(row, "股权登记日", "登记日", "record_date")),
            payment_date=_date_text(_optional_pick(row, "派息日", "payment_date")),
            subscription_start=_date_text(_optional_pick(row, "认购开始日", "subscription_start")),
            subscription_end=_date_text(_optional_pick(row, "认购截止日", "subscription_end")),
            rights_issue_price_raw=_nonnegative_scaled(rights_price, "rights_issue_price"),
            rights_issue_ratio_num=rights_num,
            rights_issue_ratio_den=rights_den,
            issue_price_raw=_nonnegative_scaled(issue_price, "issue_price"),
            conversion_price_raw=_nonnegative_scaled(conversion_price, "conversion_price"),
            conversion_ratio_num=conversion_num,
            conversion_ratio_den=conversion_den,
            rights_instrument=(normalize_instrument(str(rights_instrument))
                               if rights_instrument not in (None, "") else None),
            subscription_qty_raw=_nonnegative_scaled(subscription_qty, "subscription_qty"),
            rights_expiry_qty_raw=_nonnegative_scaled(rights_expiry_qty, "rights_expiry_qty"),
            repurchase_qty_raw=_nonnegative_scaled(repurchase_qty, "repurchase_qty"),
            repurchase_price_raw=_nonnegative_scaled(repurchase_price, "repurchase_price"),
            convertible_bond_instrument=(normalize_instrument(str(bond_instrument))
                                         if bond_instrument not in (None, "") else None),
            conversion_target_instrument=(normalize_instrument(str(target_instrument))
                                          if target_instrument not in (None, "") else None),
            conversion_qty_raw=_nonnegative_scaled(conversion_qty, "conversion_qty"),
            conversion_target_qty_raw=_nonnegative_scaled(target_qty, "conversion_target_qty"),
            interest_per_bond_raw=_nonnegative_scaled(interest_per_bond, "interest_per_bond"),
            settlement_qty_raw=_nonnegative_scaled(settlement_qty, "settlement_qty"),
            settlement_price_raw=_nonnegative_scaled(settlement_price, "settlement_price"),
            issuer_total_shares_raw=issuer_total_shares,
            issuer_free_float_shares_raw=issuer_free_float_shares or None,
            raw_payload={str(key): _json_safe(value) for key, value in row.items()},
        )
        event.validate()
        key = (event.ex_date, event.action_type, json.dumps(event.raw_payload, ensure_ascii=False, sort_keys=True))
        result[key] = event
    return tuple(sorted(result.values(), key=lambda item: (item.ex_date, item.action_type, item.published_at)))


def _invoke_rows_method(module: Any, names: Sequence[str], code: str) -> tuple[Any, str]:
    """兼容不同版本数据源函数名和参数名，同时保留明确的失败原因。"""

    attempted: list[str] = []
    for name in names:
        method = getattr(module, name, None)
        if not callable(method):
            continue
        attempted.append(name)
        for kwargs in ({"symbol": code}, {"stock": code}, {"code": code}, {}):
            try:
                return method(**kwargs), name
            except TypeError:
                continue
            except Exception as exc:
                raise AshareProviderError(f"{name} 获取公司行为失败: {exc}") from exc
    if not attempted:
        raise AshareProviderError(f"数据源未提供公司行为接口，候选接口: {', '.join(names)}")
    raise AshareProviderError(f"数据源公司行为接口参数不兼容: {', '.join(attempted)}")


def _adjustment_arg(adjustment: str) -> str:
    return {"none": "", "qfq": "qfq", "hfq": "hfq"}[adjustment]


class AkShareProvider:
    name = "akshare"

    def __init__(self, module: Any | None = None) -> None:
        self._module = module

    def _load(self) -> Any:
        if self._module is not None:
            return self._module
        try:
            self._module = importlib.import_module("akshare")
        except ImportError as exc:
            raise AshareProviderError("AkShare 未安装，请执行 pip install -e '.[a-share-akshare]'") from exc
        return self._module

    def fetch(self, query: AshareQuery) -> tuple[BarFrame, AshareManifest]:
        query.validate()
        module = self._load()
        code = _bare_code(query.code)
        try:
            if query.frequency in {"daily", "weekly", "monthly"}:
                rows = module.stock_zh_a_hist(
                    symbol=code,
                    period=query.frequency,
                    start_date=query.start_compact,
                    end_date=query.end_compact,
                    adjust=_adjustment_arg(query.adjustment),
                )
            elif hasattr(module, "stock_zh_a_minute"):
                rows = module.stock_zh_a_minute(
                    symbol=code,
                    period=query.frequency.removesuffix("m"),
                    adjust=_adjustment_arg(query.adjustment),
                )
            else:
                raise AshareProviderError("当前 AkShare 版本没有 stock_zh_a_minute，无法读取分钟线")
        except AshareProviderError:
            raise
        except Exception as exc:
            raise AshareProviderError(f"AkShare fetch failed: {exc}") from exc
        frame = normalize_bar_rows(
            rows,
            code=query.code,
            source=f"akshare:stock_zh_a_hist:{query.frequency}:{query.adjustment}",
            start=query.start,
            end=query.end,
            frequency=query.frequency,
            adjustment=query.adjustment,
        )
        return frame, _manifest(query, frame, self.name, getattr(module, "__version__", "unknown"))

    def fetch_corporate_actions(
        self, query: AshareActionQuery
    ) -> tuple[tuple["AshareCorporateAction", ...], AshareActionManifest]:
        query.validate()
        module = self._load()
        rows, method_name = _invoke_rows_method(
            module,
            ("stock_dividend_cninfo", "stock_fhps_detail_em", "stock_dividend_plan_em"),
            _bare_code(query.code),
        )
        actions = normalize_corporate_action_rows(
            rows,
            code=query.code,
            source=f"akshare:{method_name}",
            start=query.start_date,
            end=query.end_date,
            as_of=query.as_of,
        )
        return actions, _actions_manifest(query, actions, self.name, getattr(module, "__version__", "unknown"))


class BaoStockProvider:
    name = "baostock"

    def __init__(self, module: Any | None = None) -> None:
        self._module = module

    def _load(self) -> Any:
        if self._module is not None:
            return self._module
        try:
            self._module = importlib.import_module("baostock")
        except ImportError as exc:
            raise AshareProviderError("Baostock 未安装，请执行 pip install -e '.[a-share-baostock]'") from exc
        return self._module

    def fetch(self, query: AshareQuery) -> tuple[BarFrame, AshareManifest]:
        query.validate()
        if query.frequency not in {"daily", "weekly", "monthly", "5m", "15m", "30m", "60m"}:
            raise AshareProviderError("Baostock 当前适配 daily/weekly/monthly/5m/15m/30m/60m")
        module = self._load()
        frequency = {"daily": "d", "weekly": "w", "monthly": "m", "5m": "5", "15m": "15", "30m": "30", "60m": "60"}[query.frequency]
        adjustflag = {"none": "3", "qfq": "2", "hfq": "1"}[query.adjustment]
        session = module.login()
        if getattr(session, "error_code", "0") not in {"0", 0, None}:
            raise AshareProviderError(f"Baostock login failed: {getattr(session, 'error_msg', session)}")
        try:
            result = module.query_history_k_data_plus(
                _provider_code(query.code),
                "date,time,code,open,high,low,close,volume,amount",
                start_date=query.start_compact[:4] + "-" + query.start_compact[4:6] + "-" + query.start_compact[6:],
                end_date=query.end_compact[:4] + "-" + query.end_compact[4:6] + "-" + query.end_compact[6:],
                frequency=frequency,
                adjustflag=adjustflag,
            )
            if getattr(result, "error_code", "0") not in {"0", 0, None}:
                raise AshareProviderError(f"Baostock query failed: {getattr(result, 'error_msg', result)}")
            fields = list(getattr(result, "fields", []))
            rows = []
            while result.next():
                rows.append(dict(zip(fields, result.get_row_data())))
        finally:
            logout = getattr(module, "logout", None)
            if callable(logout):
                logout()
        frame = normalize_bar_rows(
            rows,
            code=query.code,
            source=f"baostock:query_history_k_data_plus:{query.frequency}:{query.adjustment}",
            start=query.start,
            end=query.end,
            frequency=query.frequency,
            adjustment=query.adjustment,
        )
        return frame, _manifest(query, frame, self.name, getattr(module, "__version__", "unknown"))

    def fetch_corporate_actions(
        self, query: AshareActionQuery
    ) -> tuple[tuple["AshareCorporateAction", ...], AshareActionManifest]:
        query.validate()
        module = self._load()
        method = getattr(module, "query_dividend_data", None)
        if not callable(method):
            raise AshareProviderError("Baostock 未提供 query_dividend_data 公司行为接口")
        session = module.login()
        if getattr(session, "error_code", "0") not in {"0", 0, None}:
            raise AshareProviderError(f"Baostock login failed: {getattr(session, 'error_msg', session)}")
        rows: list[Mapping[str, Any]] = []
        try:
            start_year = int(query.start_date[:4])
            end_year = int(query.end_date[:4])
            for year in range(start_year, end_year + 1):
                try:
                    result = method(_provider_code(query.code), year=year, yearType="report")
                except TypeError:
                    result = method(_provider_code(query.code), year, "report")
                if getattr(result, "error_code", "0") not in {"0", 0, None}:
                    raise AshareProviderError(
                        f"Baostock dividend query failed: {getattr(result, 'error_msg', result)}"
                    )
                fields = list(getattr(result, "fields", []))
                while result.next():
                    rows.append(dict(zip(fields, result.get_row_data())))
        finally:
            logout = getattr(module, "logout", None)
            if callable(logout):
                logout()
        actions = normalize_corporate_action_rows(
            rows,
            code=query.code,
            source="baostock:query_dividend_data",
            start=query.start_date,
            end=query.end_date,
            as_of=query.as_of,
        )
        return actions, _actions_manifest(query, actions, self.name, getattr(module, "__version__", "unknown"))


class EasyTdxProvider:
    """easy_tdx 的薄适配层，优先接收调用方已创建的 client 便于复用连接。"""

    name = "easy_tdx"

    def __init__(self, client: Any | None = None, module: Any | None = None) -> None:
        self._client = client
        self._module = module

    def _load(self) -> Any:
        if self._module is not None:
            return self._module
        try:
            self._module = importlib.import_module("easy_tdx")
        except ImportError as exc:
            raise AshareProviderError("easy_tdx 未安装，请执行 pip install -e '.[a-share-easy-tdx]'") from exc
        return self._module

    def fetch(self, query: AshareQuery) -> tuple[BarFrame, AshareManifest]:
        query.validate()
        module = self._load()
        client = self._client
        managed = False
        if client is None:
            factory = (
                getattr(module, "UnifiedTdxClient", None)
                or getattr(module, "MacClient", None)
                or getattr(module, "TdxClient", None)
            )
            if factory is None:
                raise AshareProviderError("easy_tdx 未提供 UnifiedTdxClient/MacClient/TdxClient")
            best_host = getattr(factory, "from_best_host", None)
            client = best_host() if callable(best_host) else factory()
            managed = hasattr(client, "__enter__")
        market_name = normalize_instrument(query.code).rsplit(".", 1)[1]
        market_code = {"SSE": "SH", "SZSE": "SZ", "BJSE": "BJ"}[market_name]
        market = getattr(module, market_code, None)
        market_type = getattr(module, "Market", None)
        if market_type is not None:
            market = getattr(market_type, market_code, market)
        if market is None:
            market = market_code
        period_name = {"daily": "DAILY", "weekly": "WEEKLY", "monthly": "MONTHLY", "1m": "MIN1", "5m": "MIN5", "15m": "MIN15", "30m": "MIN30", "60m": "MIN60"}[query.frequency]
        period_type = getattr(module, "Period", object())
        period = getattr(period_type, period_name, period_name)
        try:
            if managed:
                with client as active:
                    rows = self._call(active, market, _bare_code(query.code), period, query)
            else:
                rows = self._call(client, market, _bare_code(query.code), period, query)
        except AshareProviderError:
            raise
        except Exception as exc:
            raise AshareProviderError(f"easy_tdx fetch failed: {exc}") from exc
        frame = normalize_bar_rows(
            rows,
            code=query.code,
            source=f"easy_tdx:get_stock_kline:{query.frequency}:{query.adjustment}",
            start=query.start,
            end=query.end,
            frequency=query.frequency,
            adjustment=query.adjustment,
        )
        return frame, _manifest(query, frame, self.name, getattr(module, "__version__", "unknown"))

    def fetch_corporate_actions(
        self, query: AshareActionQuery
    ) -> tuple[tuple["AshareCorporateAction", ...], AshareActionManifest]:
        query.validate()
        module = self._load()
        client = self._client
        managed = False
        if client is None:
            factory = (
                getattr(module, "UnifiedTdxClient", None)
                or getattr(module, "MacClient", None)
                or getattr(module, "TdxClient", None)
            )
            if factory is None:
                raise AshareProviderError("easy_tdx 未提供 UnifiedTdxClient/MacClient/TdxClient")
            best_host = getattr(factory, "from_best_host", None)
            client = best_host() if callable(best_host) else factory()
            managed = hasattr(client, "__enter__")
        market_name = normalize_instrument(query.code).rsplit(".", 1)[1]
        market_code = {"SSE": "SH", "SZSE": "SZ", "BJSE": "BJ"}[market_name]
        market = getattr(module, market_code, market_code)
        market_type = getattr(module, "Market", None)
        if market_type is not None:
            market = getattr(market_type, market_code, market)

        def call(active: Any) -> Any:
            method = getattr(active, "get_xdxr_info", None) or getattr(active, "get_xdxr", None)
            if not callable(method):
                raise AshareProviderError("easy_tdx client does not expose get_xdxr_info/get_xdxr")
            try:
                return method(market, _bare_code(query.code))
            except TypeError:
                return method(market=market, code=_bare_code(query.code))

        try:
            rows = call(client) if not managed else None
            if managed:
                with client as active:
                    rows = call(active)
        except AshareProviderError:
            raise
        except Exception as exc:
            raise AshareProviderError(f"easy_tdx corporate action fetch failed: {exc}") from exc
        actions = normalize_corporate_action_rows(
            rows,
            code=query.code,
            source="easy_tdx:get_xdxr_info",
            start=query.start_date,
            end=query.end_date,
            as_of=query.as_of,
        )
        return actions, _actions_manifest(query, actions, self.name, getattr(module, "__version__", "unknown"))

    @staticmethod
    def _call(client: Any, market: Any, code: str, period: Any, query: AshareQuery) -> Any:
        method = getattr(client, "get_stock_kline", None)
        if not callable(method):
            method = getattr(client, "get_security_bars", None)
        if not callable(method):
            raise AshareProviderError("easy_tdx client does not expose get_stock_kline/get_security_bars")
        try:
            return method(market, code, period, count=5000, adjust=query.adjustment)
        except TypeError:
            return method(market, code, period, count=5000)


def create_provider(name: str = "auto") -> AshareDataProvider:
    """按显式选择或可用依赖选择数据源；不会在失败时静默切换数据源。"""

    normalized = name.lower().replace("-", "_")
    providers: dict[str, type[Any]] = {
        "akshare": AkShareProvider,
        "baostock": BaoStockProvider,
        "easy_tdx": EasyTdxProvider,
    }
    if normalized != "auto":
        if normalized not in providers:
            raise ValueError(f"unsupported A-share provider: {name}")
        return providers[normalized]()
    for candidate in ("akshare", "baostock", "easy_tdx"):
        module_name = "easy_tdx" if candidate == "easy_tdx" else candidate
        if importlib.util.find_spec(module_name) is not None:
            return providers[candidate]()
    raise AshareProviderError("没有可用 A 股数据源，请安装 a-share-akshare/baostock/easy-tdx 之一")


def screen_bar_frames(
    frames: Sequence[BarFrame | str | Path],
    *,
    min_return_bps: int = -10_000,
    min_avg_volume_raw: int = 0,
    limit: int | None = None,
) -> list[dict[str, Any]]:
    """基于已落盘 BarFrame 做无未来数据的轻量初筛，结果可直接生成回测任务。"""

    if limit is not None and limit <= 0:
        raise ValueError("limit must be positive")
    result: list[dict[str, Any]] = []
    for item in frames:
        source_path = None if isinstance(item, BarFrame) else str(Path(item))
        frame = item if isinstance(item, BarFrame) else BarFrame.from_json(Path(item).read_text(encoding="utf-8"))
        first = frame.close_raw[0]
        last = frame.close_raw[-1]
        total_return_bps = (last - first) * 10_000 // first
        average_volume = sum(frame.volume_raw) // len(frame.volume_raw)
        peak = frame.close_raw[0]
        max_drawdown_bps = 0
        for close in frame.close_raw:
            peak = max(peak, close)
            max_drawdown_bps = min(max_drawdown_bps, (close - peak) * 10_000 // peak)
        if total_return_bps < min_return_bps or average_volume < min_avg_volume_raw:
            continue
        result.append({
            "instrument": frame.instrument,
            "source": frame.source,
            "bars": len(frame.ts),
            "start_ts": frame.ts[0],
            "end_ts": frame.ts[-1],
            "total_return_bps": total_return_bps,
            "max_drawdown_bps": max_drawdown_bps,
            "average_volume_raw": average_volume,
            "source_hash": f"{frame.digest():016x}",
        })
        if source_path is not None:
            result[-1]["bars_path"] = source_path
    result.sort(key=lambda row: (row["total_return_bps"], row["average_volume_raw"]), reverse=True)
    return result[:limit] if limit else result


@dataclass(frozen=True)
class AshareTradingCalendar:
    """可审计的交易日/交易时段快照，供上层生成 Rust 规则快照。"""

    calendar_id: str
    trading_days: tuple[str, ...]
    sessions: tuple[tuple[str, str], ...] = ()

    def validate(self) -> None:
        if not self.calendar_id.strip() or not self.trading_days:
            raise ValueError("calendar_id and trading_days are required")
        if any(left >= right for left, right in zip(self.trading_days, self.trading_days[1:])):
            raise ValueError("trading_days must be strictly sorted")
        for start, end in self.sessions:
            if start >= end:
                raise ValueError("calendar session start must be before end")

    def contains(self, trading_day: str) -> bool:
        self.validate()
        return trading_day in self.trading_days

    def to_json(self) -> str:
        self.validate()
        return json.dumps(asdict(self), ensure_ascii=False, separators=(",", ":"))

    @classmethod
    def from_json(cls, payload: str) -> "AshareTradingCalendar":
        value = json.loads(payload)
        calendar = cls(
            calendar_id=value["calendar_id"],
            trading_days=tuple(value["trading_days"]),
            sessions=tuple(tuple(session) for session in value.get("sessions", [])),
        )
        calendar.validate()
        return calendar


@dataclass(frozen=True)
class AshareCorporateAction:
    """公司行为标准事件；原始字段保留在 ``raw_payload`` 供审计和重放。"""

    instrument: str
    ex_date: str
    published_at: str
    cash_dividend_raw: int = 0
    share_ratio_num: int = 1
    share_ratio_den: int = 1
    source: str = ""
    action_type: str = "unknown"
    announcement_date: str | None = None
    record_date: str | None = None
    payment_date: str | None = None
    subscription_start: str | None = None
    subscription_end: str | None = None
    rights_issue_price_raw: int = 0
    rights_issue_ratio_num: int = 0
    rights_issue_ratio_den: int = 1
    issue_price_raw: int = 0
    conversion_price_raw: int = 0
    conversion_ratio_num: int = 0
    conversion_ratio_den: int = 1
    rights_instrument: str | None = None
    subscription_qty_raw: int = 0
    rights_expiry_qty_raw: int = 0
    repurchase_qty_raw: int = 0
    repurchase_price_raw: int = 0
    convertible_bond_instrument: str | None = None
    conversion_target_instrument: str | None = None
    conversion_qty_raw: int = 0
    conversion_target_qty_raw: int = 0
    interest_per_bond_raw: int = 0
    settlement_qty_raw: int = 0
    settlement_price_raw: int = 0
    issuer_total_shares_raw: int = 0
    issuer_free_float_shares_raw: int | None = None
    raw_payload: dict[str, Any] | None = None

    def validate(self) -> None:
        normalize_instrument(self.instrument)
        if (
            not self.ex_date
            or not self.published_at
            or self.share_ratio_num <= 0
            or self.share_ratio_den <= 0
            or self.rights_issue_price_raw < 0
            or self.rights_issue_ratio_num < 0
            or self.rights_issue_ratio_den <= 0
            or self.issue_price_raw < 0
            or self.conversion_price_raw < 0
            or self.conversion_ratio_num < 0
            or self.conversion_ratio_den <= 0
            or self.subscription_qty_raw < 0
            or self.rights_expiry_qty_raw < 0
            or self.repurchase_qty_raw < 0
            or self.repurchase_price_raw < 0
            or self.conversion_qty_raw < 0
            or self.conversion_target_qty_raw < 0
            or self.interest_per_bond_raw < 0
            or self.settlement_qty_raw < 0
            or self.settlement_price_raw < 0
            or self.issuer_total_shares_raw < 0
            or (self.issuer_free_float_shares_raw is not None and self.issuer_free_float_shares_raw < 0)
        ):
            raise ValueError("invalid corporate action date or ratio")
        if self.action_type not in _CORPORATE_ACTION_TYPES:
            raise ValueError(f"unsupported corporate action type: {self.action_type}")
        if self.action_type == "rights_issue":
            if (
                not self.rights_instrument
                or self.rights_issue_price_raw <= 0
                or self.rights_issue_ratio_num <= 0
            ):
                raise ValueError("rights_issue requires instrument, price and ratio")
        elif self.action_type == "rights_issue_expiry":
            if not self.rights_instrument or self.rights_expiry_qty_raw <= 0:
                raise ValueError("rights_issue_expiry requires instrument and quantity")
        elif self.action_type == "new_share_issue":
            if self.issue_price_raw <= 0 or self.subscription_qty_raw <= 0:
                raise ValueError("new_share_issue requires price and subscription quantity")
        elif self.action_type == "convertible_bond_issue":
            if (
                not self.convertible_bond_instrument
                or self.issue_price_raw <= 0
                or self.subscription_qty_raw <= 0
            ):
                raise ValueError("convertible_bond_issue requires bond, price and subscription quantity")
        elif self.action_type == "convertible_bond_interest":
            if not self.convertible_bond_instrument or self.interest_per_bond_raw <= 0:
                raise ValueError("convertible_bond_interest requires bond and interest")
        elif self.action_type in {"convertible_bond_redemption", "convertible_bond_put"}:
            if (
                not self.convertible_bond_instrument
                or self.settlement_qty_raw <= 0
                or self.settlement_price_raw <= 0
            ):
                raise ValueError("convertible bond settlement requires bond, quantity and price")
        elif self.action_type == "convertible_bond_call":
            if not self.convertible_bond_instrument or self.settlement_price_raw <= 0:
                raise ValueError("convertible_bond_call requires bond and price")
        elif self.action_type == "repurchase":
            if self.repurchase_qty_raw <= 0 or self.repurchase_price_raw <= 0:
                raise ValueError("repurchase requires quantity and price")
        elif self.action_type == "convertible_bond_conversion":
            if (
                not self.convertible_bond_instrument
                or not self.conversion_target_instrument
                or self.conversion_qty_raw <= 0
                or self.conversion_target_qty_raw <= 0
                or self.conversion_price_raw <= 0
            ):
                raise ValueError("convertible_bond_conversion requires explicit conversion facts")
        elif self.action_type == "capital_change":
            if self.issuer_total_shares_raw <= 0:
                raise ValueError("capital_change requires issuer_total_shares_raw")
            if (
                self.issuer_free_float_shares_raw is not None
                and (
                    self.issuer_free_float_shares_raw <= 0
                    or self.issuer_free_float_shares_raw > self.issuer_total_shares_raw
                )
            ):
                raise ValueError("capital_change free float cannot exceed total shares")
        if self.cash_dividend_raw < 0:
            raise ValueError("cash_dividend_raw cannot be negative")
        for field_name in (
            "ex_date",
            "announcement_date",
            "record_date",
            "payment_date",
            "subscription_start",
            "subscription_end",
        ):
            value = getattr(self, field_name)
            if value is not None and _date_text(value) is None:
                raise ValueError(f"invalid corporate action date: {field_name}")
        if self.raw_payload is not None and not isinstance(self.raw_payload, dict):
            raise ValueError("raw_payload must be a dict")

    def visible_at(self, as_of: str) -> bool:
        self.validate()
        return self.published_at <= as_of

    def to_json(self) -> str:
        self.validate()
        return json.dumps(asdict(self), ensure_ascii=False, sort_keys=True, separators=(",", ":"))

    @classmethod
    def from_json(cls, payload: str) -> "AshareCorporateAction":
        value = cls(**json.loads(payload))
        value.validate()
        return value


@dataclass(frozen=True)
class AsharePITRecord:
    """财务/公告/行业等研究事实的最小 PIT 封装。"""

    instrument: str
    effective_at: str
    published_at: str
    values: dict[str, Any]
    source: str = ""

    def validate(self) -> None:
        normalize_instrument(self.instrument)
        if not self.effective_at or not self.published_at or not isinstance(self.values, dict):
            raise ValueError("invalid PIT record")

    def visible_at(self, as_of: str) -> bool:
        self.validate()
        return self.published_at <= as_of


__all__ = [
    "AshareDataProvider",
    "build_dataset_bundle_manifest",
    "AshareManifest",
    "AsharePITRecord",
    "AshareProviderError",
    "AshareQuery",
    "AshareCorporateAction",
    "AshareActionManifest",
    "AshareActionConflict",
    "AshareActionQuery",
    "AshareTradingCalendar",
    "AkShareProvider",
    "BaoStockProvider",
    "EasyTdxProvider",
    "BarFrame",
    "SCALE",
    "create_provider",
    "normalize_bar_rows",
    "normalize_corporate_action_rows",
    "reconcile_corporate_actions",
    "normalize_instrument",
    "screen_bar_frames",
]
