"""数据层：数据集与合约身份（方案 §6.2 接口分层）。

这一层只有"数据是什么"和"它能不能用"：`DatasetSpec` 描述一份输入，`DatasetVerdict`
是**裁定**（`usable: false` 是结论，不是错误），`InstrumentId` / `TradingInstrumentSpec`
描述被交易的东西。

**可选适配器刻意不在这里 import**：`qianxing.ccxt` 与 `qianxing.ashare` 是各自独立的
可选依赖面（`ccxt` / `a-share-*` extras）。方案 §6.2 的原话是「optional dependencies
不改变核心语义」——所以 `import qianxing.data` 在一条 extras 都没装的环境里必须照样成功，
本模块因此一行都不碰那两个发行包。

数据**身份与质量**的判定全在 Rust（`qx-data`）；这一层只搬类型，不做校验逻辑。
"""

from qianxing.app import DatasetSpec, DatasetVerdict, InstrumentId, TradingInstrumentSpec
from qianxing.data.providers import (
    BAR_JSON_CONTENT_TYPE,
    FUNDING_RATE_JSON_CONTENT_TYPE,
    ORDER_BOOK_SNAPSHOT_JSON_CONTENT_TYPE,
    ProviderAdapter,
    ProviderAdapterError,
    ProviderCapability,
    ProviderDatasetBacktestOutcome,
    ProviderDepthBacktestOutcome,
    ProviderManualInterventionError,
    ProviderPayloadRow,
    ProviderPermanentError,
    ProviderQuery,
    ProviderRegistry,
    ProviderResponse,
)

__all__ = [
    "DatasetSpec",
    "DatasetVerdict",
    "InstrumentId",
    "TradingInstrumentSpec",
    "BAR_JSON_CONTENT_TYPE",
    "FUNDING_RATE_JSON_CONTENT_TYPE",
    "ORDER_BOOK_SNAPSHOT_JSON_CONTENT_TYPE",
    "ProviderAdapter",
    "ProviderAdapterError",
    "ProviderCapability",
    "ProviderDatasetBacktestOutcome",
    "ProviderDepthBacktestOutcome",
    "ProviderManualInterventionError",
    "ProviderPayloadRow",
    "ProviderPermanentError",
    "ProviderQuery",
    "ProviderRegistry",
    "ProviderResponse",
]
