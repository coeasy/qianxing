use super::*;
#[cfg(test)]
mod test_cases {
    use super::*;
    #[path = "config.rs"]
    mod config;
    #[path = "../tests/currency_validation.rs"]
    mod currency_validation;
    #[path = "fill_callback.rs"]
    mod fill_callback;
    #[path = "portfolio_budget.rs"]
    mod portfolio_budget;
    #[path = "portfolio_pending_risk.rs"]
    mod portfolio_pending_risk;
    #[path = "portfolio_strategy.rs"]
    mod portfolio_strategy;
    use crate::ashare::AshareCorporateActionEvent;
    use crate::{
        MarginTier, NextBarOpenFillModel, NoMargin, TieredMargin, VolumeSensitiveFillModel,
        ZeroLatency,
    };
    use config::simple_config;
    use qx_core::{MakerTakerFeeModel, OrderStatus, Quantity, Side, SCALE};
    #[path = "../tests/ashare_virtual_events.rs"]
    mod ashare_virtual_events;
    /// Q62：报告必须能被事实重新驱动，且逐条账簿条目都对得上。
    /// 旧的 `result_hash() == replay_hash()` 把同一段事件切片哈希两遍，恒等且不可能失败。
    fn assert_replay_matches(report: &BacktestReport) {
        let (log, ledger) = ReplayVerifier::replay(report.event_log.events()).unwrap();
        assert_eq!(log.digest(), report.result_hash());
        assert_eq!(log.events(), report.event_log.events());
        assert_eq!(ledger.entries(), report.ledger.entries());
        let facts = ReplayVerifier::verify(report.event_log.events(), &report.ledger).unwrap();
        assert_eq!(facts.ledger_entries, report.ledger.entries().len());
        assert!(
            !report.ledger.entries().is_empty(),
            "空账簿上的重放判据是空的"
        );
    }

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
        let decisions = report
            .event_log
            .events()
            .iter()
            .filter_map(|event| match &event.kind {
                EventKind::StrategyDecision { decision } => Some((event, decision)),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(decisions.len(), 2, "each native callback is auditable");
        assert_eq!(
            decisions
                .iter()
                .map(|(event, _)| event.ts)
                .collect::<Vec<_>>(),
            [2, 3],
            "strategy decisions are timestamped at their submission boundary"
        );
        assert_eq!(
            decisions
                .iter()
                .map(|(_, fact)| fact.signal_id)
                .collect::<Vec<_>>(),
            [1, 2],
            "each decision identifies only the last completed Bar it consumed"
        );
        assert!(
            decisions[1].1.intents.is_empty(),
            "empty decisions are auditable too"
        );
        let emitted = decisions[0].1;
        assert_eq!(emitted.strategy_id, "native-bar");
        // Bar(2) consumes the signal formed from visible Bar(1), never Bar(2)'s OHLC.
        assert_eq!(emitted.signal_id, 1);
        assert_eq!(emitted.intents.len(), 1);
        assert_eq!(decisions[0].0.prio, qx_core::Priority::COMMAND);
        assert_eq!(decisions[0].0.metadata.context.signal_id, "1");

        let risk_event = report
            .event_log
            .events()
            .iter()
            .find_map(|event| match &event.kind {
                EventKind::OrderRiskDecision { decision } => Some((event, decision)),
                _ => None,
            })
            .expect("the admitted native order has a replayable risk decision");
        assert!(risk_event.1.allowed);
        assert_eq!(risk_event.1.strategy_id.as_deref(), Some("native-bar"));
        assert_eq!(risk_event.1.signal_id, Some(emitted.signal_id));
        assert_eq!(risk_event.1.intent_id, Some(emitted.intents[0].intent_id));
        assert_eq!(risk_event.1.reference_price_raw, Some(100));
        let submit_index = report
            .event_log
            .events()
            .iter()
            .position(|event| {
                matches!(event.kind, EventKind::Submit { client_order_id } if client_order_id == risk_event.1.client_order_id)
            })
            .expect("risk decision links to submitted order");
        let risk_index = report
            .event_log
            .events()
            .iter()
            .position(|event| event.seq == risk_event.0.seq)
            .unwrap();
        assert!(risk_index > 0 && risk_index < submit_index);
        assert_replay_matches(&report);
    }

    #[test]
    fn protective_stop_and_take_crossing_close_next_bar_at_open_and_replay() {
        let bars = [
            Bar::new(1, 100, 101, 99, 100, 100),
            // The entry fills at this bar's open. Both the 5% stop and 5% target
            // are crossed; the conservative same-bar rule must select stop loss.
            Bar::new(2, 100, 110, 90, 101, 100),
            Bar::new(3, 80, 85, 75, 82, 100),
            Bar::new(4, 82, 84, 80, 83, 100),
        ];
        let policy = ProtectiveExitPolicy {
            stop_loss_bps: Some(500),
            take_profit_bps: Some(500),
            trailing_stop_bps: None,
        };
        let report = BacktestEngine::new(simple_config())
            .with_protective_exits(policy)
            .unwrap()
            .run(&bars, &mut BuyOnce { done: false })
            .unwrap();
        assert_eq!(report.fills.len(), 2);
        assert_eq!(report.fills[0].price.raw(), 100);
        assert_eq!(report.fills[1].price.raw(), 80);
        assert_eq!(report.protective_exit_fills.len(), 1);
        assert_eq!(
            report.protective_exit_fills[0].reason,
            ProtectiveExitReason::StopLoss
        );
        assert_eq!(report.protective_exit_fills[0].trigger_ts, 2);
        assert_eq!(report.protective_exit_fills[0].fill_ts, 3);
        assert_replay_matches(&report);
    }

    #[test]
    fn trailing_stop_tracks_favorable_extreme_without_lookahead() {
        let policy = ProtectiveExitPolicy {
            stop_loss_bps: None,
            take_profit_bps: None,
            trailing_stop_bps: Some(1_000),
        };
        let no_trigger = policy
            .evaluate(1, 100, &Bar::new(2, 100, 120, 110, 115, 1), Some(120))
            .unwrap();
        assert!(no_trigger.is_none());
        let trigger = policy
            .evaluate(1, 100, &Bar::new(3, 110, 112, 107, 108, 1), Some(120))
            .unwrap()
            .unwrap();
        assert_eq!(trigger.reason, ProtectiveExitReason::TrailingStop);
        assert_eq!(trigger.trigger_price_raw, 108);
    }

    #[test]
    fn protective_exit_policy_rejects_unbounded_or_empty_settings() {
        assert!(ProtectiveExitPolicy {
            stop_loss_bps: None,
            take_profit_bps: None,
            trailing_stop_bps: None,
        }
        .validate()
        .is_err());
        assert!(ProtectiveExitPolicy {
            stop_loss_bps: Some(10_000),
            take_profit_bps: None,
            trailing_stop_bps: None,
        }
        .validate()
        .is_err());
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
    fn maximum_drawdown_uses_each_rolling_peak_before_later_equity_highs() {
        let bars = vec![
            Bar::new(1, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 10),
            Bar::new(2, 102 * SCALE, 103 * SCALE, 89 * SCALE, 90 * SCALE, 10),
            Bar::new(3, 90 * SCALE, 201 * SCALE, 90 * SCALE, 200 * SCALE, 10),
        ];
        let report = BacktestEngine::new(simple_config())
            .run(&bars, &mut BuyOnce { done: false })
            .unwrap();

        // Equity falls from 1000 to 988, then rallies to 1098. The 120 bps
        // historical drawdown must not be diluted by the later 1098 equity peak.
        assert_eq!(
            report.equity,
            [1_000, 988, 1_098].map(|value| value * SCALE)
        );
        assert_eq!(report.max_drawdown_bps, 120);
    }

    #[test]
    fn drawdown_budget_prunes_at_the_first_training_bar_that_breaches_it() {
        let bars = vec![
            Bar::new(1, 100 * SCALE, 101 * SCALE, 99 * SCALE, 100 * SCALE, 10),
            Bar::new(2, 102 * SCALE, 103 * SCALE, 89 * SCALE, 90 * SCALE, 10),
            Bar::new(3, 90 * SCALE, 201 * SCALE, 90 * SCALE, 200 * SCALE, 10),
        ];
        let (status, report) = BacktestEngine::new(simple_config())
            .run_with_drawdown_pruning(&bars, &mut BuyOnce { done: false }, 2, 3, 115, || false)
            .unwrap();

        assert_eq!(
            status,
            BacktestRunStatus::Pruned {
                processed_bars: 2,
                max_drawdown_bps: 120,
            }
        );
        assert!(report.is_none(), "被剪枝的候选不得伪装成完整回测报告");
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
        assert_replay_matches(&report);
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
        // 逐条相等，而不只是两个查询口径相等：多记、漏记、记错顺序都会在这里红。
        assert_eq!(replayed.entries(), report.ledger.entries());
        // 反向证据：运行账簿里凭空多一条没有事实事件支撑的条目时，校验必须报错。
        let mut unbacked = report.ledger.clone();
        let next_id = report.ledger.entries().len() as u64;
        let extra = qx_core::LedgerEntry {
            id: next_id,
            ..report.ledger.entries()[0].clone()
        };
        unbacked.apply_entry(extra).unwrap();
        let error = ReplayVerifier::verify(report.event_log.events(), &unbacked).unwrap_err();
        assert!(
            error.to_string().contains("replayed="),
            "重放校验的错误文本应给出两侧条数: {error}"
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
        derivative.currency = "USDT".into();
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
            min_notional_raw: 0,
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
        let reports = [0, 1]
            .into_iter()
            .map(|_| {
                let mut strategy: Box<dyn BarStrategy> = Box::new(BuyOnce { done: false });
                BacktestEngine::new(simple_config())
                    .run(&bars, strategy.as_mut())
                    .unwrap()
            })
            .collect::<Vec<_>>();
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
    fn derivative_backtest_preserves_hedge_legs_and_replays_them() {
        let instrument = InstrumentId::parse("BTC/USDT:USDT.SIM").unwrap();
        let mut config = simple_config();
        config.instrument = instrument.clone();
        config.currency = "USDT".into();
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
            min_notional_raw: 0,
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
        assert_replay_matches(&report);
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
            min_notional_raw: 0,
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
        accepted.currency = "USDT".into();
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
        rejected.currency = "USDT".into();
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
            min_notional_raw: 0,
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
        config.currency = "USDT".into();
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
        accepted.currency = "USDT".into();
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
            min_notional_raw: 0,
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
            min_notional_raw: 0,
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
            cfg.currency = "USDT".into();
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
        config.currency = "USDT".into();
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
            min_notional_raw: 0,
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
