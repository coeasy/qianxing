//! Strict versioned contract for deterministic Tick and OrderBook backtests.

use super::{
    is_identity_token, supports_fixed_notional_sizing, BacktestArtifacts, BacktestRiskRules,
    BuiltinStrategySpec, PortfolioBacktestConfig,
};
use crate::error::{AppError, AppErrorCategory};
use qx_core::{OrderPolicy, TradingInstrumentSpec};
use qx_strategy::BuiltinStrategyKind;
use qx_xingban::{ExecutionCostRules, MarketMakingConfig, QueuePositionModel};
use serde::{Deserialize, Serialize};

/// Schema for deterministic L1/L2 market-data backtests.
pub const DEPTH_BACKTEST_SPEC_SCHEMA_VERSION: u32 = 1;

/// L1 frames execute through the Rust Tick engine; L2 frames use the Rust book engine.
///
/// 自 WP-2 起，这条链的**全部**输入都走这份契约：门面（CLI）把 `--config` 解析成下面这些
/// 已解析的值与来源标签，用例只消费它们。这样"拒绝面"只有一处——`--config` 里声明了深度
/// 内核接不住的口径（A 股规则、Bar 撮合模型、Bar 成交量上限、非零成本延迟）时，CLI / HTTP /
/// Python 三个入口拿到的是同一条 `InvalidInput`，而不是各自在门面里抄一份判断。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DepthBacktestSpec {
    pub schema_version: u32,
    pub run_id: String,
    /// `RunManifest` 的身份，缺省沿用 [`Self::run_id`]。
    ///
    /// CLI 的 `backtest book` 用它保留 `depth-backtest:<tier>:<kind>:<instrument>` 这个形状——
    /// 离线复算（`run-evidence verify`）按该形状解析证据包，形状一改，历史证据包全部失效。
    /// 它不属于 `run_id` 的字符集（含 `:`），所以单独一格。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub manifest_run_id: Option<String>,
    pub depth_path: String,
    /// Exactly `l1` or `l2`; this determines the Rust matching engine.
    pub tier: String,
    pub settlement_currency: String,
    pub initial_cash_raw: i128,
    /// 吃单费率。`Some` 是显式点名（CLI 的 `--fee-bps`，摘要据此记 `cli-flag`）；
    /// `None` 表示从 [`Self::execution_cost_rules`] 的 `taker_bp` 取，再缺省取内核默认。
    /// 三级优先级（旗标 > 成本文件 > 内核默认）只在用例里判一次。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fee_bps: Option<i128>,
    pub latency_snapshots: u64,
    pub queue_position_bps: i128,
    /// Optional L2 passive-order queue estimate; defaults to legacy execution behavior.
    #[serde(default, skip_serializing_if = "is_queue_position_model_disabled")]
    pub queue_position_model: QueuePositionModel,
    pub market_impact_bps: i128,
    pub output_dir: String,
    pub strategy: BuiltinStrategySpec,
    /// Optional normalized market contract. Missing preserves legacy spot semantics.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_spec: Option<TradingInstrumentSpec>,
    /// Required for derivative products so the Rust engine can apply margin/position rules.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub order_policy: Option<OrderPolicy>,
    /// Optional L2 two-sided quote strategy executed by the shared Rust book engine.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_making: Option<MarketMakingConfig>,
    /// Ledger/account ownership for this run. Omitted legacy specs use `app`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// Same-account strategy portfolio over the shared Tick/OrderBook ledger.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub portfolio: Option<PortfolioBacktestConfig>,
    /// 已解析的执行成本规则（门面读文件，用例只消费值）。
    ///
    /// 深度内核只有单一吃单费率：`taker_bp` 是 [`Self::fee_bps`] 缺省时的兜底。延迟口径是
    /// `latency_snapshots`（按快照个数），所以**非零的 `latency_base_ns` / `latency_insert_ns`
    /// 在这里被拒绝**——收下再静默丢掉，就是把"能配"演成"生效"（V11 Q0c）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_cost_rules: Option<ExecutionCostRules>,
    /// 成本口径来源（门面解析）：`cost-rules-file:<path>` / `runtime-config-default` /
    /// `builtin-default`。显式 `fee_bps` 时摘要记成 `cli-flag`，不读这一格。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub execution_cost_source: Option<String>,
    /// 配置里声明的风控规则；`None` 表示保守默认。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk_rules: Option<BacktestRiskRules>,
    /// 解析层判定的允许做空；缺省按现货保守禁空。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub allow_short: Option<bool>,
    /// 配置里声明了 A 股规则（`ashare_rules_path` / `ashare_actions_path` /
    /// `ashare_calendar_path` 任一）。深度内核没有 T+1、整手与涨跌停的挂钩点，声明即拒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_ashare_rules: Option<String>,
    /// 配置里声明的撮合模型名。深度链的撮合口径是四参数描述子，没有 `FillModel` 落点，声明即拒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_fill_model: Option<String>,
    /// 配置里声明的 Bar 成交量参与上限。它只适用于 OHLCV Bar 回测，声明即拒。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub declared_bar_volume_participation_bps: Option<u16>,
    /// 可选数据质量清单：用例自己复核它与本帧的身份、档位与时间轴（不是照抄声明）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quality_manifest_path: Option<String>,
    /// runtime 配置路径：只作产物指针与来源判定，用例不读它（门面已经解析完了）。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub runtime_config_path: Option<String>,
    /// 外部 market spec 路径：只作产物指针。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_spec_path: Option<String>,
    /// market spec 的来源标签（门面按实际读入的 JSON 形状判定）。缺省沿用本用例的历史口径。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub market_spec_source: Option<String>,
    /// 账户本金的来源标签（V11 Q72 的四格之一）。缺省 `builtin-default`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_base_source: Option<String>,
    /// 生效信号口径的来源标签（`config` / `builtin-default`）。缺省 `builtin-default`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_source: Option<String>,
    /// 配置声明了、本 kind 的信号不读的旋钮（`none` = 声明的都上场）。缺省 `none`。
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub signal_declared_unused: Option<String>,
}

impl DepthBacktestSpec {
    /// Validate the command's strategy/execution dimensions before a facade opens input files.
    /// The complete spec validator calls this same function, so early CLI diagnostics cannot
    /// drift from HTTP/Python validation.
    pub fn validate_entry(
        tier: &str,
        kind: BuiltinStrategyKind,
        quantity_raw: i128,
        fee_bps: Option<i128>,
    ) -> Result<(), AppError> {
        if !matches!(tier, "l1" | "l2") {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.tier 仅支持 l1 或 l2",
            ));
        }
        if quantity_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.quantity_raw 必须为正",
            ));
        }
        if fee_bps.is_some_and(|bps| !(0..=10_000).contains(&bps)) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "深度回测 --fee-bps 必须在 0..=10000 内",
            ));
        }
        if kind.needs_reference_leg() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "深度档位回测只支持单标的策略，多腿配对请使用 multi-builtin：{}",
                    kind.name()
                ),
            ));
        }
        Ok(())
    }

    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != DEPTH_BACKTEST_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DepthBacktestSpec schema_version={} 本版本只认 {}",
                    self.schema_version, DEPTH_BACKTEST_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        if !is_identity_token(&self.run_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.run_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        if self
            .account_id
            .as_deref()
            .is_some_and(|account_id| !is_identity_token(account_id))
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.account_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符",
            ));
        }
        for (field, value) in [
            ("depth_path", &self.depth_path),
            ("settlement_currency", &self.settlement_currency),
            ("output_dir", &self.output_dir),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("DepthBacktestSpec.{field} 不能为空"),
                ));
            }
        }
        Self::validate_entry(
            &self.tier,
            self.strategy.kind,
            self.strategy.quantity_raw,
            self.fee_bps,
        )?;
        if let Some(market_making) = self.market_making {
            if self.portfolio.is_some() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "DepthBacktestSpec 暂不允许把 market_making 与策略组合混用",
                ));
            }
            if self.tier != "l2" {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "market_making 仅支持 L2 订单簿回测",
                ));
            }
            market_making
                .validate()
                .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
            if self
                .market_spec
                .as_ref()
                .is_some_and(|market| market.price_tick != market_making.tick_size_raw)
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "market_making.tick_size_raw 必须与 market_spec.price_tick 一致",
                ));
            }
            if strategy_has_sizing(&self.strategy) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "market_making 使用 quote_qty_raw 控制报价数量，不接受策略仓位 sizing 参数",
                ));
            }
        }
        if self.initial_cash_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DepthBacktestSpec.initial_cash_raw 必须为正",
            ));
        }
        for (field, value) in [
            ("queue_position_bps", self.queue_position_bps),
            ("market_impact_bps", self.market_impact_bps),
        ] {
            if !(0..=10_000).contains(&value) {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("DepthBacktestSpec.{field} 必须在 0..=10000 内"),
                ));
            }
        }
        // 四条"配置声明了、本链没有落点"的口径，必须在任何产物落盘之前拒（V11 Q61/Q0c/R11）。
        // 顺序沿用门面历史：先判配置侧声明，再判成本规则里的延迟。
        if let Some(declared) = self.declared_ashare_rules.as_deref() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "深度档位回测不接受 strategy.ashare_rules_path（含 ashare_actions_path / ashare_calendar_path）：L1/L2 盘口引擎没有 T+1、整手与涨跌停的挂钩点（声明值 {declared}）。需要 A 股规则口径请改用 strategy backtest 或 backtest builtin"
                ),
            ));
        }
        if let Some(declared) = self.declared_fill_model.as_deref() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "深度档位回测不接受 strategy.fill_model={declared}：L1/L2 盘口引擎的撮合口径是四参数描述子，没有 FillModel 落点。需要撮合模型口径请改用 strategy backtest 或 backtest builtin"
                ),
            ));
        }
        if let Some(declared) = self.declared_bar_volume_participation_bps {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "深度档位回测不接受 strategy.max_bar_volume_participation_bps={declared}：该约束只适用于 OHLCV Bar 回测"
                ),
            ));
        }
        // 成本规则里的延迟没有落点：这一链的延迟口径是 `latency_snapshots`（按快照个数）。
        // 收下再静默忽略，产物会自称吃了这份成本文件而实际延迟为 0（V11 Q0c）。
        if let Some(costs) = self.execution_cost_rules.as_ref() {
            if costs.latency_base_ns != 0 || costs.latency_insert_ns != 0 {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!(
                        "深度档回测不接受成本规则里的延迟设置（{}），它的延迟口径是 latency_snapshots（按快照个数）；请把 latency_base_ns/latency_insert_ns 置 0，或用 latency_snapshots 表达深度延迟",
                        self.execution_cost_source
                            .as_deref()
                            .unwrap_or("execution_cost_rules")
                    ),
                ));
            }
        }
        // Only the optional L2 market-making strategy submits passive limit orders.
        if (self.queue_position_bps != 0
            || self.queue_position_model != QueuePositionModel::Disabled)
            && self.market_making.is_none()
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "队列位置参数仅影响 L2 market_making 限价单；当前策略只提交市价单",
            ));
        }
        if self.queue_position_model != QueuePositionModel::Disabled && self.tier != "l2" {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "QueuePositionModel 仅支持 L2 订单簿回测",
            ));
        }
        self.queue_position_model
            .validate()
            .map_err(|error| AppError::new(AppErrorCategory::InvalidInput, error))?;
        match (&self.market_spec, self.order_policy) {
            (Some(market), policy) => {
                market.validate().map_err(|error| {
                    AppError::new(
                        AppErrorCategory::InvalidInput,
                        format!("DepthBacktestSpec.market_spec 非法: {error:?}"),
                    )
                })?;
                if market.settlement_currency != self.settlement_currency {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "DepthBacktestSpec.market_spec settlement_currency 与请求不一致",
                    ));
                }
                if self.strategy.target_notional_raw.is_some()
                    && !supports_fixed_notional_sizing(market)
                {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "target_notional_raw 当前只支持现货的一单位线性名义金额；衍生品需先配置合约专用 sizing",
                    ));
                }
                if market.product.supports_leverage() && policy.is_none() {
                    return Err(AppError::new(
                        AppErrorCategory::InvalidInput,
                        "Tick/OrderBook 衍生品回测必须显式提供 OrderPolicy",
                    ));
                }
                if let Some(policy) = policy {
                    if policy.reduce_only || policy.post_only {
                        return Err(AppError::new(
                            AppErrorCategory::InvalidInput,
                            "DepthBacktestSpec 的内置策略只提交开仓市价意图；OrderPolicy.reduce_only/post_only 必须为 false",
                        ));
                    }
                    policy.validate_for(market).map_err(|error| {
                        AppError::new(
                            AppErrorCategory::InvalidInput,
                            format!("DepthBacktestSpec.order_policy 非法: {error:?}"),
                        )
                    })?;
                }
            }
            (None, Some(_)) => {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "DepthBacktestSpec.order_policy 需要对应的 market_spec",
                ));
            }
            (None, None) => {}
        }
        self.strategy.validate()?;
        if let Some(portfolio) = &self.portfolio {
            portfolio.validate(&self.strategy, self.initial_cash_raw)?;
            if self
                .order_policy
                .is_some_and(|policy| policy.position_mode != qx_core::PositionMode::OneWay)
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Tick/OrderBook 策略组合目前要求 OneWay 持仓模式",
                ));
            }
        }
        let strategies = std::iter::once(&self.strategy).chain(
            self.portfolio
                .iter()
                .flat_map(|portfolio| portfolio.additional_strategies.iter())
                .map(|member| &member.strategy),
        );
        for strategy in strategies {
            if self
                .market_spec
                .as_ref()
                .is_some_and(|market| market.product.supports_leverage())
                && (strategy.target_available_margin_bps.is_some()
                    || strategy.target_volatility_bps.is_some()
                    || strategy.target_kelly_fraction_bps.is_some())
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "Tick/OrderBook 保证金 sizing 需要已认证的衍生品 free-margin 快照；当前仅现货现金 sizing 可用",
                ));
            }
            if strategy.target_notional_raw.is_some()
                && self
                    .market_spec
                    .as_ref()
                    .is_some_and(|market| !supports_fixed_notional_sizing(market))
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "target_notional_raw 当前只支持现货的一单位线性名义金额；衍生品需先配置合约专用 sizing",
                ));
            }
        }
        Ok(())
    }

    /// 解析后的吃单费率：显式 `fee_bps` > 成本规则 `taker_bp` > 内核默认。
    ///
    /// 三级优先级只在这一处判：门面各自 `unwrap_or` 一次，就会出现"同一份配置在 book 上
    /// 用一个费率、在 multi-builtin 上用另一个"（V11 Q0c）。
    pub fn resolved_fee_bps(&self) -> i128 {
        match self.fee_bps {
            Some(fee_bps) => fee_bps,
            None => self
                .execution_cost_rules
                .as_ref()
                .map(|rules| i128::from(rules.taker_bp))
                .unwrap_or_else(|| i128::from(ExecutionCostRules::default().taker_bp)),
        }
    }

    /// 生效的允许做空：门面已判定就照用，否则按标的产品形态回落——衍生品允许，现货保守禁空。
    ///
    /// 这一格必须与 Bar 链同源（`BacktestSpec` 的同一回落规则）：两条链各自判一次，
    /// 同一份 `strategy.risk_rules` 就会得到两个 `rule_set_version`（V10 §4.2）。
    pub fn allow_short(&self) -> bool {
        self.allow_short.unwrap_or_else(|| {
            self.market_spec
                .as_ref()
                .is_some_and(|market| market.product.supports_leverage())
        })
    }

    /// 成本口径来源：显式旗标记 `cli-flag`，否则沿用门面解析出的来源标签。
    pub fn resolved_cost_source(&self) -> String {
        if self.fee_bps.is_some() {
            return "cli-flag".to_string();
        }
        self.execution_cost_source
            .clone()
            .unwrap_or_else(|| "builtin-default".to_string())
    }

    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DepthBacktestSpec JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("DepthBacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

#[cfg(test)]
mod entry_validation_tests {
    use super::DepthBacktestSpec;
    use qx_strategy::BuiltinStrategyKind;

    #[test]
    fn entry_validation_has_one_closed_failure_contract() {
        let single_leg = BuiltinStrategyKind::ALL
            .iter()
            .copied()
            .find(|kind| !kind.needs_reference_leg())
            .expect("single-leg strategy exists");
        let multi_leg = BuiltinStrategyKind::ALL
            .iter()
            .copied()
            .find(|kind| kind.needs_reference_leg())
            .expect("multi-leg strategy exists");

        assert!(DepthBacktestSpec::validate_entry("l1", single_leg, 1, None).is_ok());
        assert!(DepthBacktestSpec::validate_entry("l3", single_leg, 1, None)
            .unwrap_err()
            .to_string()
            .contains("tier"));
        assert!(DepthBacktestSpec::validate_entry("l1", single_leg, 0, None)
            .unwrap_err()
            .to_string()
            .contains("quantity"));
        assert!(
            DepthBacktestSpec::validate_entry("l1", single_leg, 1, Some(10_001))
                .unwrap_err()
                .to_string()
                .contains("fee-bps")
        );
        assert!(DepthBacktestSpec::validate_entry("l1", multi_leg, 1, None)
            .unwrap_err()
            .to_string()
            .contains("只支持单标的策略"));
    }
}

fn is_queue_position_model_disabled(model: &QueuePositionModel) -> bool {
    *model == QueuePositionModel::Disabled
}

fn strategy_has_sizing(strategy: &BuiltinStrategySpec) -> bool {
    strategy.target_notional_raw.is_some()
        || strategy.target_available_margin_bps.is_some()
        || strategy.target_volatility_bps.is_some()
        || strategy.target_kelly_fraction_bps.is_some()
}

/// Stable result for an L1 Tick or L2 order-book run.
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DepthBacktestOutcome {
    pub run_id: String,
    pub instrument: String,
    pub tier: String,
    pub result_hash: String,
    pub data_fingerprint: String,
    pub fills: u64,
    pub equity_points: u64,
    /// 本轮的输入快照数：深度链的"样本数"，报告与 stdout 共用这一格。
    #[serde(default)]
    pub snapshots: u64,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    /// 四份产物的路径：run manifest / summary / equity / fills。
    pub artifacts: BacktestArtifacts,
    /// RunManifest 的内容摘要：`[RunManifest] digest=` 那一行与证据包核对共用它。
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub manifest_digest: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
    /// 摘要的内存投影仅供完成本次调用的门面打印；不进入稳定 JSON 结果契约。
    #[serde(skip)]
    pub display_summary: serde_json::Value,
}

impl PartialEq for DepthBacktestOutcome {
    fn eq(&self, other: &Self) -> bool {
        self.run_id == other.run_id
            && self.instrument == other.instrument
            && self.tier == other.tier
            && self.result_hash == other.result_hash
            && self.data_fingerprint == other.data_fingerprint
            && self.fills == other.fills
            && self.equity_points == other.equity_points
            && self.snapshots == other.snapshots
            && self.return_bps == other.return_bps
            && self.max_drawdown_bps == other.max_drawdown_bps
            && self.artifacts == other.artifacts
            && self.manifest_digest == other.manifest_digest
            && self.account_id == other.account_id
    }
}

impl Eq for DepthBacktestOutcome {}

impl DepthBacktestOutcome {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DepthBacktestOutcome JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("DepthBacktestOutcome 序列化失败: {error}"),
            )
        })
    }
}
