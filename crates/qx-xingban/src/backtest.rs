//! 可复用的 Bar 事件回测驱动器。
//!
//! CLI 只是入口；真正的回测闭环在这里固定为：as_of → 风控 → OMS →
//! 下一根 bar 撮合 → Fill → Ledger → EventLog。

use qx_core::{
    Event, EventKind, EventLog, FeeModel, Fill, Fnv1a, InstrumentId, Ledger, Money, Order,
    OrderStatus, Price, Priority, Quantity, ReplayVerifier, RunManifest, Side,
    TradingInstrumentSpec, SCALE,
};
use qx_guanxing::{Bar, DataSourceId, DataView, QualityGate, Verdict};
use qx_risk::OrderRiskPosition;
use qx_strategy::{MarketEvent, Strategy, StrategyContext};
use qx_zhenlu::{Oms, RiskGate};

use crate::ashare::{
    AshareCorporateActionType, AshareIssuerCapitalSnapshot, AshareRuleConfig, AshareSettlementState,
};
use crate::cost::{LatencyModel, MarginRule};
use crate::fill::{DataTier, FillModel};
use crate::venue::BarMatchingEngine;
use std::collections::BTreeMap;

pub trait BarStrategy {
    fn on_bar(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Option<Order> {
        let _ = (history, instrument, ts, position);
        None
    }

    /// 多订单策略入口。旧策略只实现 `on_bar` 即可；组合/多腿策略可以
    /// 覆盖该方法一次返回多个 OrderIntent 对应的核心订单。
    fn on_bar_orders(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Vec<Order> {
        self.on_bar(history, instrument, ts, position)
            .into_iter()
            .collect()
    }

    /// 可返回错误的正式入口。兼容策略继续使用 `on_bar_orders`，跨语言或
    /// 统一 Strategy API adapter 可以把契约错误传回回测引擎，而不是静默
    /// 丢弃策略事件。
    fn on_bar_orders_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        Ok(self.on_bar_orders(history, instrument, ts, position))
    }
}

/// 将统一 `qx-strategy::Strategy` 接入 Bar 回测。
///
/// 回测引擎在 Bar(t) 开始决策时传入的 `history` 最后一根是 Bar(t-1)，
/// 因此 adapter 只把该可见 Bar 转换为事件；新订单仍在 Bar(t) 提交，
/// 由撮合器按既有规则成交。
pub struct NativeBarStrategy<S: Strategy> {
    pub strategy: S,
    pub context: StrategyContext,
}

impl<S: Strategy> NativeBarStrategy<S> {
    pub fn new(strategy: S, context: StrategyContext) -> Self {
        Self { strategy, context }
    }

    pub fn initialize(&mut self) -> Result<(), qx_core::QxError> {
        self.strategy
            .on_init(&self.context)
            .map_err(qx_core::QxError::BusinessViolation)
    }
}

impl<S: Strategy> BarStrategy for NativeBarStrategy<S> {
    fn on_bar_orders_checked(
        &mut self,
        history: &[Bar],
        instrument: &InstrumentId,
        ts: u64,
        position: i128,
    ) -> Result<Vec<Order>, qx_core::QxError> {
        let Some(visible) = history.last() else {
            return Ok(Vec::new());
        };
        if visible.ts >= ts {
            return Err(qx_core::QxError::Invariant(
                "NativeBarStrategy 收到不可见的当前或未来 Bar".into(),
            ));
        }
        self.context.as_of = visible.ts;
        self.context
            .positions
            .insert(instrument.to_string(), position);
        let event = MarketEvent::Bar {
            instrument: instrument.clone(),
            ts: visible.ts,
            open_raw: visible.open,
            high_raw: visible.high,
            low_raw: visible.low,
            close_raw: visible.close,
            volume_raw: visible.volume,
        };
        let decision = self
            .strategy
            .on_event(&self.context, &event)
            .map_err(qx_core::QxError::BusinessViolation)?;
        decision
            .to_orders(&self.context)
            .map_err(qx_core::QxError::BusinessViolation)
    }
}

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
    /// 回测期间生效的发行人股本事实；它不属于账户 Ledger。
    pub issuer_capital_snapshots: Vec<AshareIssuerCapitalSnapshot>,
    pub fees_raw: i128,
    pub turnover_raw: i128,
    pub max_drawdown_raw: i128,
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
    pub fn replay_hash(&self) -> u64 {
        ReplayVerifier::rebuild_from(self.event_log.events())
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
}

impl BacktestEngine {
    pub fn new(config: BacktestConfig) -> Self {
        Self { config }
    }

    /// 批量执行候选配置；每个候选都创建独立的策略和账户状态，不能共享可变 Ledger。
    /// 工厂接收稳定的候选序号，调用方可以用它绑定 CandidateConfig 参数。
    pub fn run_batch<I, F>(
        configs: I,
        bars: &[Bar],
        mut strategy_factory: F,
    ) -> Result<Vec<BacktestReport>, qx_core::QxError>
    where
        I: IntoIterator<Item = BacktestConfig>,
        F: FnMut(usize) -> Box<dyn BarStrategy>,
    {
        configs
            .into_iter()
            .enumerate()
            .map(|(index, config)| {
                let mut strategy = strategy_factory(index);
                BacktestEngine::new(config).run(bars, strategy.as_mut())
            })
            .collect()
    }

    pub fn run(
        self,
        bars: &[Bar],
        strategy: &mut dyn BarStrategy,
    ) -> Result<BacktestReport, qx_core::QxError> {
        let quality = QualityGate::check(bars);
        if matches!(quality.verdict(), Verdict::Fail | Verdict::Quarantine) {
            return Err(qx_core::QxError::Permanent(format!(
                "回测输入未通过质量门: {:?}",
                quality.issues
            )));
        }
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
        } = self.config;
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
        // 有产品规格时规格就是每手规模的唯一真相：`contract_size` 已经参与名义额，
        // 再叠一个历史 `multiplier` 会让同一段成交在回测与实盘记出不同现金——实盘
        // 路径根本没有 multiplier。两者同时给出且不为 1 时必须显式拒绝，不能静默
        // 丢弃其中一个。
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
        let view =
            DataView::try_new(bars.to_vec(), DataSourceId::new("backtest")).map_err(|report| {
                qx_core::QxError::Permanent(format!("回测视图未通过质量门: {:?}", report.issues))
            })?;
        let input_data_hash = view.metadata().data_hash;
        // 费用基准需要 SCALE 标度乘数：contract_size 本身就是定点值，而兼容路径的
        // multiplier 是普通整数，必须折算，否则小额手续费会被整数除法截断为 0。
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
        // 反向（币本位）合约的费用基准与线性合约不同，必须跟随产品规格切换，
        // 否则撮合计出的手续费会与账本按 `spec.notional` 记的名义额脱钩。
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
                for mut order in
                    strategy.on_bar_orders_checked(history, &instrument, bar.ts, position)?
                {
                    if order.client_id == 0 {
                        order.client_id = next_id;
                        next_id += 1;
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
                        if !reduce_only_allowed {
                            append_rejection(
                                &mut log,
                                bar.ts,
                                order.client_id,
                                "reduce-only order would open or exceed the existing position",
                            );
                        } else if required_funding > available_funding {
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
                                // 的**普通整数**口径；SCALE 折算只发生在唯一入口
                                // `qx_risk::legacy_spot_spec`，这里再乘一次 SCALE 会把名义额
                                // 放大 1e9 倍（上游 `PositionSnapshot` 的定点口径不适用于
                                // 我方 `OrderRiskPosition`）。
                                multiplier,
                            )
                            .with_hedge_legs(long_qty, short_qty);
                            let risk_result = risk.check_with_price(
                                &order,
                                &risk_position,
                                Some(Price::from_raw(reference_price)),
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
                                let submit_seq = log.alloc_seq();
                                log.append(Event::new(
                                    submit_seq,
                                    bar.ts,
                                    qx_core::Priority::COMMAND,
                                    EventKind::Submit {
                                        client_order_id: order.client_id,
                                    },
                                ));
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
                    }
                }
            }
            for mut fill in matcher.on_bar(bar, bar.ts) {
                let order = oms
                    .get(fill.order_id)
                    .cloned()
                    .ok_or_else(|| qx_core::QxError::ReconcileRequired("成交找不到订单".into()))?;
                order.trace_fill(&mut fill, None, None);
                let entry_ids = qx_core::apply_fill_to_books(
                    &mut ledger,
                    &mut oms,
                    &currency,
                    &fill,
                    qx_core::FillTerms::resolve(instrument_spec.as_ref(), multiplier),
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
                fills.push(fill);
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
        Ok(BacktestReport {
            fills,
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
        })
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

fn reduce_only_order_allowed(
    ledger: &Ledger,
    account_id: &str,
    instrument: &InstrumentId,
    order: &Order,
    order_qty: i128,
) -> bool {
    let policy = order.policy.unwrap_or_default();
    let current = if policy.position_mode == qx_core::PositionMode::Hedge {
        ledger
            .position_for_side(account_id, instrument, policy.position_side)
            .quantity
            .raw()
    } else {
        ledger.position_for(account_id, instrument).quantity.raw()
    };
    if current == 0 || order_qty <= 0 {
        return false;
    }
    let delta = match order.side {
        Side::Buy => order_qty,
        Side::Sell => -order_qty,
    };
    current.signum() != delta.signum() && order_qty <= current.saturating_abs()
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
    if let Some(spec) = spec.filter(|spec| spec.product.supports_leverage()) {
        if fx_rates.is_empty() {
            ledger.equity_for_with_spec(account_id, marks, currency, spec)
        } else {
            ledger.equity_for_with_spec_and_fx(account_id, marks, currency, spec, fx_rates)
        }
    } else {
        ledger
            .equity_for_with_multiplier(account_id, marks, currency, multiplier)
            .ok_or_else(|| qx_core::QxError::Invariant("无法计算回测账户权益".into()))
    }
}

fn validate_virtual_events(config: &VirtualTradingConfig) -> Result<(), qx_core::QxError> {
    if config.liquidation_fee_bp < 0 {
        return Err(qx_core::QxError::BusinessViolation(
            "强平费用基点不能为负".into(),
        ));
    }
    if config
        .delivery
        .iter()
        .any(|event| event.settlement_price.raw() <= 0)
    {
        return Err(qx_core::QxError::BusinessViolation(
            "交割结算价必须为正".into(),
        ));
    }
    if config
        .fx_rates
        .iter()
        .any(|(currency, rate)| currency.trim().is_empty() || rate.raw() <= 0)
    {
        return Err(qx_core::QxError::BusinessViolation(
            "跨币种抵押品汇率必须有非空币种且为正".into(),
        ));
    }
    if config.collateral.values().any(|amount| amount.raw() < 0)
        || config
            .collateral
            .keys()
            .any(|currency| currency.trim().is_empty())
    {
        return Err(qx_core::QxError::BusinessViolation(
            "初始抵押品币种不能为空且金额不能为负".into(),
        ));
    }
    for events in [
        config
            .funding
            .iter()
            .map(|event| event.ts)
            .collect::<Vec<_>>(),
        config
            .interest
            .iter()
            .map(|event| event.ts)
            .collect::<Vec<_>>(),
        config
            .delivery
            .iter()
            .map(|event| event.ts)
            .collect::<Vec<_>>(),
    ] {
        if events.windows(2).any(|window| window[0] > window[1]) {
            return Err(qx_core::QxError::BusinessViolation(
                "虚拟交易事件必须按 ts 非递减配置".into(),
            ));
        }
    }
    Ok(())
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

fn is_cash_dividend_action(action_type: AshareCorporateActionType) -> bool {
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
    let entry_ids = qx_core::apply_ledger_fill(
        state.ledger,
        &order,
        state.currency,
        &fill,
        qx_core::FillTerms::resolve(state.spec, state.multiplier),
    )?;
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
mod tests {
    use super::*;
    use crate::ashare::AshareCorporateActionEvent;
    use crate::{
        MarginTier, NextBarOpenFillModel, NoMargin, TieredMargin, VolumeSensitiveFillModel,
        ZeroLatency,
    };
    use qx_core::{MakerTakerFeeModel, OrderStatus, Quantity, Side, SCALE};

    struct BuyOnce {
        done: bool,
    }
    impl BarStrategy for BuyOnce {
        fn on_bar(
            &mut self,
            _history: &[Bar],
            instrument: &InstrumentId,
            _ts: u64,
            position: i128,
        ) -> Option<Order> {
            if self.done || position != 0 {
                return None;
            }
            self.done = true;
            Some(Order {
                client_id: 0,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::Submitted,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            })
        }
    }

    struct OpenHedgeLegs {
        stage: u8,
    }

    impl BarStrategy for OpenHedgeLegs {
        fn on_bar(
            &mut self,
            _history: &[Bar],
            instrument: &InstrumentId,
            _ts: u64,
            _position: i128,
        ) -> Option<Order> {
            let (side, position_side) = match self.stage {
                0 => (Side::Buy, qx_core::PositionSide::Long),
                1 => (Side::Sell, qx_core::PositionSide::Short),
                _ => return None,
            };
            self.stage += 1;
            Some(Order {
                client_id: 0,
                instrument: instrument.clone(),
                side,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::Submitted,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: Some(qx_core::OrderPolicy {
                    reduce_only: false,
                    position_side,
                    margin_mode: qx_core::MarginMode::Cross,
                    position_mode: qx_core::PositionMode::Hedge,
                    leverage: 5,
                    post_only: false,
                }),
            })
        }
    }

    struct LeveragedBuyOnce {
        done: bool,
        leverage: u32,
    }

    struct ReduceOnlyBuyOnce {
        done: bool,
    }

    impl BarStrategy for ReduceOnlyBuyOnce {
        fn on_bar(
            &mut self,
            _history: &[Bar],
            instrument: &InstrumentId,
            _ts: u64,
            _position: i128,
        ) -> Option<Order> {
            if self.done {
                return None;
            }
            self.done = true;
            Some(Order {
                client_id: 0,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::Submitted,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: Some(qx_core::OrderPolicy {
                    reduce_only: true,
                    position_side: qx_core::PositionSide::Net,
                    margin_mode: qx_core::MarginMode::Cross,
                    position_mode: qx_core::PositionMode::OneWay,
                    leverage: 5,
                    post_only: false,
                }),
            })
        }
    }

    #[test]
    fn timed_rights_issue_lifecycle_uses_record_date_and_expires_remaining_rights() {
        let instrument = InstrumentId::parse("000001.SZSE").unwrap();
        let rights = InstrumentId::parse("700001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(10_000), 1)
            .unwrap();
        let source_order = Order {
            client_id: 1,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(100),
            limit: Some(Price::from_i64(10)),
            status: OrderStatus::Filled,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        };
        ledger
            .apply_fill(
                &source_order,
                &Fill {
                    order_id: 1,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 2,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        let record_ts = 10;
        let ex_ts = 20;
        let subscribe_ts = 30;
        let expiry_ts = 40;
        let rules = AshareRuleConfig {
            enabled: true,
            corporate_actions: vec![AshareCorporateActionEvent {
                ts: ex_ts,
                announcement_ts: None,
                record_ts: Some(record_ts),
                payment_ts: None,
                subscription_start_ts: Some(subscribe_ts),
                subscription_end_ts: Some(expiry_ts),
                action_type: AshareCorporateActionType::RightsIssue,
                split_num: 1,
                split_den: 1,
                rights_issue_price_raw: Price::from_i64(5).raw(),
                rights_issue_ratio_num: 20 * SCALE,
                rights_issue_ratio_den: 100 * SCALE,
                conversion_ratio_den: 1,
                rights_instrument: Some(rights.to_string()),
                subscription_qty_raw: Quantity::from_i64(10).raw(),
                ..AshareCorporateActionEvent::default()
            }],
            ..AshareRuleConfig::default()
        };
        rules.validate().unwrap();
        let mut config = VirtualTradingConfig {
            ashare_rules: Some(rules),
            ..VirtualTradingConfig::default()
        };
        let mut log = EventLog::new();
        let mut fills = Vec::new();
        for ts in [record_ts, ex_ts, subscribe_ts, expiry_ts] {
            let bar = Bar::new(ts, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 1_000);
            let mut state = VirtualExecution {
                instrument: &instrument,
                account_id: "main",
                currency: "CNY",
                multiplier: 1,
                spec: None,
                ledger: &mut ledger,
                log: &mut log,
                fills: &mut fills,
                fee_engine: None,
            };
            apply_virtual_events(&config, &bar, &mut state).unwrap();
            if ts == record_ts {
                assert_eq!(
                    ledger.rights_entitlement_for("main", &rights).raw(),
                    20 * SCALE
                );
            }
            if ts == subscribe_ts {
                assert_eq!(
                    ledger.position_for("main", &rights).quantity.raw(),
                    10 * SCALE
                );
                assert_eq!(
                    ledger.rights_entitlement_for("main", &rights).raw(),
                    10 * SCALE
                );
            }
        }
        assert_eq!(
            ledger.rights_entitlement_for("main", &rights),
            Quantity::ZERO
        );
        assert_eq!(
            ledger.position_for("main", &rights).quantity.raw(),
            10 * SCALE
        );
        config.ashare_rules.as_mut().unwrap().validate().unwrap();
    }

    #[test]
    fn timed_cash_dividend_pays_after_post_record_date_sale() {
        let instrument = InstrumentId::parse("000001.SZSE").unwrap();
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(10_000), 1)
            .unwrap();
        let mut buy = Order {
            client_id: 31,
            instrument: instrument.clone(),
            side: Side::Buy,
            qty: Quantity::from_i64(100),
            limit: Some(Price::from_i64(10)),
            status: OrderStatus::Filled,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
        };
        ledger
            .apply_fill(
                &buy,
                &Fill {
                    order_id: buy.client_id,
                    qty: buy.qty,
                    price: Price::from_i64(10),
                    ts: 2,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        let rules = AshareRuleConfig {
            enabled: true,
            corporate_actions: vec![AshareCorporateActionEvent {
                ts: 20,
                record_ts: Some(10),
                payment_ts: Some(30),
                action_type: AshareCorporateActionType::CashDividend,
                cash_dividend_raw: Money::from_raw(SCALE / 10).raw(),
                ..AshareCorporateActionEvent::default()
            }],
            ..AshareRuleConfig::default()
        };
        rules.validate().unwrap();
        let config = VirtualTradingConfig {
            ashare_rules: Some(rules),
            ..VirtualTradingConfig::default()
        };
        let mut log = EventLog::new();
        let mut fills = Vec::new();
        for ts in [10, 20] {
            let bar = Bar::new(ts, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 1_000);
            let mut state = VirtualExecution {
                instrument: &instrument,
                account_id: "main",
                currency: "CNY",
                multiplier: 1,
                spec: None,
                ledger: &mut ledger,
                log: &mut log,
                fills: &mut fills,
                fee_engine: None,
            };
            apply_virtual_events(&config, &bar, &mut state).unwrap();
        }
        assert_eq!(ledger.cash_for("main", "CNY"), Money::from_i64(9_000).raw());
        assert_eq!(
            ledger.cash_dividend_entitlement_for("main", &instrument, "CNY"),
            Money::from_i64(10)
        );
        buy.client_id = 32;
        buy.side = Side::Sell;
        buy.status = OrderStatus::Filled;
        ledger
            .apply_fill(
                &buy,
                &Fill {
                    order_id: 32,
                    qty: Quantity::from_i64(100),
                    price: Price::from_i64(10),
                    ts: 25,
                    ..Fill::default()
                },
                "CNY",
            )
            .unwrap();
        let payment_bar = Bar::new(30, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 1_000);
        let mut state = VirtualExecution {
            instrument: &instrument,
            account_id: "main",
            currency: "CNY",
            multiplier: 1,
            spec: None,
            ledger: &mut ledger,
            log: &mut log,
            fills: &mut fills,
            fee_engine: None,
        };
        apply_virtual_events(&config, &payment_bar, &mut state).unwrap();
        assert_eq!(
            ledger.cash_for("main", "CNY"),
            Money::from_i64(10_010).raw()
        );
        assert_eq!(
            ledger.cash_dividend_entitlement_for("main", &instrument, "CNY"),
            Money::ZERO
        );
    }

    #[test]
    fn timed_convertible_bond_issue_interest_and_redemption_flow() {
        let stock = InstrumentId::parse("000001.SZSE").unwrap();
        let bond = InstrumentId::parse("123001.SZSE").unwrap();
        let rules = AshareRuleConfig {
            enabled: true,
            corporate_actions: vec![
                AshareCorporateActionEvent {
                    ts: 10,
                    subscription_start_ts: Some(10),
                    action_type: AshareCorporateActionType::ConvertibleBondIssue,
                    convertible_bond_instrument: Some(bond.to_string()),
                    issue_price_raw: Price::from_i64(100).raw(),
                    subscription_qty_raw: Quantity::from_i64(100).raw(),
                    ..AshareCorporateActionEvent::default()
                },
                AshareCorporateActionEvent {
                    ts: 20,
                    record_ts: Some(20),
                    payment_ts: Some(30),
                    action_type: AshareCorporateActionType::ConvertibleBondInterest,
                    convertible_bond_instrument: Some(bond.to_string()),
                    interest_per_bond_raw: Price::from_i64(5).raw(),
                    ..AshareCorporateActionEvent::default()
                },
                AshareCorporateActionEvent {
                    ts: 40,
                    payment_ts: Some(40),
                    action_type: AshareCorporateActionType::ConvertibleBondCall,
                    convertible_bond_instrument: Some(bond.to_string()),
                    settlement_price_raw: Price::from_i64(105).raw(),
                    ..AshareCorporateActionEvent::default()
                },
            ],
            ..AshareRuleConfig::default()
        };
        rules.validate().unwrap();
        let config = VirtualTradingConfig {
            ashare_rules: Some(rules),
            ..VirtualTradingConfig::default()
        };
        let mut ledger = Ledger::new();
        ledger
            .deposit("main", "CNY", Money::from_i64(100_000), 1)
            .unwrap();
        let mut log = EventLog::new();
        let mut fills = Vec::new();
        for ts in [10, 20, 30, 40] {
            let bar = Bar::new(ts, 10 * SCALE, 10 * SCALE, 10 * SCALE, 10 * SCALE, 1_000);
            let mut state = VirtualExecution {
                instrument: &stock,
                account_id: "main",
                currency: "CNY",
                multiplier: 1,
                spec: None,
                ledger: &mut ledger,
                log: &mut log,
                fills: &mut fills,
                fee_engine: None,
            };
            apply_virtual_events(&config, &bar, &mut state).unwrap();
            if ts == 10 {
                assert_eq!(
                    ledger.position_for("main", &bond).quantity,
                    Quantity::from_i64(100)
                );
                assert_eq!(
                    ledger.cash_for("main", "CNY"),
                    Money::from_i64(90_000).raw()
                );
            }
            if ts == 20 {
                assert_eq!(
                    ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
                    Money::from_i64(500)
                );
            }
            if ts == 30 {
                assert_eq!(
                    ledger.convertible_bond_interest_entitlement_for("main", &bond, "CNY"),
                    Money::ZERO
                );
                assert_eq!(
                    ledger.cash_for("main", "CNY"),
                    Money::from_i64(90_500).raw()
                );
            }
        }
        assert_eq!(ledger.position_for("main", &bond).quantity, Quantity::ZERO);
        assert_eq!(
            ledger.cash_for("main", "CNY"),
            Money::from_i64(101_000).raw()
        );
    }

    impl BarStrategy for LeveragedBuyOnce {
        fn on_bar(
            &mut self,
            _history: &[Bar],
            instrument: &InstrumentId,
            _ts: u64,
            position: i128,
        ) -> Option<Order> {
            if self.done || position != 0 {
                return None;
            }
            self.done = true;
            Some(Order {
                client_id: 0,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(1),
                limit: None,
                status: OrderStatus::Submitted,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: Some(qx_core::OrderPolicy {
                    reduce_only: false,
                    position_side: qx_core::PositionSide::Net,
                    margin_mode: qx_core::MarginMode::Cross,
                    position_mode: qx_core::PositionMode::OneWay,
                    leverage: self.leverage,
                    post_only: false,
                }),
            })
        }
    }

    struct NativeBuyOnVisibleBar;

    impl qx_strategy::Strategy for NativeBuyOnVisibleBar {
        fn on_event(
            &mut self,
            context: &qx_strategy::StrategyContext,
            event: &qx_strategy::MarketEvent,
        ) -> Result<qx_strategy::StrategyDecision, String> {
            let qx_strategy::MarketEvent::Bar { instrument, ts, .. } = event else {
                return Err("测试策略只接受 Bar".into());
            };
            let position = context
                .positions
                .get(&instrument.to_string())
                .copied()
                .unwrap_or(0);
            Ok(qx_strategy::StrategyDecision {
                schema_version: qx_strategy::STRATEGY_API_VERSION,
                request_id: format!("native-bar-{ts}"),
                strategy_id: context.strategy_id.clone(),
                signal_id: *ts,
                confidence: 1,
                priority: 0,
                expires_at: *ts,
                intents: if position == 0 {
                    vec![qx_strategy::StrategyOrderIntent {
                        intent_id: *ts,
                        instrument: instrument.clone(),
                        side: Side::Buy,
                        qty: Quantity::from_i64(1),
                        limit: None,
                        policy: None,
                        reduce_only: false,
                        post_only: false,
                    }]
                } else {
                    Vec::new()
                },
            })
        }
    }

    fn simple_config() -> BacktestConfig {
        BacktestConfig {
            instrument: InstrumentId::parse("T.SIM").unwrap(),
            instrument_spec: None,
            account_id: "main".into(),
            currency: "USD".into(),
            initial_cash: Money::from_i64(1000),
            multiplier: 1,
            fill: Box::new(NextBarOpenFillModel),
            fee: Box::new(MakerTakerFeeModel {
                maker_bp: 0,
                taker_bp: 0,
            }),
            data_tier: DataTier::Bar,
            latency: Box::new(ZeroLatency),
            margin: Box::new(NoMargin),
            seed: 1,
            risk: RiskGate::conservative_default(),
            virtual_trading: VirtualTradingConfig::default(),
        }
    }

    #[test]
    fn native_strategy_adapter_sees_only_previous_bar() {
        let context = qx_strategy::StrategyContext {
            strategy_id: "native-bar".into(),
            strategy_version: "v1".into(),
            account_id: "main".into(),
            venue_id: "paper".into(),
            data_fingerprint: "bars-1".into(),
            as_of: 1,
            positions: std::collections::BTreeMap::new(),
            cash: std::collections::BTreeMap::new(),
            available_margin_raw: Some(Money::from_i64(1000).raw()),
            risk_state: "ready".into(),
        };
        let mut strategy = NativeBarStrategy::new(NativeBuyOnVisibleBar, context);
        strategy.initialize().unwrap();
        let report = BacktestEngine::new(simple_config())
            .run(
                &[
                    Bar::new(1, 100, 101, 99, 100, 10),
                    Bar::new(2, 102, 103, 101, 102, 10),
                    Bar::new(3, 104, 105, 103, 104, 10),
                ],
                &mut strategy,
            )
            .unwrap();
        assert_eq!(report.fills.len(), 1);
        assert_eq!(report.fills[0].ts, 2);
        assert_eq!(report.fills[0].price.raw(), 102);
    }

    #[test]
    fn backtest_report_keeps_issuer_capital_snapshots_outside_ledger() {
        let mut config = simple_config();
        config.virtual_trading.ashare_rules = Some(AshareRuleConfig {
            enabled: true,
            corporate_actions: vec![AshareCorporateActionEvent {
                ts: 2,
                action_type: AshareCorporateActionType::CapitalChange,
                issuer_total_shares_raw: 1_000,
                issuer_free_float_shares_raw: Some(700),
                source: "test".into(),
                ..AshareCorporateActionEvent::default()
            }],
            ..AshareRuleConfig::default()
        });
        let bars = vec![
            Bar::new(1, 100, 101, 99, 100, 10),
            Bar::new(2, 102, 103, 101, 102, 10),
            Bar::new(3, 104, 105, 103, 104, 10),
        ];
        let report = BacktestEngine::new(config)
            .run(&bars, &mut BuyOnce { done: true })
            .unwrap();
        assert_eq!(report.issuer_capital_snapshots.len(), 1);
        assert_eq!(report.issuer_capital_at(2).unwrap().total_shares_raw, 1_000);
        assert!(report
            .ledger
            .entries()
            .iter()
            .all(|entry| !matches!(entry.kind, qx_core::LedgerEntryKind::CorporateAction)));
    }

    #[test]
    fn runner_preserves_next_bar_causality_and_replay() {
        let instrument = InstrumentId::parse("T.SIM").unwrap();
        let bars = vec![
            Bar::new(1, 100, 101, 99, 100, 10),
            Bar::new(2, 102, 103, 101, 102, 10),
            Bar::new(3, 104, 105, 103, 104, 10),
        ];
        let cfg = BacktestConfig {
            instrument,
            instrument_spec: None,
            account_id: "main".into(),
            currency: "USD".into(),
            initial_cash: Money::from_i64(1000),
            multiplier: 1,
            fill: Box::new(NextBarOpenFillModel),
            fee: Box::new(MakerTakerFeeModel {
                maker_bp: 0,
                taker_bp: 0,
            }),
            data_tier: DataTier::Bar,
            latency: Box::new(ZeroLatency),
            margin: Box::new(NoMargin),
            seed: 1,
            risk: RiskGate::conservative_default(),
            virtual_trading: VirtualTradingConfig::default(),
        };
        let engine = BacktestEngine::new(cfg);
        let report = engine.run(&bars, &mut BuyOnce { done: false }).unwrap();
        assert_eq!(report.fills[0].price.raw(), 102);
        assert_eq!(report.result_hash(), report.replay_hash());
        assert_eq!(report.seed, 1);
        assert_eq!(report.equity.len(), report.benchmark_equity.len());
        assert_eq!(report.equity.len(), report.positions.len());
        assert_eq!(report.fees_raw, 0);
        assert!(report.return_bps >= 0);
        assert_eq!(report.max_drawdown_bps, 0);
        assert!(report
            .assumptions
            .iter()
            .any(|item| item == "benchmark=flat-cash"));
        let manifest = report
            .run_manifest(
                "run-1",
                "commit",
                "config",
                "strategy-v1",
                "instrument-v1",
                "runtime",
            )
            .unwrap();
        assert_eq!(manifest.global_seed, 1);
        assert_eq!(manifest.input_event_hash, manifest.data_fingerprint);
        let bound_manifest = report
            .run_manifest_with_data_fingerprint(
                RunManifestIdentity {
                    run_id: "run-bundle",
                    code_commit: "commit",
                    config_hash: "config",
                    strategy_version: "strategy-v1",
                    instrument_spec_version: "instrument-v1",
                    runtime_version: "runtime",
                },
                "dataset-bundle:bundle-fp",
            )
            .unwrap();
        assert_eq!(bound_manifest.data_fingerprint, "dataset-bundle:bundle-fp");
        assert!(report.model_descriptors[0].contains("NextBarOpen@v1"));
        assert!(report
            .event_log
            .events()
            .iter()
            .any(|event| matches!(event.kind, EventKind::Submit { .. })));
        let replayed = ReplayVerifier::rebuild_ledger(report.event_log.events()).unwrap();
        assert_eq!(
            replayed.position_for(
                "main",
                &report.ledger.entries()[1].instrument.clone().unwrap()
            ),
            report.ledger.position_for(
                "main",
                &report.ledger.entries()[1].instrument.clone().unwrap()
            )
        );
        assert_eq!(
            replayed.cash_for("main", "USD"),
            report.ledger.cash_for("main", "USD")
        );
    }

    /// 费用按 raw 口径的名义额计收：`fee_price_multiplier` 与 `contract_size` 同为
    /// SCALE=1.0 的定点倍数。历史上这里被当成整数乘数，现货手续费被压小 1e9 倍
    /// 且没有任何测试捕获（既有费用断言全部使用 0 bp）。
    #[test]
    fn fees_scale_with_raw_notional_for_spot_and_derivative() {
        let price = 60_000 * qx_core::SCALE;
        let bars = vec![
            Bar::new(1, price, price, price, price, 10 * qx_core::SCALE),
            Bar::new(2, price, price, price, price, 10 * qx_core::SCALE),
        ];

        let mut spot = simple_config();
        spot.initial_cash = Money::from_i64(1_000_000);
        spot.fee = Box::new(MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        });
        let spot_report = BacktestEngine::new(spot)
            .run(&bars, &mut BuyOnce { done: false })
            .unwrap();
        assert_eq!(spot_report.turnover_raw, 60_000 * qx_core::SCALE);
        assert_eq!(
            spot_report.fills[0].fee.raw(),
            30 * qx_core::SCALE,
            "1.0 @ 60_000 的 taker 费用应是 30，而不是 3e-8"
        );
        assert_eq!(spot_report.fees_raw, spot_report.fills[0].fee.raw());

        let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
        let mut derivative = simple_config();
        derivative.instrument = instrument.clone();
        derivative.initial_cash = Money::from_i64(1_000_000);
        derivative.fee = Box::new(MakerTakerFeeModel {
            maker_bp: 2,
            taker_bp: 5,
        });
        derivative.instrument_spec = Some(TradingInstrumentSpec {
            instrument,
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: 10 * qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        });
        let derivative_report = BacktestEngine::new(derivative)
            .run(&bars, &mut BuyOnce { done: false })
            .unwrap();
        assert_eq!(derivative_report.turnover_raw, 600_000 * qx_core::SCALE);
        assert_eq!(
            derivative_report.fills[0].fee.raw(),
            300 * qx_core::SCALE,
            "每张合约 10 个标的单位时，费用必须按 10 倍名义额计收"
        );
        assert_eq!(
            derivative_report.fees_raw,
            derivative_report.fills[0].fee.raw()
        );
    }

    #[test]
    fn deep_fill_model_cannot_run_on_bar_data() {
        let instrument = InstrumentId::parse("T.SIM").unwrap();
        let cfg = BacktestConfig {
            instrument,
            instrument_spec: None,
            account_id: "main".into(),
            currency: "USD".into(),
            initial_cash: Money::from_i64(1000),
            multiplier: 1,
            fill: Box::new(VolumeSensitiveFillModel { frac_bp: 1000 }),
            fee: Box::new(qx_core::ZeroFeeModel),
            data_tier: DataTier::Bar,
            latency: Box::new(ZeroLatency),
            margin: Box::new(NoMargin),
            seed: 1,
            risk: RiskGate::conservative_default(),
            virtual_trading: VirtualTradingConfig::default(),
        };
        let bars = vec![Bar::new(1, 100, 101, 99, 100, 10)];
        let mut strategy = BuyOnce { done: false };
        let result = BacktestEngine::new(cfg).run(&bars, &mut strategy);
        assert!(matches!(
            result,
            Err(qx_core::QxError::BusinessViolation(_))
        ));
    }

    #[test]
    fn batch_backtests_use_isolated_accounts_and_are_reproducible() {
        let bars = vec![
            Bar::new(1, 100, 101, 99, 100, 10),
            Bar::new(2, 102, 103, 101, 102, 10),
            Bar::new(3, 104, 105, 103, 104, 10),
        ];
        let reports =
            BacktestEngine::run_batch(vec![simple_config(), simple_config()], &bars, |_| {
                Box::new(BuyOnce { done: false })
            })
            .unwrap();
        assert_eq!(reports.len(), 2);
        assert_eq!(reports[0].result_hash(), reports[1].result_hash());
        assert_eq!(
            reports[0].ledger.cash_for("main", "USD"),
            reports[1].ledger.cash_for("main", "USD")
        );
    }

    #[test]
    fn virtual_funding_interest_and_delivery_enter_event_log_and_ledger() {
        let mut config = simple_config();
        config.virtual_trading = VirtualTradingConfig {
            funding: vec![FundingEvent {
                ts: 3,
                rate_bp: 100,
            }],
            interest: vec![InterestEvent { ts: 1, rate_bp: 10 }],
            delivery: vec![DeliveryEvent {
                ts: 3,
                settlement_price: Price::from_raw(110),
            }],
            enable_liquidation: false,
            liquidation_fee_bp: 0,
            fx_rates: BTreeMap::new(),
            collateral: BTreeMap::new(),
            ashare_rules: None,
        };
        let bars = vec![
            Bar::new(1, 100, 101, 99, 100, 10),
            Bar::new(2, 102, 103, 101, 102, 10),
            Bar::new(3, 110, 111, 109, 110, 10),
        ];
        let report = BacktestEngine::new(config)
            .run(&bars, &mut BuyOnce { done: false })
            .unwrap();
        assert!(report
            .ledger
            .entries()
            .iter()
            .any(|entry| entry.kind == qx_core::LedgerEntryKind::Funding));
        assert!(report
            .ledger
            .entries()
            .iter()
            .any(|entry| entry.kind == qx_core::LedgerEntryKind::Interest));
        assert_eq!(report.positions.last().copied(), Some(0));
        assert!(report
            .event_log
            .events()
            .iter()
            .any(|event| matches!(event.kind, EventKind::Settle)));
    }

    #[test]
    fn derivative_backtest_uses_spec_pnl_instead_of_full_notional_cashflow() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
        let mut config = simple_config();
        config.instrument = instrument.clone();
        config.instrument_spec = Some(TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        });
        let bars = vec![
            Bar::new(
                1,
                100 * qx_core::SCALE,
                101 * qx_core::SCALE,
                99 * qx_core::SCALE,
                100 * qx_core::SCALE,
                10 * qx_core::SCALE,
            ),
            Bar::new(
                2,
                102 * qx_core::SCALE,
                103 * qx_core::SCALE,
                101 * qx_core::SCALE,
                102 * qx_core::SCALE,
                10 * qx_core::SCALE,
            ),
            Bar::new(
                3,
                110 * qx_core::SCALE,
                111 * qx_core::SCALE,
                109 * qx_core::SCALE,
                110 * qx_core::SCALE,
                10 * qx_core::SCALE,
            ),
        ];
        let report = BacktestEngine::new(config)
            .run(&bars, &mut BuyOnce { done: false })
            .unwrap();
        assert_eq!(report.fills.len(), 1);
        assert_eq!(report.final_equity(), Money::from_i64(1008).raw());
    }

    #[test]
    fn derivative_backtest_preserves_hedge_legs_and_replays_them() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
        let mut config = simple_config();
        config.instrument = instrument.clone();
        config.instrument_spec = Some(TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        });
        let bars = vec![
            Bar::new(
                1,
                100 * qx_core::SCALE,
                101 * qx_core::SCALE,
                99 * qx_core::SCALE,
                100 * qx_core::SCALE,
                10 * qx_core::SCALE,
            ),
            Bar::new(
                2,
                100 * qx_core::SCALE,
                101 * qx_core::SCALE,
                99 * qx_core::SCALE,
                100 * qx_core::SCALE,
                10 * qx_core::SCALE,
            ),
            Bar::new(
                3,
                110 * qx_core::SCALE,
                111 * qx_core::SCALE,
                109 * qx_core::SCALE,
                110 * qx_core::SCALE,
                10 * qx_core::SCALE,
            ),
            Bar::new(
                4,
                110 * qx_core::SCALE,
                111 * qx_core::SCALE,
                109 * qx_core::SCALE,
                110 * qx_core::SCALE,
                10 * qx_core::SCALE,
            ),
        ];
        let report = BacktestEngine::new(config)
            .run(&bars, &mut OpenHedgeLegs { stage: 0 })
            .unwrap();
        assert_eq!(report.fills.len(), 2);
        assert_eq!(
            report
                .ledger
                .position_for_side("main", &instrument, qx_core::PositionSide::Long)
                .quantity
                .raw(),
            qx_core::SCALE
        );
        assert_eq!(
            report
                .ledger
                .position_for_side("main", &instrument, qx_core::PositionSide::Short)
                .quantity
                .raw(),
            -qx_core::SCALE
        );
        assert_eq!(
            report
                .ledger
                .position_for("main", &instrument)
                .quantity
                .raw(),
            0
        );
        assert_eq!(report.result_hash(), report.replay_hash());
    }

    #[test]
    fn margin_product_uses_leverage_for_initial_margin() {
        let instrument = InstrumentId::parse("BTC/USDT.OKX").unwrap();
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: qx_core::TradingProduct::Margin,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 10,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        };
        let bars = vec![
            Bar::new(
                1,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                qx_core::SCALE,
            ),
            Bar::new(
                2,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                qx_core::SCALE,
            ),
        ];
        let mut accepted = simple_config();
        accepted.instrument = instrument.clone();
        accepted.initial_cash = Money::from_i64(10);
        accepted.instrument_spec = Some(spec.clone());
        let accepted_report = BacktestEngine::new(accepted)
            .run(
                &bars,
                &mut LeveragedBuyOnce {
                    done: false,
                    leverage: 10,
                },
            )
            .unwrap();
        assert_eq!(accepted_report.fills.len(), 1);

        let mut rejected = simple_config();
        rejected.instrument = instrument;
        rejected.initial_cash = Money::from_i64(9);
        rejected.instrument_spec = Some(spec);
        let rejected_report = BacktestEngine::new(rejected)
            .run(
                &bars,
                &mut LeveragedBuyOnce {
                    done: false,
                    leverage: 10,
                },
            )
            .unwrap();
        assert!(rejected_report.fills.is_empty());
    }

    #[test]
    fn derivative_backtest_uses_tiered_initial_and_maintenance_margin() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.TIERED").unwrap();
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 100,
            valid_from: 1,
            valid_to: None,
        };
        let bars = vec![
            Bar::new(
                1,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                qx_core::SCALE,
            ),
            Bar::new(
                2,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                100 * qx_core::SCALE,
                qx_core::SCALE,
            ),
        ];
        let mut config = simple_config();
        config.instrument = instrument;
        config.initial_cash = Money::from_i64(15);
        config.instrument_spec = Some(spec);
        config.margin = Box::new(TieredMargin {
            tiers: vec![MarginTier {
                max_notional: 150 * qx_core::SCALE,
                initial_bp: 2_000,
                maintenance_bp: 1_000,
                max_leverage: None,
            }],
        });
        let report = BacktestEngine::new(config)
            .run(
                &bars,
                &mut LeveragedBuyOnce {
                    done: false,
                    leverage: 10,
                },
            )
            .unwrap();
        assert!(report.fills.is_empty());

        let mut accepted = simple_config();
        accepted.instrument = report
            .event_log
            .events()
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::LedgerApplied { entry } => entry.instrument.clone(),
                _ => None,
            })
            .unwrap_or_else(|| InstrumentId::parse("BTC/USDT:USDT.TIERED").unwrap());
        accepted.initial_cash = Money::from_i64(25);
        accepted.instrument_spec = Some(TradingInstrumentSpec {
            instrument: accepted.instrument.clone(),
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 100,
            valid_from: 1,
            valid_to: None,
        });
        accepted.margin = Box::new(TieredMargin {
            tiers: vec![MarginTier {
                max_notional: 150 * qx_core::SCALE,
                initial_bp: 2_000,
                maintenance_bp: 1_000,
                max_leverage: None,
            }],
        });
        let accepted_report = BacktestEngine::new(accepted)
            .run(
                &bars,
                &mut LeveragedBuyOnce {
                    done: false,
                    leverage: 10,
                },
            )
            .unwrap();
        assert_eq!(accepted_report.fills.len(), 1);
    }

    /// 强平成交必须与策略成交按同一费率收佣金。
    ///
    /// 撮合器的费率模型此前只作用于策略路径，虚拟强平路径把 `fill.fee` 写死为
    /// 零，于是爆仓场景等于免掉手续费平仓，回测结果系统性偏乐观。
    #[test]
    fn forced_liquidation_pays_the_same_commission_as_a_strategy_close() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.LIQ").unwrap();
        let spec = TradingInstrumentSpec {
            instrument: instrument.clone(),
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 100,
            maintenance_margin_bps: 100,
            valid_from: 1,
            valid_to: None,
        };
        let flat_bar = |ts: u64, price: i64| {
            let raw = i128::from(price) * qx_core::SCALE;
            Bar::new(ts, raw, raw, raw, raw, qx_core::SCALE)
        };
        // 100 开多一张，第三根 bar 跌到 10：权益跌破维持保证金，触发强平。
        let bars = vec![flat_bar(1, 100), flat_bar(2, 100), flat_bar(3, 10)];
        let config = |taker_bp: i64| {
            let mut cfg = simple_config();
            cfg.instrument = instrument.clone();
            cfg.initial_cash = Money::from_i64(25);
            cfg.instrument_spec = Some(spec.clone());
            cfg.fee = Box::new(MakerTakerFeeModel {
                maker_bp: 0,
                taker_bp,
            });
            cfg.margin = Box::new(TieredMargin {
                tiers: vec![MarginTier {
                    max_notional: 150 * qx_core::SCALE,
                    initial_bp: 2_000,
                    maintenance_bp: 1_000,
                    max_leverage: None,
                }],
            });
            cfg.virtual_trading = VirtualTradingConfig {
                enable_liquidation: true,
                ..VirtualTradingConfig::default()
            };
            cfg
        };
        let run = |taker_bp: i64| {
            BacktestEngine::new(config(taker_bp))
                .run(
                    &bars,
                    &mut LeveragedBuyOnce {
                        done: false,
                        leverage: 10,
                    },
                )
                .unwrap()
        };

        let charged = run(50);
        assert_eq!(charged.fills.len(), 2, "开仓成交与强平成交各一笔");
        assert_eq!(charged.positions.last(), Some(&0), "强平后必须清空持仓");
        let entry = charged.fills[0].fee.raw();
        let forced = charged.fills[1].fee.raw();
        assert_eq!(
            entry,
            qx_core::bp_amount(100 * qx_core::SCALE, 50),
            "开仓按 taker 计费"
        );
        assert_eq!(
            forced,
            qx_core::bp_amount(10 * qx_core::SCALE, 50),
            "强平必须按同一 taker 费率收佣金"
        );
        assert_eq!(
            charged.fees_raw,
            charged.fills.iter().map(|fill| fill.fee.raw()).sum(),
            "报告费用统计必须覆盖不经过撮合循环的强平成交"
        );
        let free = run(0);
        assert_eq!(free.fills.len(), 2);
        assert_eq!(free.fills[1].fee.raw(), 0);
        assert!(
            charged.equity.last() < free.equity.last(),
            "同一场景下计费权益必须低于免佣金权益"
        );
    }

    #[test]
    fn reduce_only_order_cannot_open_a_new_virtual_position() {
        let bars = vec![
            Bar::new(1, 100, 100, 100, 100, 10),
            Bar::new(2, 100, 100, 100, 100, 10),
        ];
        let mut config = simple_config();
        config.instrument_spec = Some(TradingInstrumentSpec {
            instrument: config.instrument.clone(),
            product: qx_core::TradingProduct::Perpetual,
            base_currency: "BTC".into(),
            quote_currency: "USDT".into(),
            settlement_currency: "USDT".into(),
            contract_size: qx_core::SCALE,
            linear: true,
            inverse: false,
            price_tick: 1,
            qty_step: 1,
            min_qty: 1,
            max_leverage: 10,
            maintenance_margin_bps: 500,
            valid_from: 1,
            valid_to: None,
        });
        let report = BacktestEngine::new(config)
            .run(&bars, &mut ReduceOnlyBuyOnce { done: false })
            .unwrap();
        assert!(report.fills.is_empty());
        assert!(report
            .event_log
            .events()
            .iter()
            .any(|event| matches!(event.kind, EventKind::Rejected { .. })));
    }

    /// 按调用次序依次下单，用于构造"先建仓再用持仓市值继续买"的场景。
    struct StagedBuyer {
        quantities: Vec<i64>,
        stage: usize,
    }

    impl BarStrategy for StagedBuyer {
        fn on_bar(
            &mut self,
            _history: &[Bar],
            instrument: &InstrumentId,
            _ts: u64,
            _position: i128,
        ) -> Option<Order> {
            let qty = *self.quantities.get(self.stage)?;
            self.stage += 1;
            Some(Order {
                client_id: self.stage as u64,
                instrument: instrument.clone(),
                side: Side::Buy,
                qty: Quantity::from_i64(qty),
                limit: None,
                status: OrderStatus::Submitted,
                filled: Quantity::ZERO,
                account_id: "main".into(),
                trace: None,
                policy: None,
            })
        }
    }

    fn rejection_reasons(report: &BacktestReport) -> Vec<String> {
        report
            .event_log
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::Rejected { reason, .. } => Some(reason.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn spot_buys_cannot_be_funded_by_position_value() {
        let bars = (0..4)
            .map(|i| Bar::new(i + 1, 100, 100, 100, 100, 10))
            .collect::<Vec<_>>();
        let mut config = simple_config();
        config.initial_cash = Money::from_raw(1_000);
        let mut strategy = StagedBuyer {
            quantities: vec![5, 8],
            stage: 0,
        };
        let report = BacktestEngine::new(config)
            .run(&bars, &mut strategy)
            .unwrap();

        assert_eq!(report.fills.len(), 1, "只有第一笔现金买单应当成交");
        // 5 手在 bar2 开盘 100 成交后剩现金 500，权益仍是 1000；第二笔 8 手名义额
        // 800 必须因为现金不足被拒，否则等于用持仓市值凭空加杠杆。
        assert!(
            rejection_reasons(&report)
                .iter()
                .any(|reason| reason.contains("可用现金")),
            "现金不足必须留下可审计的拒绝原因，实际为 {:?}",
            rejection_reasons(&report)
        );
        assert!(
            report.ledger.cash_for("main", "USD") >= 0,
            "现货买入不得把账本现金写成负数"
        );
    }

    #[test]
    fn spot_buy_cash_floor_includes_the_estimated_fee() {
        let bars = (0..3)
            .map(|i| Bar::new(i + 1, 100, 100, 100, 100, 10))
            .collect::<Vec<_>>();
        let mut config = simple_config();
        config.initial_cash = Money::from_raw(1_000);
        config.fee = Box::new(MakerTakerFeeModel {
            maker_bp: 0,
            taker_bp: 1_000,
        });
        let mut strategy = StagedBuyer {
            quantities: vec![10],
            stage: 0,
        };
        let report = BacktestEngine::new(config)
            .run(&bars, &mut strategy)
            .unwrap();

        // 名义额恰好等于现金，但 10% 吃单费会让成交后透支，所以必须前置拒绝。
        assert!(report.fills.is_empty());
        assert!(rejection_reasons(&report)
            .iter()
            .any(|reason| reason.contains("含预估手续费")));
    }
}
