//! 稳定契约单点（§7 M1「契约去重」/ P1-5 的落地）。
//!
//! 本模块做两件事，**都不引入新的类型语义**：
//!
//! 1. **稳定入口重导出**：把内核里已经是单点的稳定类型——身份 [`InstrumentId`] / [`MarketId`] /
//!    [`VenueId`]、合约规格 [`CanonicalProduct`] / [`TradingInstrumentSpec`]、事件元数据
//!    [`EventMetadata`] / [`EventContext`]、五元错误契约 [`ErrorCode`] / [`Retryability`] /
//!    [`ErrorContract`]、定点数值 [`Fixed`] / [`Money`] / [`Price`] / [`Quantity`]——**重导出**成一条
//!    稳定路径。全仓引用"稳定契约"时只认 `qx_core::contract::*`，不再各自 `use qx_core::identity::…`。
//!    定义点一个都没搬，入口收成一条。
//! 2. **命名转换矩阵** [`CONTRACT_MATRIX`]：跨 crate 的同名概念（`Bar` / `StrategyContext` /
//!    `DataProvider` / `RiskDecision`）**按 DD-1 不合并**——`qx-core` 是纯确定性内核，不允许反向依赖
//!    上层 crate；物理合并会把上层类型（`qx_data::Bar` 带 `String` instrument、`qx_runtime::StrategyContext`
//!    带 `StrategyResearchSnapshot`）拖进内核，等于把"纯内核"这条底线让掉。所以逐条登记
//!    「谁是规范单点、哪些是**同名兄弟**、由哪个**显式 adapter** 桥接」，让"重复"从**隐式的**
//!    （两处各写一份字段映射、谁也不知道还有第三处）变成**显式的、可核对的**。
//!
//! 矩阵是机器可读的（[`contract_matrix_json`]），由 `GET /schema/contract-matrix` 公布，并由门禁
//! `contract_matrix_check` 与真实代码逐条对账：规范单点在仓内**唯一**、同名兄弟真在盘、adapter
//! 真有生产读者、**没有未登记的第三个同名声明**、且「同名但刻意不同」的行必须写明理由。
//!
//! **刻意不做**：不把 `ApiProjectionKey` / `ExecutionEvent` / `ReconcileReportSnapshot` 搬进 `qx-core`。
//! 它们已各自单点，搬进来只会多一层转发，还会把 `qx-api` / `qx-execution` 的稳定 API 变成二手货。
//! 矩阵登记的是「它们是单点、在哪」，不是「它们该搬家」。

use serde::Serialize;

pub use crate::error::{ErrorCode, ErrorContract, Retryability};
pub use crate::event::{EventContext, EventMetadata, EVENT_METADATA_SCHEMA_VERSION};
pub use crate::identity::{
    CanonicalProduct, InstrumentId, MarketId, VenueId, DEFAULT_SETTLEMENT_CURRENCY,
};
pub use crate::numeric::{Fixed, Money, Price, Quantity, SCALE};
pub use crate::trading::TradingInstrumentSpec;

/// 命名转换矩阵的一行：一个概念、它的规范单点、它的同名兄弟、桥接它们的显式 adapter。
///
/// - `canonical_types`：规范单点的类型名，多个用 `,` 分隔，都必须在 `canonical_source` 里声明。
/// - `canonical_source`：规范单点所在文件（仓内相对路径）。
/// - `duplicates`：**同名兄弟**，每格写 `类型名@仓内相对路径`。空列表表示这个概念在仓内只有一处声明
///   ——矩阵仍然登记它，好让"唯一"也被核对，而不是靠"没人写下来"来保证。
/// - `adapter`：桥接两者的显式 adapter，写 `crate::module::fn`。空表示两者**刻意不同层、无需桥接**，
///   此时 `note` 必须写明理由（门禁核对这一点）。
/// - `note`：这一行为什么长这样。人读字段，也是「刻意不同」那类行的免责说明。
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
pub struct ContractConcept {
    pub concept: &'static str,
    pub canonical_types: &'static str,
    pub canonical_source: &'static str,
    pub duplicates: &'static [&'static str],
    pub adapter: &'static str,
    pub note: &'static str,
}

/// 命名转换矩阵（§7 M1）。**每行一行写完**——门禁按行解析它，别折行、别插注释。
pub const CONTRACT_MATRIX: &[ContractConcept] = &[
    ContractConcept { concept: "identity", canonical_types: "InstrumentId,MarketId,VenueId", canonical_source: "crates/qx-core/src/identity.rs", duplicates: &[], adapter: "", note: "交易场所与标的分层身份，内核单点" },
    ContractConcept { concept: "canonical_product", canonical_types: "CanonicalProduct", canonical_source: "crates/qx-core/src/identity.rs", duplicates: &[], adapter: "", note: "与市场状态分离的合约本体，内核单点" },
    ContractConcept { concept: "trading_instrument_spec", canonical_types: "TradingInstrumentSpec", canonical_source: "crates/qx-core/src/trading.rs", duplicates: &[], adapter: "", note: "合约规格，内核单点" },
    ContractConcept { concept: "event_metadata", canonical_types: "EventMetadata,EventContext", canonical_source: "crates/qx-core/src/event.rs", duplicates: &[], adapter: "", note: "事件元数据与因果上下文，内核单点" },
    ContractConcept { concept: "error_contract", canonical_types: "ErrorCode,Retryability,ErrorContract", canonical_source: "crates/qx-core/src/error.rs", duplicates: &[], adapter: "", note: "错误码五元契约（P1-11 / DD-5），内核单点" },
    ContractConcept { concept: "account_scope", canonical_types: "ApiProjectionKey", canonical_source: "crates/qx-api/src/lib.rs", duplicates: &[], adapter: "", note: "账户作用域键（account_id + venue_id），读面单点；刻意不搬进 qx-core" },
    ContractConcept { concept: "execution_fact", canonical_types: "ExecutionEvent", canonical_source: "crates/qx-execution/src/application.rs", duplicates: &[], adapter: "", note: "统一执行事实，执行层单点；刻意不搬进 qx-core" },
    ContractConcept { concept: "reconcile_report", canonical_types: "ReconcileReportSnapshot", canonical_source: "crates/qx-api/src/lib.rs", duplicates: &[], adapter: "", note: "对账报告读面，API 单点；刻意不搬进 qx-core" },
    ContractConcept { concept: "market_data_bar", canonical_types: "Bar", canonical_source: "crates/qx-guanxing/src/lib.rs", duplicates: &["Bar@crates/qx-data/src/schema.rs"], adapter: "qx_cli::contract_adapters::ingest_bar_matches_market_bar", note: "市场数据层是规范单点（无 instrument、列式 OHLCV）；数据摄取层的 Bar 多带 instrument 与原始列，adapter 逐列对账两者" },
    ContractConcept { concept: "strategy_context", canonical_types: "StrategyContext", canonical_source: "crates/qx-strategy/src/lib.rs", duplicates: &["StrategyContext@crates/qx-runtime/src/strategy_contract/context.rs"], adapter: "qx_cli::strategy_host::native_strategy_context", note: "原生策略面是规范单点；跨语言契约那一份多带 research 研究快照" },
    ContractConcept { concept: "data_provider", canonical_types: "DataProvider", canonical_source: "crates/qx-data/src/provider.rs", duplicates: &["DataProvider@crates/qx-provider/src/lib.rs"], adapter: "", note: "同名但刻意不同层：qx-data 的是摄取层按标的+区间取 Bars，qx-provider 的是供应商能力层 fetch(query)->ProviderResult；两者没有桥接也不该有" },
    ContractConcept { concept: "risk_decision", canonical_types: "RiskDecision", canonical_source: "crates/qx-risk/src/lib.rs", duplicates: &[], adapter: "", note: "风控裁决（Allow/Reject/Reduce），风控层单点" },
];

/// 矩阵只读视图。`GET /schema/contract-matrix` 与门禁读的都是它。
pub fn contract_matrix() -> &'static [ContractConcept] {
    CONTRACT_MATRIX
}

/// 矩阵的 JSON 形态，即 `GET /schema/contract-matrix` 的响应体。
pub fn contract_matrix_json() -> String {
    serde_json::to_string(contract_matrix()).expect("contract matrix serialization cannot fail")
}
