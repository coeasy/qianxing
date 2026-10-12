//! 可复用的 Bar 事件回测驱动器。
//!
//! CLI 只是入口；真正的回测闭环在这里固定为：as_of → 风控 → OMS →
//! 下一根 bar 撮合 → Fill → Ledger → EventLog。

use qx_core::{
    Event, EventKind, EventLog, FeeModel, Fill, Fnv1a, InstrumentId, Ledger, Money, Order,
    OrderStatus, Price, Priority, Quantity, ReplayVerifier, RunManifest, Side,
    TradingInstrumentSpec, ValuationContext, SCALE,
};
use qx_guanxing::{Bar, DataSourceId, DataView, QualityGate, Verdict};
use qx_risk::OrderRiskPosition;
use qx_zhenlu::{Oms, RiskGate};

use crate::ashare::{
    AshareCorporateActionType, AshareIssuerCapitalSnapshot, AshareRuleConfig, AshareSettlementState,
};
use crate::cost::{LatencyModel, MarginRule};
use crate::fill::{DataTier, FillModel};
use crate::venue::BarMatchingEngine;
use std::collections::BTreeMap;
pub mod strategy;
pub use strategy::{BarStrategy, NativeBarStrategy};
mod portfolio_strategy;
pub use portfolio_strategy::{PortfolioBarStrategy, PortfolioStrategySlot};
pub(crate) mod portfolio_budget;
use portfolio_budget::PortfolioBudgetState;
mod order_validation;
use order_validation::reduce_only_order_allowed;
mod protective_exit;
mod virtual_validation;
pub use protective_exit::{
    ProtectiveExitFill, ProtectiveExitPolicy, ProtectiveExitReason, ProtectiveExitSignal,
};
use virtual_validation::validate_virtual_events;

/// 回测中可重放的账户业务事件。事件时间必须使用与 Bar 相同的时间单位，
/// 费率使用基点；所有现金变化最终都要进入 Ledger，而不是直接改报告。
#[derive(Clone, Debug, Default)]
pub struct VirtualTradingConfig {
    pub funding: Vec<FundingEvent>,
    pub interest: Vec<InterestEvent>,
    pub delivery: Vec<DeliveryEvent>,
    pub enable_liquidation: bool,
    pub liquidation_fee_bp: i64,
    /// 资产币种到回测报告币种的显式汇率；缺少汇率时拒绝跨币种权益估值。
    pub fx_rates: BTreeMap<String, Price>,
    /// 除 `BacktestConfig.currency/initial_cash` 外的初始抵押品余额。
    pub collateral: BTreeMap<String, Money>,
    /// A 股专用交易规则；None 保持通用现货/衍生品兼容路径。
    pub ashare_rules: Option<AshareRuleConfig>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct FundingEvent {
    pub ts: u64,
    /// 多头支付为正，空头收取为负；结算金额按持仓名义额计算。
    pub rate_bp: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct InterestEvent {
    pub ts: u64,
    /// 账户现金按基点计息，可使用负值表达利息支出。
    pub rate_bp: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct DeliveryEvent {
    pub ts: u64,
    pub settlement_price: Price,
}

pub struct BacktestConfig {
    pub instrument: InstrumentId,
    /// 可选的市场规格；提供后使用合约数量、线性/反向 PnL 和衍生品现金流。
    /// None 保持历史 multiplier 兼容路径。
    pub instrument_spec: Option<TradingInstrumentSpec>,
    pub account_id: String,
    pub currency: String,
    pub initial_cash: Money,
    /// 合约乘数，**普通整数**（现货使用 1）；期货/永续必须显式传入市场规格中的乘数。
    /// 注意与 `TradingInstrumentSpec::contract_size` 不同：后者是 SCALE 标度定点值。
    pub multiplier: i128,
    pub fill: Box<dyn FillModel>,
    pub fee: Box<dyn FeeModel>,
    pub data_tier: DataTier,
    pub latency: Box<dyn LatencyModel>,
    pub margin: Box<dyn MarginRule>,
    pub seed: u64,
    pub risk: RiskGate,
    pub virtual_trading: VirtualTradingConfig,
}

pub struct BacktestReport {
    pub fills: Vec<qx_core::Fill>,
    pub equity: Vec<i128>,
    /// 现金基准曲线；没有外部基准序列时显式标记为 flat-cash。
    pub benchmark_equity: Vec<i128>,
    pub positions: Vec<i128>,
    /// Protective exits are keyed to their completed-bar trigger and actual next-bar fills.
    pub protective_exit_fills: Vec<ProtectiveExitFill>,
    /// 回测期间生效的发行人股本事实；它不属于账户 Ledger。
    pub issuer_capital_snapshots: Vec<AshareIssuerCapitalSnapshot>,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    pub max_drawdown_raw: i128, // max_drawdown_bps 的定点原值伴生格；仓库内零生产读者，见 capabilities.yaml local_backtest limitation
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    pub assumptions: Vec<String>,
    pub event_log: EventLog,
    pub ledger: Ledger,
    pub model_descriptors: Vec<String>,
    pub seed: u64,
    pub input_data_hash: u64,
    pub clock_start: u64,
    pub clock_end: u64,
}

pub struct RunManifestIdentity<'a> {
    pub run_id: &'a str,
    pub code_commit: &'a str,
    pub config_hash: &'a str,
    pub strategy_version: &'a str,
    pub instrument_spec_version: &'a str,
    pub runtime_version: &'a str,
}

impl BacktestReport {
    pub fn result_hash(&self) -> u64 {
        self.event_log.digest()
    }

    pub fn final_equity(&self) -> i128 {
        *self.equity.last().unwrap_or(&0)
    }

    /// 返回不晚于指定时间的最新发行人股本快照。
    pub fn issuer_capital_at(&self, ts: u64) -> Option<&AshareIssuerCapitalSnapshot> {
        self.issuer_capital_snapshots
            .iter()
            .filter(|snapshot| snapshot.effective_ts <= ts)
            .max_by_key(|snapshot| snapshot.effective_ts)
    }

    /// 用报告事实生成完整运行指纹；代码提交、策略版本和配置摘要由编排层注入。
    pub fn run_manifest(
        &self,
        run_id: &str,
        code_commit: &str,
        config_hash: &str,
        strategy_version: &str,
        instrument_spec_version: &str,
        runtime_version: &str,
    ) -> Result<RunManifest, String> {
        self.run_manifest_with_data_fingerprint(
            RunManifestIdentity {
                run_id,
                code_commit,
                config_hash,
                strategy_version,
                instrument_spec_version,
                runtime_version,
            },
            &format!("{:016x}", self.input_data_hash),
        )
    }

    /// 使用外部冻结数据指纹生成 RunManifest。默认方法仍以回测输入哈希
    /// 保持兼容；Bundle 回测应传入 dataset-bundle:<fingerprint>，从而
    /// 让运行清单能够证明策略实际绑定了哪份研究快照。
    pub fn run_manifest_with_data_fingerprint(
        &self,
        identity: RunManifestIdentity<'_>,
        data_fingerprint: &str,
    ) -> Result<RunManifest, String> {
        self.run_manifest_with_input_components(identity, data_fingerprint, BTreeMap::new())
    }

    /// 为运行清单附加每个数据组件的独立指纹。Bundle 回测必须使用该入口，
    /// 这样公司行为、交易日历等非 Bars 组件不会只被一个聚合 fingerprint 掩盖。
    pub fn run_manifest_with_input_components(
        &self,
        identity: RunManifestIdentity<'_>,
        data_fingerprint: &str,
        input_components: BTreeMap<String, String>,
    ) -> Result<RunManifest, String> {
        if data_fingerprint.trim().is_empty() {
            return Err("RunManifest data_fingerprint 不能为空".into());
        }
        let mut model_hash = Fnv1a::new();
        for descriptor in &self.model_descriptors {
            model_hash.write_text(descriptor);
        }
        let manifest = RunManifest {
            run_id: identity.run_id.into(),
            code_commit: identity.code_commit.into(),
            config_hash: identity.config_hash.into(),
            data_fingerprint: data_fingerprint.into(),
            input_components,
            clock_start: self.clock_start,
            clock_end: self.clock_end,
            global_seed: self.seed,
            determinism_mode: true,
            result_hash: format!("{:016x}", self.result_hash()),
            strategy_version: identity.strategy_version.into(),
            instrument_spec_version: identity.instrument_spec_version.into(),
            model_fingerprint: format!("{:016x}", model_hash.finish()),
            input_event_hash: format!("{:016x}", self.input_data_hash),
            output_event_hash: format!("{:016x}", self.result_hash()),
            runtime_version: identity.runtime_version.into(),
        };
        manifest.validate()?;
        Ok(manifest)
    }
}

pub struct BacktestEngine {
    config: BacktestConfig,
    protective_exits: Option<ProtectiveExitPolicy>,
}

impl BacktestEngine {
    pub fn new(config: BacktestConfig) -> Self {
        Self {
            config,
            protective_exits: None,
        }
    }

    pub fn with_protective_exits(
        mut self,
        policy: ProtectiveExitPolicy,
    ) -> Result<Self, qx_core::QxError> {
        policy.validate()?;
        self.protective_exits = Some(policy);
        Ok(self)
    }

    pub fn run(
        self,
        bars: &[Bar],
        strategy: &mut dyn BarStrategy,
    ) -> Result<BacktestReport, qx_core::QxError> {
        self.run_with_cancel(bars, strategy, || false)?
            .ok_or_else(|| qx_core::QxError::Invariant("未请求取消但回测未产生报告".into()))
    }

    /// Run the same deterministic engine with a cooperative cancellation check at
    /// every bar boundary. `None` means the caller requested cancellation; no
    /// partial report or artifacts are returned.
    pub fn run_with_cancel<F>(
        self,
        bars: &[Bar],
        strategy: &mut dyn BarStrategy,
        mut is_cancelled: F,
    ) -> Result<Option<BacktestReport>, qx_core::QxError>
    where
        F: FnMut() -> bool,
    {
        let quality = QualityGate::check(bars);
        if matches!(quality.verdict(), Verdict::Fail | Verdict::Quarantine) {
            return Err(qx_core::QxError::Permanent(format!(
                "回测输入未通过质量门: {:?}",
                quality.issues
            )));
        }
        let BacktestEngine {
            config,
            protective_exits,
        } = self;
        let BacktestConfig {
            instrument,
            instrument_spec,
            account_id,
            currency,
            initial_cash,
            multiplier,
            fill,
            fee,
            data_tier,
            latency,
            margin,
            seed,
            risk,
            virtual_trading,
        } = config;
        if multiplier <= 0 {
            return Err(qx_core::QxError::BusinessViolation(
                "回测合约乘数必须为正".into(),
            ));
        }
        if let Some(spec) = &instrument_spec {
            if spec.instrument != instrument {
                return Err(qx_core::QxError::BusinessViolation(
                    "回测产品规格 instrument 与配置不一致".into(),
                ));
            }
            spec.validate()?;
        }
        // 已指定规格时只认其中的 `contract_size`，拒绝叠加历史乘数以保持回测与实盘现金一致。
        if instrument_spec.is_some() && multiplier != 1 {
            return Err(qx_core::QxError::BusinessViolation(
                "已提供产品规格时回测乘数必须为 1，每手规模由 contract_size 决定".into(),
            ));
        }
        let derivative_spec = instrument_spec
            .as_ref()
            .filter(|spec| spec.product.supports_leverage());
        if !data_tier.supports(fill.tier()) {
            return Err(qx_core::QxError::BusinessViolation(format!(
                "撮合模型 {} 需要 {:?} 数据，但当前只有 {:?}",
                fill.name(),
                fill.tier(),
                data_tier
            )));
        }
        let mut model_descriptors = vec![
            fill.descriptor(),
            fee.descriptor(),
            latency.descriptor(),
            margin.descriptor(),
        ];
        if let Some(policy) = protective_exits {
            model_descriptors.push(format!(
                "protective-exits-v1:stop={:?}:take={:?}:trailing={:?}",
                policy.stop_loss_bps, policy.take_profit_bps, policy.trailing_stop_bps
            ));
        }
        let view =
            DataView::try_new(bars.to_vec(), DataSourceId::new("backtest")).map_err(|report| {
                qx_core::QxError::Permanent(format!("回测视图未通过质量门: {:?}", report.issues))
            })?;
        let input_data_hash = view.metadata().data_hash;
        // 将兼容路径的整数 multiplier 转为 SCALE 标度，避免小额手续费被截断为 0。
        let fee_price_multiplier = derivative_spec
            .map(|spec| spec.contract_size)
            .unwrap_or_else(|| multiplier.saturating_mul(SCALE));
        let mut matcher = BarMatchingEngine::new_with_latency_and_fee_multiplier(
            fill,
            fee,
            latency,
            seed,
            fee_price_multiplier,
        );
        // 反向合约的费用基准须匹配按 `spec.notional` 记账的名义额。
        matcher.set_inverse_fee_basis(derivative_spec.is_some_and(|spec| spec.inverse));
        let mut ledger = Ledger::new();
        let deposit_id = ledger.deposit(
            &account_id,
            &currency,
            initial_cash,
            bars.first().map(|b| b.ts).unwrap_or(0),
        )?;
        let mut oms = Oms::new();
        let mut log = EventLog::new();
        let mut fills = Vec::new();
        let mut protective_exit_fills = Vec::new();
        let mut portfolio_budget = PortfolioBudgetState::default();
        let mut position_policies = BTreeMap::<qx_core::PositionSide, qx_core::OrderPolicy>::new();
        let mut favorable_extremes = BTreeMap::<qx_core::PositionSide, i128>::new();
        let mut protective_order_meta = BTreeMap::<u64, (ProtectiveExitReason, u64)>::new();
        let mut equity = Vec::new();
        let mut benchmark_equity = Vec::new();
        let mut positions = Vec::new();
        let mut peak_equity = initial_cash.raw();
        let mut max_drawdown_raw = 0_i128;
        let mut next_id = 1_u64;
        validate_virtual_events(&virtual_trading)?;
        if let Some(rules) = virtual_trading.ashare_rules.as_ref() {
            rules
                .validate()
                .map_err(qx_core::QxError::BusinessViolation)?;
        }
        let ashare_rules = virtual_trading
            .ashare_rules
            .as_ref()
            .filter(|rules| rules.enabled);
        let issuer_capital_snapshots = ashare_rules
            .map(|rules| {
                rules
                    .issuer_capital_snapshots(
                        &instrument.to_string(),
                        bars.last().map(|bar| bar.ts),
                    )
                    .map_err(qx_core::QxError::BusinessViolation)
            })
            .transpose()?
            .unwrap_or_default();
        let mut ashare_state = AshareSettlementState::default();
        if let Some(rules) = ashare_rules {
            model_descriptors.push(rules.descriptor());
        }
        let deposit_entry = ledger
            .entries()
            .iter()
            .find(|entry| entry.id == deposit_id)
            .cloned()
            .ok_or_else(|| qx_core::QxError::Invariant("找不到初始入金 entry".into()))?;
        let deposit_seq = log.alloc_seq();
        log.append(Event::new(
            deposit_seq,
            deposit_entry.ts,
            Priority::APPLY,
            EventKind::LedgerApplied {
                entry: deposit_entry,
            },
        ));
        for (collateral_currency, amount) in &virtual_trading.collateral {
            let collateral_id = ledger.deposit(
                &account_id,
                collateral_currency,
                *amount,
                bars.first().map(|b| b.ts).unwrap_or(0),
            )?;
            let collateral_entry = ledger
                .entries()
                .iter()
                .find(|entry| entry.id == collateral_id)
                .cloned()
                .ok_or_else(|| qx_core::QxError::Invariant("找不到初始抵押品 entry".into()))?;
            let collateral_seq = log.alloc_seq();
            log.append(Event::new(
                collateral_seq,
                collateral_entry.ts,
                Priority::APPLY,
                EventKind::LedgerApplied {
                    entry: collateral_entry,
                },
            ));
        }

        for (i, bar) in bars.iter().enumerate() {
            if is_cancelled() {
                return Ok(None);
            }
            if let Some(rules) = ashare_rules {
                ashare_state.prepare(bar.ts);
                let previous_close = rules.previous_close(bars, i);
                matcher.set_halted(!rules.is_trading(bar.ts));
                matcher.set_side_blocks(
                    rules.blocks_fill(Side::Buy, bar, previous_close),
                    rules.blocks_fill(Side::Sell, bar, previous_close),
                );
            } else {
                matcher.set_halted(false);
                matcher.set_side_blocks(false, false);
            }
            let mut virtual_state = VirtualExecution {
                instrument: &instrument,
                account_id: &account_id,
                currency: &currency,
                multiplier,
                spec: derivative_spec,
                ledger: &mut ledger,
                log: &mut log,
                fills: &mut fills,
                fee_engine: None,
            };
            apply_virtual_events(&virtual_trading, bar, &mut virtual_state)?;
            if i > 0 {
                let history = view.as_of(bars[i - 1].ts);
                let position = ledger.position_for(&account_id, &instrument).quantity.raw();
                // Spot buying power is settled cash after prior fills/fees. This
                // value is visible at the decision boundary; derivative margin
                // remains unavailable until the configured margin model can
                // expose a certified free-margin snapshot.
                let available_margin_raw = derivative_spec
                    .is_none()
                    .then(|| ledger.cash_for(&account_id, &currency).max(0));
                // A protective exit evaluates the last completed bar and submits
                // a market close for the current bar. It never sees the current
                // bar's OHLC while deciding, preventing same-bar lookahead.
                protective_order_meta.retain(|order_id, _| {
                    oms.get(*order_id)
                        .is_some_and(|order| !order.status.is_terminal())
                });
                let mut orders = Vec::new();
                if let (Some(policy), Some(completed_bar)) = (protective_exits, history.last()) {
                    let mut active_sides = position_policies.keys().copied().collect::<Vec<_>>();
                    if active_sides.is_empty() && position != 0 {
                        active_sides.push(qx_core::PositionSide::Net);
                    }
                    for position_side in active_sides {
                        if !protective_order_meta.is_empty() {
                            break;
                        }
                        let position_state = if position_side == qx_core::PositionSide::Net {
                            ledger.position_for(&account_id, &instrument)
                        } else {
                            ledger.position_for_side(&account_id, &instrument, position_side)
                        };
                        let qty = position_state.quantity.raw();
                        if qty == 0 {
                            continue;
                        }
                        let signal = policy.evaluate(
                            qty,
                            position_state.average_entry.raw(),
                            completed_bar,
                            favorable_extremes.get(&position_side).copied(),
                        )?;
                        if let Some(signal) = signal {
                            let mut order_policy = position_policies
                                .get(&position_side)
                                .copied()
                                .unwrap_or_default();
                            order_policy.reduce_only = true;
                            order_policy.position_side = position_side;
                            let order = Order {
                                client_id: next_id,
                                instrument: instrument.clone(),
                                side: if qty > 0 { Side::Sell } else { Side::Buy },
                                qty: Quantity::from_raw(checked_abs(qty)?),
                                limit: None,
                                status: OrderStatus::PendingSubmit,
                                filled: Quantity::ZERO,
                                account_id: account_id.clone(),
                                trace: Some(qx_core::OrderTrace {
                                    strategy_id: Some("qx-protective-exit".into()),
                                    signal_id: None,
                                    intent_id: None,
                                    rule_version: Some(format!(
                                        "protective-exit-v1:{:?}:trigger={}",
                                        signal.reason, signal.trigger_price_raw
                                    )),
                                }),
                                policy: Some(order_policy),
                            };
                            protective_order_meta
                                .insert(next_id, (signal.reason, completed_bar.ts));
                            next_id = next_id.checked_add(1).ok_or_else(|| {
                                qx_core::QxError::Invariant("保护性退出订单序号溢出".into())
                            })?;
                            orders.push(order);
                            break;
                        }
                    }
                }
                if orders.is_empty() && protective_order_meta.is_empty() {
                    orders = strategy.on_bar_orders_with_margin_checked(
                        history,
                        &instrument,
                        bar.ts,
                        position,
                        available_margin_raw,
                    )?;
                }
                // All decisions at this timestamp share one account. Reserve funding
                // in deterministic strategy order before the orders reach the matcher,
                // otherwise siblings can each pass against the same stale cash balance.
                let mut reserved_funding_raw = 0_i128;
                let mut accepted_orders = Vec::new();
                for mut order in orders {
                    // Strategy intent IDs are local to a strategy. Once several
                    // strategies share one OMS, remap zero or colliding IDs into
                    // the engine's deterministic account-wide order namespace.
                    if order.client_id == 0 || oms.get(order.client_id).is_some() {
                        while next_id == 0 || oms.get(next_id).is_some() {
                            next_id = next_id.checked_add(1).ok_or_else(|| {
                                qx_core::QxError::Invariant("回测订单序号溢出".into())
                            })?;
                        }
                        order.client_id = next_id;
                        next_id = next_id.checked_add(1).ok_or_else(|| {
                            qx_core::QxError::Invariant("回测订单序号溢出".into())
                        })?;
                    } else if order.client_id >= next_id {
                        next_id = order.client_id.checked_add(1).ok_or_else(|| {
                            qx_core::QxError::Invariant("回测订单序号溢出".into())
                        })?;
                    }
                    if order.account_id.is_empty() {
                        order.account_id = account_id.clone();
                    }
                    let invalid_reason = if order.account_id != account_id {
                        Some("订单账户与回测账户不一致")
                    } else if order.instrument != instrument {
                        Some("订单标的与回测标的不一致")
                    } else {
                        None
                    };
                    let ashare_reason = ashare_rules.and_then(|rules| {
                        rules
                            .validate_order(&order, position, ashare_state.bought_today(), bar.ts)
                            .err()
                    });
                    if let Some(reason) = invalid_reason {
                        append_rejection(&mut log, bar.ts, order.client_id, reason);
                    } else if let Some(reason) = ashare_reason {
                        append_rejection(&mut log, bar.ts, order.client_id, &reason);
                    } else {
                        let product_policy = order.policy.unwrap_or_default();
                        let leverage = if let Some(spec) = derivative_spec {
                            product_policy.validate_for(spec)?;
                            product_policy.leverage
                        } else {
                            1
                        };
                        // Strategy decisions at Bar(t) only observe data through Bar(t-1).
                        // Pre-trade risk and margin therefore use the last visible mark,
                        // never the current bar close, which is future information here.
                        let visible_close = bars[i - 1].close;
                        let reference_price = order.limit.map(|p| p.raw()).unwrap_or(visible_close);
                        let order_qty = checked_abs(order.qty.raw())?;
                        let strategy_budget_check = portfolio_budget.order_budget_check(
                            |strategy_id| strategy.strategy_budget_raw(strategy_id),
                            &order,
                            reference_price,
                            instrument_spec.as_ref(),
                            multiplier,
                            margin.as_ref(),
                        )?;
                        let reduce_only_allowed = !product_policy.reduce_only
                            || reduce_only_order_allowed(
                                &ledger,
                                &account_id,
                                &instrument,
                                &order,
                                order_qty,
                            );
                        let required_margin = if let Some(spec) = derivative_spec {
                            let signed_delta = match order.side {
                                Side::Buy => order_qty,
                                Side::Sell => -order_qty,
                            };
                            let projected_position =
                                if product_policy.position_mode == qx_core::PositionMode::Hedge {
                                    ledger
                                        .position_for_side(
                                            &account_id,
                                            &instrument,
                                            product_policy.position_side,
                                        )
                                        .quantity
                                        .raw()
                                        .checked_add(signed_delta)
                                        .ok_or_else(|| {
                                            qx_core::QxError::Invariant(
                                                "回测 projected hedge position 溢出".into(),
                                            )
                                        })?
                                } else {
                                    position.checked_add(signed_delta).ok_or_else(|| {
                                        qx_core::QxError::Invariant(
                                            "回测 projected position 溢出".into(),
                                        )
                                    })?
                                };
                            let margin_qty =
                                if product_policy.position_mode == qx_core::PositionMode::Hedge {
                                    let current_long = ledger
                                        .position_for_side(
                                            &account_id,
                                            &instrument,
                                            qx_core::PositionSide::Long,
                                        )
                                        .quantity
                                        .raw();
                                    let current_short = ledger
                                        .position_for_side(
                                            &account_id,
                                            &instrument,
                                            qx_core::PositionSide::Short,
                                        )
                                        .quantity
                                        .raw();
                                    let long = if product_policy.position_side
                                        == qx_core::PositionSide::Long
                                    {
                                        projected_position
                                    } else {
                                        current_long
                                    };
                                    let short = if product_policy.position_side
                                        == qx_core::PositionSide::Short
                                    {
                                        projected_position
                                    } else {
                                        current_short
                                    };
                                    checked_abs(long)?.saturating_add(checked_abs(short)?)
                                } else {
                                    checked_abs(projected_position)?
                                };
                            margin.instrument_initial_margin(
                                spec,
                                margin_qty,
                                checked_abs(reference_price)?,
                                leverage,
                            )?
                        } else {
                            margin.initial_margin(notional_for(
                                derivative_spec,
                                order_qty,
                                checked_abs(reference_price)?,
                                multiplier,
                            )?)
                        };
                        let marks = std::collections::BTreeMap::from([(
                            instrument.clone(),
                            Price::from_raw(visible_close),
                        )]);
                        let equity = equity_for(
                            &ledger,
                            &account_id,
                            &marks,
                            &currency,
                            multiplier,
                            derivative_spec,
                            &virtual_trading.fx_rates,
                        )?;
                        // 现货买入的资金下限只能是**可用现金**：权益含持仓未实现盈亏，
                        // 用它放行等于让策略拿已有持仓当现金继续买，Ledger 会静默透支成
                        // 负现金。衍生品/保证金交易仍按权益判定，与订单簿回测同一分支规则。
                        //
                        // 现货买入的资金需求是**全额名义额**，与 `MarginRule` 无关：
                        // 保证金模型描述的是衍生品分级，`NoMargin` 让需求归零等于给现货
                        // 账户免费杠杆。手续费同理由前置门禁承担，避免"校验时以为免费、
                        // 成交时才扣款"。
                        let cash_funded_spot_buy = derivative_spec.is_none()
                            && order.side == Side::Buy
                            && !product_policy.reduce_only;
                        let required_funding = if cash_funded_spot_buy {
                            let notional = notional_for(
                                derivative_spec,
                                order_qty,
                                checked_abs(reference_price)?,
                                multiplier,
                            )?;
                            notional
                                .checked_add(matcher.estimate_fee(
                                    order_qty,
                                    checked_abs(reference_price)?,
                                    &order,
                                ))
                                .ok_or_else(|| {
                                    qx_core::QxError::Invariant("买入所需现金溢出".into())
                                })?
                                .max(required_margin)
                        } else {
                            required_margin
                        };
                        let available_funding = if cash_funded_spot_buy {
                            ledger.cash_for(&account_id, &currency)
                        } else {
                            equity
                        };
                        let unreserved_funding = available_funding
                            .checked_sub(reserved_funding_raw)
                            .unwrap_or(i128::MIN);
                        if !reduce_only_allowed {
                            append_rejection(
                                &mut log,
                                bar.ts,
                                order.client_id,
                                "reduce-only order would open or exceed the existing position",
                            );
                        } else if strategy_budget_check
                            .is_some_and(|(budget, required)| required > budget)
                        {
                            append_rejection(
                                &mut log,
                                bar.ts,
                                order.client_id,
                                "订单会使策略持仓需求超过 reserved_capital_raw",
                            );
                        } else if required_funding > unreserved_funding {
                            append_rejection(
                                &mut log,
                                bar.ts,
                                order.client_id,
                                if cash_funded_spot_buy {
                                    "买入成本（含预估手续费）超过账户可用现金"
                                } else {
                                    "initial margin exceeds equity"
                                },
                            );
                        } else {
                            let long_qty = ledger
                                .position_for_side(
                                    &account_id,
                                    &instrument,
                                    qx_core::PositionSide::Long,
                                )
                                .quantity
                                .raw();
                            let short_qty = ledger
                                .position_for_side(
                                    &account_id,
                                    &instrument,
                                    qx_core::PositionSide::Short,
                                )
                                .quantity
                                .raw();
                            let one_way_qty = position
                                .checked_sub(long_qty)
                                .and_then(|value| value.checked_sub(short_qty))
                                .ok_or_else(|| {
                                    qx_core::QxError::Invariant(
                                        "拆分回测 one-way/hedge 持仓数量溢出".into(),
                                    )
                                })?;
                            let mark_abs = checked_abs(visible_close)?;
                            let one_way_notional = notional_for(
                                derivative_spec,
                                checked_abs(one_way_qty)?,
                                mark_abs,
                                multiplier,
                            )?;
                            let long_notional = notional_for(
                                derivative_spec,
                                checked_abs(long_qty)?,
                                mark_abs,
                                multiplier,
                            )?;
                            let short_notional = notional_for(
                                derivative_spec,
                                checked_abs(short_qty)?,
                                mark_abs,
                                multiplier,
                            )?;
                            let gross_notional = one_way_notional
                                .checked_add(long_notional)
                                .and_then(|value| value.checked_add(short_notional))
                                .ok_or_else(|| {
                                    qx_core::QxError::Invariant(
                                        "回测 hedge gross notional 溢出".into(),
                                    )
                                })?;
                            if matches!(order.status, OrderStatus::PendingSubmit) {
                                order.status = OrderStatus::Submitted;
                            }
                            let risk_position = OrderRiskPosition::new_with_multiplier(
                                one_way_qty,
                                gross_notional,
                                // `OrderRiskPosition::multiplier` 沿用 `BacktestConfig::multiplier`
                                // 的**普通整数**口径（上游 `PositionSnapshot` 的定点口径不适用），
                                // 名义额折算只走 `TradingInstrumentSpec::notional` 一份公式。
                                multiplier,
                            )
                            .with_hedge_legs(long_qty, short_qty);
                            let risk_result = risk.check_with_spec(
                                &order,
                                &risk_position,
                                Some(Price::from_raw(reference_price)),
                                instrument_spec.as_ref(),
                            );
                            if let Err(error) = risk_result {
                                append_rejection(
                                    &mut log,
                                    bar.ts,
                                    order.client_id,
                                    &error.to_string(),
                                );
                            } else if let Err(error) = oms.submit(order.clone()) {
                                append_rejection(
                                    &mut log,
                                    bar.ts,
                                    order.client_id,
                                    &error.to_string(),
                                );
                            } else if let Err(error) = oms.accept(order.client_id) {
                                append_rejection(
                                    &mut log,
                                    bar.ts,
                                    order.client_id,
                                    &error.to_string(),
                                );
                            } else {
                                portfolio_budget.accept_if_owned(
                                    |strategy_id| strategy.strategy_budget_raw(strategy_id),
                                    &order,
                                    reference_price,
                                )?;
                                reserved_funding_raw = reserved_funding_raw
                                    .checked_add(required_funding)
                                    .ok_or_else(|| {
                                        qx_core::QxError::Invariant(
                                            "同 Bar 组合资金预留溢出".into(),
                                        )
                                    })?;
                                let submit_seq = log.alloc_seq();
                                log.append(Event::new(
                                    submit_seq,
                                    bar.ts,
                                    qx_core::Priority::COMMAND,
                                    EventKind::Submit {
                                        client_order_id: order.client_id,
                                    },
                                ));
                                accepted_orders.push(order);
                            }
                        }
                    }
                }
                // EventLog enforces timestamp/priority ordering. Emit all command
                // decisions above before any matcher acceptance at MATCH priority.
                for order in accepted_orders {
                    let accepted_seq = log.alloc_seq();
                    log.append(Event::new(
                        accepted_seq,
                        bar.ts,
                        qx_core::Priority::MATCH,
                        EventKind::Accepted {
                            client_order_id: order.client_id,
                            venue_order_id: None,
                        },
                    ));
                    matcher.submit_at(order, bar.ts);
                }
            }
            for mut fill in matcher.on_bar(bar, bar.ts) {
                let order = oms
                    .get(fill.order_id)
                    .cloned()
                    .ok_or_else(|| qx_core::QxError::ReconcileRequired("成交找不到订单".into()))?;
                if let Some(spec) = instrument_spec.as_ref() {
                    spec.validate_fill(fill.qty.raw(), fill.price.raw())?;
                }
                order.trace_fill(&mut fill, None, None);
                portfolio_budget.validate_fill_budget(
                    |strategy_id| strategy.strategy_budget_raw(strategy_id),
                    &order,
                    &fill,
                    instrument_spec.as_ref(),
                    multiplier,
                    margin.as_ref(),
                )?;
                if let Some((reason, trigger_ts)) =
                    protective_order_meta.get(&fill.order_id).copied()
                {
                    protective_exit_fills.push(ProtectiveExitFill {
                        reason,
                        trigger_ts,
                        fill_ts: fill.ts,
                        order_id: fill.order_id,
                        qty_raw: fill.qty.raw(),
                        price_raw: fill.price.raw(),
                    });
                }
                if let Some(policy) = order.policy {
                    position_policies.insert(policy.position_side, policy);
                } else {
                    position_policies
                        .insert(qx_core::PositionSide::Net, qx_core::OrderPolicy::default());
                }
                let terms = qx_core::FillTerms::resolve(instrument_spec.as_ref(), multiplier);
                let entry_ids =
                    qx_core::apply_fill_to_books(&mut ledger, &mut oms, &currency, &fill, terms)?;
                portfolio_budget.apply_fill_if_owned(
                    |strategy_id| strategy.strategy_budget_raw(strategy_id),
                    &order,
                    &fill,
                )?;
                if ashare_rules.is_some() && order.side == Side::Buy {
                    ashare_state.on_buy(fill.qty.raw());
                }
                let fill_seq = log.alloc_seq();
                log.append(Event::new(
                    fill_seq,
                    fill.ts,
                    Priority::APPLY,
                    EventKind::Filled { fill: fill.clone() },
                ));
                for entry_id in entry_ids {
                    let entry_seq = log.alloc_seq();
                    let entry = ledger
                        .entries()
                        .iter()
                        .find(|entry| entry.id == entry_id)
                        .cloned()
                        .ok_or_else(|| {
                            qx_core::QxError::Invariant("找不到已应用账簿 entry".into())
                        })?;
                    log.append(Event::new(
                        entry_seq,
                        fill.ts,
                        Priority::APPLY,
                        EventKind::LedgerApplied { entry },
                    ));
                }
                strategy.on_fill(&order, &fill)?;
                fills.push(fill);
            }
            if protective_exits.is_some() {
                let sides = position_policies.keys().copied().collect::<Vec<_>>();
                for position_side in sides {
                    let state = if position_side == qx_core::PositionSide::Net {
                        ledger.position_for(&account_id, &instrument)
                    } else {
                        ledger.position_for_side(&account_id, &instrument, position_side)
                    };
                    let qty = state.quantity.raw();
                    if qty == 0 {
                        position_policies.remove(&position_side);
                        favorable_extremes.remove(&position_side);
                    } else {
                        let extreme = if qty > 0 { bar.high } else { bar.low };
                        favorable_extremes
                            .entry(position_side)
                            .and_modify(|value| {
                                *value = if qty > 0 {
                                    (*value).max(extreme)
                                } else {
                                    (*value).min(extreme)
                                };
                            })
                            .or_insert(extreme);
                    }
                }
            }
            let mut marks = std::collections::BTreeMap::new();
            marks.insert(instrument.clone(), Price::from_raw(bar.close));
            let mut marked_equity = equity_for(
                &ledger,
                &account_id,
                &marks,
                &currency,
                multiplier,
                derivative_spec,
                &virtual_trading.fx_rates,
            )?;
            if virtual_trading.enable_liquidation {
                let current_position = ledger.position_for(&account_id, &instrument).quantity.raw();
                let (long_leg, short_leg) = if derivative_spec.is_some() {
                    let long = ledger
                        .position_for_side(&account_id, &instrument, qx_core::PositionSide::Long)
                        .quantity
                        .raw();
                    let short = ledger
                        .position_for_side(&account_id, &instrument, qx_core::PositionSide::Short)
                        .quantity
                        .raw();
                    (long, short)
                } else {
                    (0, 0)
                };
                let gross_qty = if long_leg != 0 || short_leg != 0 {
                    checked_abs(long_leg)?.saturating_add(checked_abs(short_leg)?)
                } else {
                    checked_abs(current_position)?
                };
                let gross_notional = notional_for(
                    derivative_spec,
                    gross_qty,
                    checked_abs(bar.close)?,
                    multiplier,
                )?;
                let maintenance_margin = if let Some(spec) = derivative_spec {
                    margin.instrument_maintenance_margin(
                        spec,
                        gross_qty,
                        checked_abs(bar.close)?,
                    )?
                } else {
                    margin.maintain_margin(gross_notional)
                };
                if gross_qty != 0 && marked_equity < maintenance_margin {
                    let mut virtual_state = VirtualExecution {
                        instrument: &instrument,
                        account_id: &account_id,
                        currency: &currency,
                        multiplier,
                        spec: derivative_spec,
                        ledger: &mut ledger,
                        log: &mut log,
                        fills: &mut fills,
                        fee_engine: Some(&matcher),
                    };
                    // 只有真的持有双向腿时才按腿平仓：单向（Net）持仓在双向账本里
                    // 记为 0，用 `derivative_spec` 判定分支会让单向衍生持仓的强平
                    // 变成两次空操作——权益永远停在维持保证金之下却不平仓。
                    if derivative_spec.is_some() && (long_leg != 0 || short_leg != 0) {
                        for (position_side, leg) in [
                            (qx_core::PositionSide::Long, long_leg),
                            (qx_core::PositionSide::Short, short_leg),
                        ] {
                            close_virtual_position(
                                &mut virtual_state,
                                leg,
                                Price::from_raw(bar.close),
                                bar.ts,
                                Some(virtual_trading.liquidation_fee_bp),
                                Some(position_side),
                            )?;
                        }
                    } else {
                        close_virtual_position(
                            &mut virtual_state,
                            current_position,
                            Price::from_raw(bar.close),
                            bar.ts,
                            Some(virtual_trading.liquidation_fee_bp),
                            None,
                        )?;
                    }
                    marked_equity = equity_for(
                        &ledger,
                        &account_id,
                        &marks,
                        &currency,
                        multiplier,
                        derivative_spec,
                        &virtual_trading.fx_rates,
                    )?;
                }
            }
            peak_equity = peak_equity.max(marked_equity);
            max_drawdown_raw = max_drawdown_raw.max(peak_equity.saturating_sub(marked_equity));
            equity.push(marked_equity);
            benchmark_equity.push(initial_cash.raw());
            positions.push(ledger.position_for(&account_id, &instrument).quantity.raw());
        }
        log.validate()?;
        // 费用与换手按成交事实汇总，而不是在撮合循环里累加：强平、交割的成交由
        // 虚拟事件路径直接记账，不经过该循环，循环内累加会让报告统计与账本现金
        // 对不上。
        let mut fees_raw = 0_i128;
        let mut turnover_raw = 0_i128;
        for fill in &fills {
            fees_raw = fees_raw
                .checked_add(fill.fee.raw())
                .ok_or_else(|| qx_core::QxError::Invariant("手续费累计溢出".into()))?;
            turnover_raw = turnover_raw
                .checked_add(notional_for(
                    derivative_spec,
                    checked_abs(fill.qty.raw())?,
                    checked_abs(fill.price.raw())?,
                    multiplier,
                )?)
                .ok_or_else(|| qx_core::QxError::Invariant("换手累计溢出".into()))?;
        }
        let final_equity = *equity.last().unwrap_or(&initial_cash.raw());
        let return_bps = if initial_cash.raw() > 0 {
            final_equity
                .saturating_sub(initial_cash.raw())
                .saturating_mul(10_000)
                .checked_div(initial_cash.raw())
                .unwrap_or(0)
                .clamp(i128::from(i32::MIN), i128::from(i32::MAX)) as i32
        } else {
            0
        };
        let max_drawdown_bps = if peak_equity > 0 {
            max_drawdown_raw
                .saturating_mul(10_000)
                .checked_div(peak_equity)
                .unwrap_or(0)
                .clamp(0, 10_000) as u32
        } else {
            0
        };
        let report = BacktestReport {
            fills,
            protective_exit_fills,
            equity,
            benchmark_equity,
            positions,
            issuer_capital_snapshots,
            fees_raw,
            turnover_raw,
            max_drawdown_raw,
            return_bps,
            max_drawdown_bps,
            assumptions: vec![
                format!("data_tier={data_tier:?}"),
                "benchmark=flat-cash".into(),
                "metrics=raw-fixed-point".into(),
                format!(
                    "virtual_trading=funding:{} interest:{} delivery:{} liquidation:{}",
                    virtual_trading.funding.len(),
                    virtual_trading.interest.len(),
                    virtual_trading.delivery.len(),
                    virtual_trading.enable_liquidation
                ),
            ],
            event_log: log,
            ledger,
            model_descriptors,
            seed,
            input_data_hash,
            clock_start: bars.first().map(|bar| bar.ts).unwrap_or(0),
            clock_end: bars.last().map(|bar| bar.ts).unwrap_or(0),
        };
        // 事实必须能重新驱动账户：就地改了账簿而没有对应事实事件，报告在这里就出不去（V11 Q62）。
        ReplayVerifier::verify(report.event_log.events(), &report.ledger)?;
        Ok(Some(report))
    }
}

fn notional_for(
    spec: Option<&TradingInstrumentSpec>,
    qty: i128,
    price: i128,
    multiplier: i128,
) -> Result<i128, qx_core::QxError> {
    if let Some(spec) = spec {
        spec.notional(qty, price)
    } else {
        Ok(qx_core::notional(qty, price).saturating_mul(multiplier))
    }
}

fn equity_for(
    ledger: &Ledger,
    account_id: &str,
    marks: &std::collections::BTreeMap<InstrumentId, Price>,
    currency: &str,
    multiplier: i128,
    spec: Option<&TradingInstrumentSpec>,
    fx_rates: &BTreeMap<String, Price>,
) -> Result<i128, qx_core::QxError> {
    // 「该用哪把尺子」的派发单源在 `Ledger::valuate`（P1-6 估值单点）：这里只把回测输入收进
    // 上下文，再取权益一个字段。此前这段派发在本文件与 orderbook_backtest 各抄一份。
    let context = ValuationContext::spot(account_id, marks, currency)
        .with_spec(spec)
        .with_multiplier(multiplier)
        .with_fx(fx_rates);
    Ok(ledger.valuate(&context)?.equity.raw())
}

fn append_ledger_entry(
    log: &mut EventLog,
    ledger: &Ledger,
    entry_id: u64,
    ts: u64,
) -> Result<(), qx_core::QxError> {
    let entry = ledger
        .entries()
        .iter()
        .find(|entry| entry.id == entry_id)
        .cloned()
        .ok_or_else(|| qx_core::QxError::Invariant("找不到虚拟交易账簿 entry".into()))?;
    let seq = log.alloc_seq();
    log.append(Event::new(
        seq,
        ts,
        Priority::APPLY,
        EventKind::LedgerApplied { entry },
    ));
    Ok(())
}

fn append_virtual_fill(
    log: &mut EventLog,
    ledger: &Ledger,
    fill: &Fill,
    entry_ids: &[u64],
) -> Result<(), qx_core::QxError> {
    let fill_seq = log.alloc_seq();
    log.append(Event::new(
        fill_seq,
        fill.ts,
        Priority::APPLY,
        EventKind::Filled { fill: fill.clone() },
    ));
    for entry_id in entry_ids {
        append_ledger_entry(log, ledger, *entry_id, fill.ts)?;
    }
    Ok(())
}

struct VirtualExecution<'a> {
    instrument: &'a InstrumentId,
    account_id: &'a str,
    currency: &'a str,
    multiplier: i128,
    spec: Option<&'a TradingInstrumentSpec>,
    ledger: &'a mut Ledger,
    log: &'a mut EventLog,
    fills: &'a mut Vec<Fill>,
    /// 强制平仓用的费用口径。None 表示按无佣金结算（交割），Some 表示与
    /// 策略成交共用同一撮合器的费率模型。
    fee_engine: Option<&'a BarMatchingEngine>,
}

pub(crate) fn is_cash_dividend_action(action_type: AshareCorporateActionType) -> bool {
    matches!(
        action_type,
        AshareCorporateActionType::CashDividend
            | AshareCorporateActionType::BonusShare
            | AshareCorporateActionType::CapitalTransfer
            | AshareCorporateActionType::Unknown
    )
}

fn is_convertible_bond_interest_action(action_type: AshareCorporateActionType) -> bool {
    action_type == AshareCorporateActionType::ConvertibleBondInterest
}

fn is_convertible_bond_settlement_action(action_type: AshareCorporateActionType) -> bool {
    matches!(
        action_type,
        AshareCorporateActionType::ConvertibleBondRedemption
            | AshareCorporateActionType::ConvertibleBondCall
            | AshareCorporateActionType::ConvertibleBondPut
    )
}

fn close_virtual_position(
    state: &mut VirtualExecution<'_>,
    position: i128,
    price: Price,
    ts: u64,
    liquidation_fee_bp: Option<i64>,
    position_side: Option<qx_core::PositionSide>,
) -> Result<(), qx_core::QxError> {
    if position == 0 {
        return Ok(());
    }
    let client_id = u64::MAX.saturating_sub(state.log.events().len() as u64);
    let qty = checked_abs(position)?;
    let order = Order {
        client_id,
        instrument: state.instrument.clone(),
        side: if position > 0 { Side::Sell } else { Side::Buy },
        qty: Quantity::from_raw(qty),
        limit: Some(price),
        status: OrderStatus::Accepted,
        filled: Quantity::ZERO,
        account_id: state.account_id.into(),
        trace: None,
        policy: position_side.map(|position_side| qx_core::OrderPolicy {
            reduce_only: true,
            position_side,
            margin_mode: qx_core::MarginMode::Cross,
            position_mode: qx_core::PositionMode::Hedge,
            leverage: 1,
            post_only: false,
        }),
    };
    // 强平成交必须与策略成交收同一份佣金：忽略费率会让爆仓路径凭空免掉
    // 手续费，回测出"亏损被佣金补贴"的系统性乐观偏差。合成订单不是 post-only，
    // 因此按 taker 计费。
    let price_abs = checked_abs(price.raw())?;
    let commission_raw = state
        .fee_engine
        .map(|engine| engine.estimate_fee(qty, price_abs, &order).max(0))
        .unwrap_or(0);
    let fill = Fill {
        order_id: client_id,
        qty: Quantity::from_raw(qty),
        price,
        fee: Money::from_raw(commission_raw),
        ts,
        account_id: state.account_id.into(),
        ..Fill::default()
    };
    let terms = qx_core::FillTerms::resolve(state.spec, state.multiplier);
    let entry_ids = qx_core::apply_ledger_fill(state.ledger, &order, state.currency, &fill, terms)?;
    append_virtual_fill(state.log, state.ledger, &fill, &entry_ids)?;
    state.fills.push(fill);
    if let Some(fee_bp) = liquidation_fee_bp {
        let notional = notional_for(state.spec, qty, checked_abs(price.raw())?, state.multiplier)?;
        let penalty = qx_core::bp_amount(notional, fee_bp);
        if penalty > 0 {
            let entry_id = state.ledger.apply_liquidation(
                state.account_id,
                state.currency,
                Money::from_raw(-penalty),
                ts,
            )?;
            append_ledger_entry(state.log, state.ledger, entry_id, ts)?;
        }
    }
    Ok(())
}

fn apply_virtual_events(
    config: &VirtualTradingConfig,
    bar: &Bar,
    state: &mut VirtualExecution<'_>,
) -> Result<(), qx_core::QxError> {
    if let Some(rules) = config.ashare_rules.as_ref().filter(|rules| rules.enabled) {
        // 现金分红按登记日（或没有登记日时的除权日）生成待支付权益，
        // 不在此处直接增加现金，避免支付日按错误的当前持仓重新计算。
        for event in rules.corporate_actions.iter().filter(|event| {
            is_cash_dividend_action(event.action_type)
                && event.cash_dividend_raw > 0
                && (event.record_ts == Some(bar.ts)
                    || (event.record_ts.is_none()
                        && event.payment_ts.is_some()
                        && event.ts == bar.ts))
        }) {
            if let Some(entry_id) = state.ledger.grant_cash_dividend_entitlement(
                state.account_id,
                state.instrument,
                state.currency,
                event.cash_dividend_raw,
                bar.ts,
            )? {
                append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }

        // 可转债利息在登记日锁定债券持仓，支付日只结算已登记的权益。
        for event in rules.corporate_actions.iter().filter(|event| {
            is_convertible_bond_interest_action(event.action_type)
                && event.interest_per_bond_raw > 0
                && (event.record_ts == Some(bar.ts)
                    || (event.record_ts.is_none()
                        && event.payment_ts.is_some()
                        && event.ts == bar.ts))
        }) {
            let bond_instrument = event
                .convertible_bond_instrument
                .as_deref()
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation(
                        "可转债利息事件缺少 convertible_bond_instrument".into(),
                    )
                })
                .and_then(|value| {
                    InstrumentId::parse(value).ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "可转债利息 convertible_bond_instrument 格式非法".into(),
                        )
                    })
                })?;
            if let Some(entry_id) = state.ledger.grant_convertible_bond_interest_entitlement(
                state.account_id,
                &bond_instrument,
                state.currency,
                event.interest_per_bond_raw,
                bar.ts,
            )? {
                append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }

        // 登记日只授予独立权利，不能在除权日重新读取持仓推导配额。
        for event in rules.corporate_actions.iter().filter(|event| {
            event.action_type == AshareCorporateActionType::RightsIssue
                && event.record_ts == Some(bar.ts)
        }) {
            let rights_instrument = event
                .rights_instrument
                .as_deref()
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation("配股登记事件缺少 rights_instrument".into())
                })
                .and_then(|value| {
                    InstrumentId::parse(value).ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "配股登记 rights_instrument 格式非法".into(),
                        )
                    })
                })?;
            let current_position = state
                .ledger
                .position_for(state.account_id, state.instrument)
                .quantity
                .raw();
            let entitled_qty = current_position
                .checked_mul(event.rights_issue_ratio_num)
                .and_then(|value| value.checked_div(event.rights_issue_ratio_den))
                .ok_or_else(|| qx_core::QxError::Invariant("配股配额计算溢出".into()))?;
            let entry_id = state.ledger.grant_rights_entitlement(
                state.account_id,
                state.instrument,
                &rights_instrument,
                state.currency,
                entitled_qty,
                bar.ts,
            )?;
            append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
        }

        for event in rules.corporate_actions.iter().filter(|event| {
            (event.ts == bar.ts
                && (event.record_ts.is_none()
                    || event.subscription_start_ts.is_none()
                    || event.subscription_start_ts == Some(bar.ts))
                && !(event.action_type == AshareCorporateActionType::ConvertibleBondIssue
                    && event.subscription_start_ts.is_some())
                && !is_convertible_bond_interest_action(event.action_type)
                && !(is_convertible_bond_settlement_action(event.action_type)
                    && event.payment_ts.is_some()))
                || (event.action_type == AshareCorporateActionType::RightsIssue
                    && event.subscription_start_ts == Some(bar.ts))
                || (event.action_type == AshareCorporateActionType::ConvertibleBondIssue
                    && event.subscription_start_ts == Some(bar.ts))
                || (is_convertible_bond_settlement_action(event.action_type)
                    && event.payment_ts == Some(bar.ts))
        }) {
            let entry_ids = match event.action_type {
                AshareCorporateActionType::CashDividend
                | AshareCorporateActionType::BonusShare
                | AshareCorporateActionType::CapitalTransfer
                | AshareCorporateActionType::Unknown => state.ledger.apply_corporate_action(
                    state.account_id,
                    state.instrument,
                    state.currency,
                    qx_core::CorporateAction {
                        cash_dividend_raw: if event.record_ts.is_some()
                            || event.payment_ts.is_some()
                        {
                            0
                        } else {
                            event.cash_dividend_raw
                        },
                        split_num: event.split_num,
                        split_den: event.split_den,
                    },
                    bar.ts,
                )?,
                AshareCorporateActionType::RightsIssue => {
                    let rights_instrument = event
                        .rights_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "配股事件缺少 rights_instrument，不能隐式使用普通股标的".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "配股 rights_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    if event.record_ts.is_some() {
                        if event.subscription_qty_raw == 0 {
                            Vec::new()
                        } else {
                            state
                                .ledger
                                .apply_rights_issue_subscription_from_entitlement(
                                    state.account_id,
                                    &rights_instrument,
                                    state.currency,
                                    event.subscription_qty_raw,
                                    event.rights_issue_price_raw,
                                    bar.ts,
                                )?
                        }
                    } else {
                        let current_position = state
                            .ledger
                            .position_for(state.account_id, state.instrument)
                            .quantity
                            .raw();
                        let entitled_qty = current_position
                            .checked_mul(event.rights_issue_ratio_num)
                            .and_then(|value| value.checked_div(event.rights_issue_ratio_den))
                            .ok_or_else(|| {
                                qx_core::QxError::Invariant("配股配额计算溢出".into())
                            })?;
                        state.ledger.apply_rights_issue_event(
                            state.account_id,
                            qx_core::RightsIssueEvent {
                                source_instrument: state.instrument.clone(),
                                rights_instrument,
                                currency: state.currency.to_owned(),
                                entitled_qty_raw: entitled_qty,
                                subscription_qty_raw: event.subscription_qty_raw,
                                subscription_price_raw: event.rights_issue_price_raw,
                            },
                            bar.ts,
                        )?
                    }
                }
                AshareCorporateActionType::RightsIssueExpiry => {
                    let rights_instrument = event
                        .rights_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "配股权利失效事件缺少 rights_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "配股权利失效 rights_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    vec![state.ledger.expire_rights_entitlement(
                        state.account_id,
                        &rights_instrument,
                        event.rights_expiry_qty_raw,
                        state.currency,
                        bar.ts,
                    )?]
                }
                AshareCorporateActionType::NewShareIssue => {
                    state.ledger.apply_new_share_subscription(
                        state.account_id,
                        state.instrument,
                        state.currency,
                        event.subscription_qty_raw,
                        event.issue_price_raw,
                        bar.ts,
                    )?
                }
                AshareCorporateActionType::ConvertibleBondIssue => {
                    let bond_instrument = event
                        .convertible_bond_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债发行事件缺少 convertible_bond_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债发行 convertible_bond_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    state.ledger.apply_convertible_bond_issue_subscription(
                        state.account_id,
                        &bond_instrument,
                        state.currency,
                        event.subscription_qty_raw,
                        event.issue_price_raw,
                        bar.ts,
                    )?
                }
                AshareCorporateActionType::ConvertibleBondRedemption
                | AshareCorporateActionType::ConvertibleBondPut => {
                    let bond_instrument = event
                        .convertible_bond_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债回售/赎回事件缺少 convertible_bond_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债回售/赎回 convertible_bond_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    state.ledger.apply_convertible_bond_tender(
                        state.account_id,
                        &bond_instrument,
                        state.currency,
                        event.settlement_qty_raw,
                        event.settlement_price_raw,
                        bar.ts,
                    )?
                }
                AshareCorporateActionType::ConvertibleBondCall => {
                    let bond_instrument = event
                        .convertible_bond_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债强赎事件缺少 convertible_bond_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债强赎 convertible_bond_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    let quantity_raw = if event.settlement_qty_raw > 0 {
                        event.settlement_qty_raw
                    } else {
                        state
                            .ledger
                            .position_for(state.account_id, &bond_instrument)
                            .quantity
                            .raw()
                    };
                    if quantity_raw <= 0 {
                        Vec::new()
                    } else {
                        state.ledger.apply_convertible_bond_tender(
                            state.account_id,
                            &bond_instrument,
                            state.currency,
                            quantity_raw,
                            event.settlement_price_raw,
                            bar.ts,
                        )?
                    }
                }
                AshareCorporateActionType::Repurchase => state.ledger.apply_repurchase_tender(
                    state.account_id,
                    state.instrument,
                    state.currency,
                    event.repurchase_qty_raw,
                    event.repurchase_price_raw,
                    bar.ts,
                )?,
                AshareCorporateActionType::ConvertibleBondConversion => {
                    let bond_instrument = event
                        .convertible_bond_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债转股事件缺少 convertible_bond_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债 convertible_bond_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    let target_instrument = event
                        .conversion_target_instrument
                        .as_deref()
                        .ok_or_else(|| {
                            qx_core::QxError::BusinessViolation(
                                "可转债转股事件缺少 conversion_target_instrument".into(),
                            )
                        })
                        .and_then(|value| {
                            InstrumentId::parse(value).ok_or_else(|| {
                                qx_core::QxError::BusinessViolation(
                                    "可转债 conversion_target_instrument 格式非法".into(),
                                )
                            })
                        })?;
                    state.ledger.apply_convertible_bond_conversion(
                        state.account_id,
                        state.currency,
                        qx_core::ConvertibleBondConversion {
                            bond_instrument,
                            target_instrument,
                            bond_qty_raw: event.conversion_qty_raw,
                            target_qty_raw: event.conversion_target_qty_raw,
                            conversion_price_raw: event.conversion_price_raw,
                        },
                        bar.ts,
                    )?
                }
                AshareCorporateActionType::CapitalChange => {
                    // 发行人股本是显式市场事实，已在 BacktestReport 中保存；
                    // 这里不能生成账户 Ledger entry，也不能按持仓比例猜测。
                    Vec::new()
                }
                unsupported => {
                    return Err(qx_core::QxError::BusinessViolation(format!(
                        "A 股公司行为 {:?} 需要发行人/登记结算事实，当前禁止静默回测",
                        unsupported
                    )));
                }
            };
            for entry_id in entry_ids {
                append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }

        // 认购截止日自动使剩余权利失效；若数据源另有显式失效事实，
        // 以显式事件为准，避免同一权利被重复扣减。
        for event in rules.corporate_actions.iter().filter(|event| {
            event.action_type == AshareCorporateActionType::RightsIssue
                && event.subscription_end_ts == Some(bar.ts)
                && !rules.corporate_actions.iter().any(|expiry| {
                    expiry.action_type == AshareCorporateActionType::RightsIssueExpiry
                        && expiry.ts == bar.ts
                        && expiry.rights_instrument == event.rights_instrument
                })
        }) {
            let rights_instrument = event
                .rights_instrument
                .as_deref()
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation("配股截止事件缺少 rights_instrument".into())
                })
                .and_then(|value| {
                    InstrumentId::parse(value).ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "配股截止 rights_instrument 格式非法".into(),
                        )
                    })
                })?;
            let remaining = state
                .ledger
                .rights_entitlement_for(state.account_id, &rights_instrument)
                .raw();
            if remaining > 0 {
                let entry_id = state.ledger.expire_rights_entitlement(
                    state.account_id,
                    &rights_instrument,
                    remaining,
                    state.currency,
                    bar.ts,
                )?;
                append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }

        // 支付日只结算登记日已经锁定的权益；没有 payment_date 但存在
        // record_date 时，除权日作为兼容的支付时点。
        for _event in rules.corporate_actions.iter().filter(|event| {
            is_cash_dividend_action(event.action_type)
                && (event.record_ts.is_some() || event.payment_ts.is_some())
                && event.payment_ts.unwrap_or(event.ts) == bar.ts
        }) {
            if let Some(entry_id) = state.ledger.settle_cash_dividend_entitlement(
                state.account_id,
                state.instrument,
                state.currency,
                bar.ts,
            )? {
                append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }
        for event in rules.corporate_actions.iter().filter(|event| {
            is_convertible_bond_interest_action(event.action_type)
                && (event.record_ts.is_some() || event.payment_ts.is_some())
                && event.payment_ts.unwrap_or(event.ts) == bar.ts
        }) {
            let bond_instrument = event
                .convertible_bond_instrument
                .as_deref()
                .ok_or_else(|| {
                    qx_core::QxError::BusinessViolation(
                        "可转债利息支付事件缺少 convertible_bond_instrument".into(),
                    )
                })
                .and_then(|value| {
                    InstrumentId::parse(value).ok_or_else(|| {
                        qx_core::QxError::BusinessViolation(
                            "可转债利息支付 convertible_bond_instrument 格式非法".into(),
                        )
                    })
                })?;
            if let Some(entry_id) = state.ledger.settle_convertible_bond_interest_entitlement(
                state.account_id,
                &bond_instrument,
                state.currency,
                bar.ts,
            )? {
                append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
            }
        }
    }
    // 公司行为可能改变数量；后续资金费按归约后的最新持仓计算，避免
    // 在同一根 bar 上继续使用事件前的旧快照。
    let position = state
        .ledger
        .position_for(state.account_id, state.instrument)
        .quantity
        .raw();
    for event in config.funding.iter().filter(|event| event.ts == bar.ts) {
        if position == 0 {
            continue;
        }
        let amount = if let Some(spec) = state.spec {
            spec.funding_payment(position, bar.close, event.rate_bp)?
                .saturating_neg()
        } else {
            let notional = notional_for(
                state.spec,
                checked_abs(position)?,
                checked_abs(bar.close)?,
                state.multiplier,
            )?;
            let signed = if position > 0 { -1 } else { 1 };
            qx_core::bp_amount(notional, event.rate_bp).saturating_mul(signed)
        };
        if amount != 0 {
            let entry_id = state.ledger.apply_funding(
                state.account_id,
                state.currency,
                Money::from_raw(amount),
                bar.ts,
            )?;
            append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
        }
    }
    for event in config.interest.iter().filter(|event| event.ts == bar.ts) {
        let cash = state.ledger.cash_for(state.account_id, state.currency);
        let amount = qx_core::bp_amount(cash, event.rate_bp);
        if amount != 0 {
            let entry_id = state.ledger.apply_interest(
                state.account_id,
                state.currency,
                Money::from_raw(amount),
                bar.ts,
            )?;
            append_ledger_entry(state.log, state.ledger, entry_id, bar.ts)?;
        }
    }
    for event in config.delivery.iter().filter(|event| event.ts == bar.ts) {
        let settle_seq = state.log.alloc_seq();
        state.log.append(Event::new(
            settle_seq,
            bar.ts,
            Priority::APPLY,
            EventKind::Settle,
        ));
        let current = state
            .ledger
            .position_for(state.account_id, state.instrument)
            .quantity
            .raw();
        let long_leg = state
            .ledger
            .position_for_side(
                state.account_id,
                state.instrument,
                qx_core::PositionSide::Long,
            )
            .quantity
            .raw();
        let short_leg = state
            .ledger
            .position_for_side(
                state.account_id,
                state.instrument,
                qx_core::PositionSide::Short,
            )
            .quantity
            .raw();
        // 单向（Net）持仓在双向账本里两条腿都是 0，"按腿结算"等于什么都不平；
        // 与强平路径共用同一分支规则。
        if state.spec.is_some() && (long_leg != 0 || short_leg != 0) {
            for (position_side, leg) in [
                (qx_core::PositionSide::Long, long_leg),
                (qx_core::PositionSide::Short, short_leg),
            ] {
                close_virtual_position(
                    state,
                    leg,
                    event.settlement_price,
                    bar.ts,
                    None,
                    Some(position_side),
                )?;
            }
        } else {
            close_virtual_position(state, current, event.settlement_price, bar.ts, None, None)?;
        }
    }
    Ok(())
}

fn checked_abs(value: i128) -> Result<i128, qx_core::QxError> {
    value
        .checked_abs()
        .ok_or_else(|| qx_core::QxError::BusinessViolation("i128 绝对值溢出".into()))
}

fn append_rejection(log: &mut EventLog, ts: u64, client_order_id: u64, reason: &str) {
    let seq = log.alloc_seq();
    log.append(Event::new(
        seq,
        ts,
        qx_core::Priority::COMMAND,
        EventKind::Rejected {
            client_order_id,
            reason: reason.into(),
        },
    ));
}

#[cfg(test)]
#[path = "backtest/tests.rs"]
mod tests;
