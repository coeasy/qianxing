//! 可复用的 Bar 事件回测驱动器；CLI 只是入口，闭环固定为 as_of → 风控 → OMS → 下一根 bar 撮合 → Fill → Ledger → EventLog。

use qx_core::{
    Event, EventKind, EventLog, FeeModel, Fill, Fnv1a, InstrumentId, Ledger, Money, Order,
    OrderStatus, Price, Priority, Quantity, QxError, ReplayVerifier, RunManifest, Side,
    TradingInstrumentSpec, ValuationContext, SCALE,
};
use qx_guanxing::{Bar, DataSourceId, DataView};
use qx_zhenlu::{Oms, RiskGate};

use crate::ashare::{
    trading::{AshareCorporateActionSchedule, AshareSessionIndex},
    AshareCorporateActionType, AshareIssuerCapitalSnapshot, AshareRuleConfig,
    AshareSettlementState,
};
use crate::cost::{LatencyModel, MarginRule};
use crate::fill::{DataTier, FillModel};
use crate::venue::BarMatchingEngine;
use std::collections::BTreeMap;
mod report;
pub use report::BacktestReport;
#[path = "backtest/currency.rs"]
mod currency;
pub mod strategy;
pub use strategy::{BarStrategy, NativeBarStrategy};
mod portfolio_strategy;
pub use portfolio_strategy::{PortfolioBarStrategy, PortfolioStrategySlot};
mod market_router;
pub use market_router::MarketRoutedBarStrategy;
mod portfolio_session;
pub use portfolio_session::{
    PortfolioBarEngine, PortfolioBarMarket, PortfolioBarSessionConfig, PortfolioBarSessionReport,
};
pub(crate) mod portfolio_budget;
#[cfg(test)]
#[path = "backtest/portfolio_session_tests.rs"]
mod portfolio_session_tests;
use portfolio_budget::PortfolioBudgetState;
mod fill_application;
use fill_application::FillApplicationInput;
mod order_admission;
use order_admission::OrderAdmissionInput;
mod order_validation;
use order_validation::reduce_only_order_allowed;
mod portfolio_timeline;
mod protective_exit;
mod virtual_validation;
pub use portfolio_timeline::{PortfolioBarEvent, PortfolioBarSeries, PortfolioBarTimeline};
pub use protective_exit::{
    ProtectiveExitFill, ProtectiveExitPolicy, ProtectiveExitReason, ProtectiveExitSignal,
};
mod virtual_events;
use virtual_events::apply_virtual_events;
use virtual_validation::{events_at, validate_virtual_events};

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
        // Snapshots preserve the validated, timestamp-sorted corporate-action order.
        let end = self
            .issuer_capital_snapshots
            .partition_point(|snapshot| snapshot.effective_ts <= ts);
        end.checked_sub(1)
            .and_then(|index| self.issuer_capital_snapshots.get(index))
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
        let input_data_hash = DataView::metadata_for_borrowed(bars, &DataSourceId::new("backtest"))
            .map_err(|report| {
                qx_core::QxError::Permanent(format!("回测输入未通过质量门: {:?}", report.issues))
            })?
            .data_hash;
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
        let fill_model_descriptor = fill.audit_descriptor();
        fee.validate()?;
        let currency =
            currency::normalize_and_validate(currency, &instrument, instrument_spec.as_ref())?;
        if multiplier <= 0 {
            return Err(qx_core::QxError::BusinessViolation(
                "回测合约乘数必须为正".into(),
            ));
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
        let [mut equity, mut benchmark_equity, mut positions] =
            std::array::from_fn(|_| Vec::with_capacity(bars.len()));
        let mut marks = BTreeMap::from([(instrument.clone(), Price::from_raw(0))]);
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
        let ashare_sessions = AshareSessionIndex::new(ashare_rules);
        let ashare_action_schedule = ashare_rules
            .map(|rules| AshareCorporateActionSchedule::for_bars(&rules.corporate_actions, bars));
        let ashare_previous_closes = ashare_rules.map(|rules| rules.previous_close_series(bars));
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
            .entry(deposit_id)
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
                .entry(collateral_id)
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
                let previous_close = ashare_previous_closes.as_ref().and_then(|closes| closes[i]);
                let (trading, block_buy, block_sell) =
                    rules.bar_restrictions(&ashare_sessions, bar, previous_close);
                matcher.set_halted(!trading);
                matcher.set_side_blocks(block_buy, block_sell);
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
            apply_virtual_events(
                &virtual_trading,
                ashare_action_schedule.as_ref(),
                bar,
                &mut virtual_state,
            )?;
            if i > 0 {
                // 质量门已确认 Bar 时间严格递增，因此历史切片恰为当前 Bar 之前的所有事实；
                // 直接借用原输入，避免构造拥有整份 Bar 副本的 DataView。
                let history = &bars[..i];
                let position = ledger.position_for(&account_id, &instrument).quantity.raw();
                // Spot buying power is settled cash after prior fills/fees. This
                // value is visible at the decision boundary; derivative margin
                // remains unavailable until the configured margin model can
                // expose a certified free-margin snapshot.
                let available_margin_raw =
                    strategy.refresh(&ledger, &account_id, &currency, derivative_spec.is_none());
                // A protective exit evaluates the last completed bar and submits
                // a market close for the current bar. It never sees the current
                // bar's OHLC while deciding, preventing same-bar lookahead.
                protective_order_meta.retain(|order_id, _| {
                    oms.get(*order_id)
                        .is_some_and(|order| !order.status.is_terminal())
                });
                let mut orders = Vec::new();
                if let (Some(policy), Some(completed_bar)) = (protective_exits, history.last()) {
                    let net_fallback = (position_policies.is_empty() && position != 0)
                        .then_some(qx_core::PositionSide::Net);
                    for position_side in position_policies.keys().copied().chain(net_fallback) {
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
                let accepted_orders = order_admission::admit_orders(OrderAdmissionInput {
                    orders: &mut orders,
                    account_id: &account_id,
                    instrument: &instrument,
                    currency: &currency,
                    ts: bar.ts,
                    position,
                    visible_close: bars[i - 1].close,
                    instrument_spec: instrument_spec.as_ref(),
                    derivative_spec,
                    multiplier,
                    strategy,
                    ledger: &ledger,
                    oms: &mut oms,
                    portfolio_budget: &mut portfolio_budget,
                    margin: margin.as_ref(),
                    matcher: &matcher,
                    other_pending_funding_raw: 0,
                    other_market_margin_raw: 0,
                    portfolio_marks: None,
                    portfolio_specs: None,
                    risk: &risk,
                    fx_rates: &virtual_trading.fx_rates,
                    ashare_rules,
                    ashare_bought_today: ashare_state.bought_today(),
                    next_id: &mut next_id,
                    log: &mut log,
                })?;
                // EventLog enforces timestamp/priority ordering. Emit all command
                // decisions above before any matcher acceptance at MATCH priority.
                for accepted in accepted_orders {
                    let accepted_seq = log.alloc_seq();
                    log.append(Event::new(
                        accepted_seq,
                        bar.ts,
                        qx_core::Priority::MATCH,
                        EventKind::Accepted {
                            client_order_id: accepted.order.client_id,
                            venue_order_id: None,
                        },
                    ));
                    matcher.submit_with_funding_reservation_at(
                        accepted.order,
                        bar.ts,
                        accepted.funding_reservation_raw,
                    );
                }
            }
            for fill in matcher.on_bar(bar, bar.ts) {
                let (order, fill) = fill_application::apply(FillApplicationInput {
                    fill,
                    instrument_spec: instrument_spec.as_ref(),
                    cash_account: derivative_spec.is_none(),
                    multiplier,
                    strategy,
                    account_id: &account_id,
                    currency: &currency,
                    ledger: &mut ledger,
                    oms: &mut oms,
                    portfolio_budget: &mut portfolio_budget,
                    margin: margin.as_ref(),
                    log: &mut log,
                    fills: &mut fills,
                })?;
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
                if ashare_rules.is_some() && order.side == Side::Buy {
                    ashare_state.on_buy(fill.qty.raw());
                }
            }
            if protective_exits.is_some() {
                position_policies.retain(|position_side, _| {
                    let state = if *position_side == qx_core::PositionSide::Net {
                        ledger.position_for(&account_id, &instrument)
                    } else {
                        ledger.position_for_side(&account_id, &instrument, *position_side)
                    };
                    let qty = state.quantity.raw();
                    if qty == 0 {
                        favorable_extremes.remove(position_side);
                        false
                    } else {
                        let extreme = if qty > 0 { bar.high } else { bar.low };
                        favorable_extremes
                            .entry(*position_side)
                            .and_modify(|value| {
                                *value = if qty > 0 {
                                    (*value).max(extreme)
                                } else {
                                    (*value).min(extreme)
                                };
                            })
                            .or_insert(extreme);
                        true
                    }
                });
            }
            *marks
                .get_mut(&instrument)
                .ok_or_else(|| QxError::Invariant("mark".into()))? = Price::from_raw(bar.close);
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
        // 费用与换手按成交事实汇总；虚拟事件成交不经过撮合循环，循环累加会让统计与账本不符。
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
            execution_attempts: matcher.execution_attempts().to_vec(),
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
            fill_model_descriptor,
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
        .entry(entry_id)
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

fn append_ashare_action_ledger_entry(
    log: &mut EventLog,
    ledger: &Ledger,
    entry_id: u64,
    ts: u64,
) -> Result<(), qx_core::QxError> {
    let entry = ledger
        .entry(entry_id)
        .cloned()
        .ok_or_else(|| qx_core::QxError::Invariant("找不到 A 股公司行为账簿 entry".into()))?;
    let seq = log.alloc_seq();
    // Corporate actions are market facts applied before this bar's strategy decisions.
    // Keeping them at MARKET priority makes date-only events mapped to the first intraday Bar
    // sort before same-timestamp COMMAND/MATCH/APPLY events in the replay log.
    log.append(Event::new(
        seq,
        ts,
        Priority::MARKET,
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
