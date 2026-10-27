//! 事实提交与失败回滚（P0-2：从顶格文件 `pipeline.rs` 切出的同 crate inherent impl）。
//!
//! 稳态提交不再 `self.clone()` 整份状态（那是每次追加 O(日志长度) 的源头）；改成就地提交，
//! 失败时由薄包装调 `rebuild_from_store(true)` 从 store 重放回滚——store 只在 `persist` 成功时
//! 才写，所以失败时它仍是旧账本，重放即回滚。`true` 是必须的：`append_at_engine` 会在
//! `append_checked` 之前就 `alloc_seq` 推过 `next_seq`，而 `events` 一条没变，常规重建的
//! "日志逐条相等就跳过"短路会让这一格漏回滚。

use super::*;

impl LiveEventPipeline {
    /// Append one immutable capability decision to the account EventLog. The caller has already
    /// run the catalog/maturity preflight; this method enforces order identity and persistence.
    pub(super) fn record_execution_capability_decision_once(
        &mut self,
        decision: ExecutionCapabilityDecisionFact,
        ts: u64,
    ) -> QxResult<u64> {
        match self.record_execution_capability_decision_commit(decision, ts) {
            Ok(seq) => Ok(seq),
            Err(error) => {
                if let Err(rollback_error) = self.rebuild_from_store(true) {
                    return Err(QxError::ReconcileRequired(format!(
                        "执行能力证据提交失败: {error}; 从持久化日志回滚失败: {rollback_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    fn record_execution_capability_decision_commit(
        &mut self,
        decision: ExecutionCapabilityDecisionFact,
        ts: u64,
    ) -> QxResult<u64> {
        if decision.schema_version != EXECUTION_CAPABILITY_DECISION_SCHEMA_VERSION
            || decision.account_id.trim().is_empty()
            || decision.venue_id.trim().is_empty()
            || decision.preflight_json.trim().is_empty()
            || !is_sha256_hex(&decision.decision_id)
            || !is_sha256_hex(&decision.order_fingerprint)
            || !is_sha256_hex(&decision.catalog_fingerprint)
            || !is_sha256_hex(&decision.market_rule_fingerprint)
        {
            return Err(QxError::BusinessViolation(
                "ExecutionCapabilityDecisionFact schema、身份或摘要字段无效".into(),
            ));
        }
        if let Some(event) = self.log.events().iter().find(|event| {
            matches!(
                &event.kind,
                EventKind::ExecutionCapabilityDecision { decision: existing }
                    if existing.client_order_id == decision.client_order_id
            )
        }) {
            return match &event.kind {
                EventKind::ExecutionCapabilityDecision { decision: existing }
                    if existing == &decision =>
                {
                    Ok(event.seq)
                }
                _ => Err(QxError::Invariant(format!(
                    "client_order_id={} 已绑定不同的执行能力证据，拒绝覆盖",
                    decision.client_order_id
                ))),
            };
        }

        match decision.kind {
            ExecutionCapabilityDecisionKind::Submit => {
                if self.oms.get(decision.client_order_id).is_some() {
                    return Err(QxError::Invariant(format!(
                        "client_order_id={} 已进入 OMS，不能补写提交前能力证据",
                        decision.client_order_id
                    )));
                }
            }
            ExecutionCapabilityDecisionKind::RecoveryAdoption => {
                let order = self.oms.get(decision.client_order_id).ok_or_else(|| {
                    QxError::ReconcileRequired(format!(
                        "不能为未知活动订单 {} 建立恢复能力证据",
                        decision.client_order_id
                    ))
                })?;
                if order.account_id != decision.account_id
                    || !order
                        .instrument
                        .venue
                        .as_str()
                        .eq_ignore_ascii_case(&decision.venue_id)
                    || order.status.is_terminal()
                {
                    return Err(QxError::Invariant(format!(
                        "恢复能力证据与活动订单归属不匹配 order={} account={} venue={}",
                        decision.client_order_id, decision.account_id, decision.venue_id
                    )));
                }
            }
        }

        let context = EventContext {
            tenant_id: decision.account_id.clone(),
            run_id: format!("account:{}", decision.account_id),
            account_id: decision.account_id.clone(),
            portfolio_id: decision.account_id.clone(),
            strategy_id: "execution-capability-preflight".into(),
            signal_id: String::new(),
            intent_id: format!("order:{}", decision.client_order_id),
        };
        let correlation_id = format!(
            "execution-capability:{}:{}",
            decision.account_id, decision.client_order_id
        );
        let event = self.append_fact(
            ts,
            ts,
            Priority::COMMAND,
            decision.command_id.unwrap_or(decision.client_order_id),
            correlation_id,
            EventFact {
                metadata: EventMetadata {
                    source_id: "execution-capability-gate".into(),
                    source_kind: "control".into(),
                    dedup_key: format!(
                        "execution-capability:{}:{}",
                        decision.account_id, decision.client_order_id
                    ),
                    context,
                    ..EventMetadata::default()
                },
                kind: EventKind::ExecutionCapabilityDecision {
                    decision: decision.clone(),
                },
            },
        )?;
        self.persist()?;
        Ok(event.seq)
    }

    /// 失败即回滚的薄包装：就地提交失败时从 store 重放，把状态收回上一次成功提交的那一点。
    ///
    /// 原实现靠 `self.clone()` 整份拷贝换这个保证（每次追加 O(日志长度)）；现在稳态提交不再
    /// 拷贝，只有失败路径付一次重放。store 只在 `persist` 成功时才写，所以失败时它仍是旧账本，
    /// 重放即回滚。
    pub(super) fn register_order_with_correlation_once(
        &mut self,
        order: Order,
        ts: u64,
        correlation: Option<String>,
    ) -> QxResult<u64> {
        match self.register_order_commit(order, ts, correlation) {
            Ok(seq) => Ok(seq),
            Err(error) => {
                // 回滚本身失败说明 store 已经不可信：让它的错误浮上来（fail-closed），
                // 同时保留触发回滚的原始错误，否则根因会被回滚错误覆盖。
                if let Err(rollback_error) = self.rebuild_from_store(true) {
                    return Err(QxError::ReconcileRequired(format!(
                        "提交失败: {error}; 从持久化日志回滚失败: {rollback_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    /// 就地提交一个订单事实（不拷贝整份状态；回滚由调用方 [`Self::register_order_with_correlation_once`] 负责）。
    fn register_order_commit(
        &mut self,
        mut order: Order,
        ts: u64,
        correlation: Option<String>,
    ) -> QxResult<u64> {
        order.validate().map_err(QxError::BusinessViolation)?;
        if self.oms.get(order.client_id).is_some() {
            return Err(QxError::Invariant(format!(
                "重复的 live client_order_id: {}",
                order.client_id
            )));
        }
        if order.status == OrderStatus::PendingSubmit {
            order
                .status
                .transition(OrderStatus::Submitted)
                .map_err(QxError::Invariant)?;
        }
        if order.status != OrderStatus::Submitted {
            return Err(QxError::BusinessViolation(
                "register_order 只接受 PendingSubmit 或 Submitted 订单".into(),
            ));
        }
        let context = order_event_context(&order);
        context
            .validate_for_trading()
            .map_err(QxError::BusinessViolation)?;
        let event = self.append_fact(
            ts,
            ts,
            Priority::COMMAND,
            order.client_id,
            correlation.unwrap_or_else(|| format!("{}:order:{}", self.log_name, order.client_id)),
            EventFact {
                metadata: EventMetadata {
                    source_id: "control".into(),
                    source_kind: "control".into(),
                    dedup_key: format!("control:order:{}", order.client_id),
                    context,
                    ..EventMetadata::default()
                },
                kind: EventKind::OrderSubmitted {
                    order: order.clone(),
                },
            },
        )?;
        self.oms.insert_replayed(order)?;
        self.persist()?;
        Ok(event.seq)
    }

    /// 失败即回滚的薄包装：见 [`Self::register_order_with_correlation_once`] 的同族说明。
    pub(super) fn ingest_once(
        &mut self,
        envelope: RuntimeEventEnvelope,
    ) -> QxResult<RuntimeIngestReceipt> {
        match self.ingest_commit(envelope) {
            Ok(receipt) => Ok(receipt),
            Err(error) => {
                if let Err(rollback_error) = self.rebuild_from_store(true) {
                    return Err(QxError::ReconcileRequired(format!(
                        "归约失败: {error}; 从持久化日志回滚失败: {rollback_error}"
                    )));
                }
                Err(error)
            }
        }
    }

    /// 就地归约一条外部事实（不拷贝整份状态；回滚由 [`Self::ingest_once`] 负责）。
    fn ingest_commit(&mut self, envelope: RuntimeEventEnvelope) -> QxResult<RuntimeIngestReceipt> {
        let RuntimeEventEnvelope {
            event,
            event_ts,
            receive_ts,
            source_seq,
            correlation_id,
            mut metadata,
        } = envelope;
        let correlation_id = if correlation_id.trim().is_empty() {
            format!("{}:source:{}", self.log_name, source_seq)
        } else {
            correlation_id
        };
        if metadata.source_id.trim().is_empty() {
            metadata.source_id =
                runtime_event_metadata(&correlation_id, source_seq, "internal").source_id;
        }
        if metadata.dedup_key.trim().is_empty() {
            metadata.dedup_key =
                runtime_event_metadata(&correlation_id, source_seq, "internal").dedup_key;
        }
        if metadata.source_kind.trim().is_empty() {
            metadata.source_kind =
                runtime_event_metadata(&correlation_id, source_seq, "internal").source_kind;
        }
        if metadata.rule_version.trim().is_empty() {
            metadata.rule_version = "runtime-v1".into();
        }
        enrich_runtime_event_context(&mut metadata, &event);
        metadata.validate().map_err(QxError::BusinessViolation)?;

        let semantic_replay = matches!(
            &event,
            RuntimeExternalEvent::Accepted { .. }
                | RuntimeExternalEvent::CancelPending { .. }
                | RuntimeExternalEvent::Cancelled { .. }
                | RuntimeExternalEvent::ReconcileRequired { .. }
                | RuntimeExternalEvent::AccountCashflow { .. }
                | RuntimeExternalEvent::FeeAdjustment { .. }
                | RuntimeExternalEvent::FillWithSpec { .. }
        );
        if let Some(engine_ts) = self.event_identities.duplicate_ts(
            &metadata.dedup_key,
            &correlation_id,
            source_seq,
            semantic_replay,
        ) {
            return Ok(RuntimeIngestReceipt {
                derived_seqs: Vec::new(),
                engine_ts,
                deduplicated: true,
            });
        }

        let fill_for_dedup = match &event {
            RuntimeExternalEvent::Fill { fill } => Some(fill),
            RuntimeExternalEvent::FillWithSpec { fill, .. } => Some(fill.as_ref()),
            _ => None,
        };
        if let Some(fill) = fill_for_dedup {
            let key = fill_key(fill);
            if self.seen_fills.contains(&key) {
                return Ok(RuntimeIngestReceipt {
                    derived_seqs: Vec::new(),
                    engine_ts: self.last_engine_ts,
                    deduplicated: true,
                });
            }
        }

        let (kind, priority) = match &event {
            RuntimeExternalEvent::MarketQuote {
                instrument,
                bid,
                ask,
                bid_qty,
                ask_qty,
                bid_qty_exact,
                ask_qty_exact,
                bid_exact,
                ask_exact,
            } => {
                let exact_quote_valid = match (bid_exact, ask_exact) {
                    (Some(bid), Some(ask)) => {
                        bid.is_positive()
                            && ask.is_positive()
                            && bid
                                .checked_cmp(*ask)
                                .is_some_and(|ordering| !ordering.is_gt())
                    }
                    (None, None) => bid.raw() > 0 && ask.raw() > 0 && bid.raw() <= ask.raw(),
                    _ => false,
                };
                let valid_size = |legacy: Quantity, exact: &Option<ExactDecimal>| match exact {
                    Some(value) => {
                        value.is_positive()
                            && value
                                .checked_mul_at_scale(
                                    ExactDecimal::ONE,
                                    9,
                                    DecimalRounding::TowardZero,
                                )
                                .and_then(|projection| projection.to_raw(9).ok())
                                == Some(legacy.raw())
                    }
                    None => legacy.raw() > 0,
                };
                if !exact_quote_valid
                    || !valid_size(*bid_qty, bid_qty_exact)
                    || !valid_size(*ask_qty, ask_qty_exact)
                {
                    return Err(QxError::BusinessViolation(
                        "L1 行情精确 bid/ask/数量非法、精度字段不完整或买卖盘交叉".into(),
                    ));
                }
                (
                    EventKind::MarketQuote {
                        instrument: instrument.clone(),
                        bid: *bid,
                        ask: *ask,
                        bid_qty: *bid_qty,
                        ask_qty: *ask_qty,
                        bid_qty_exact: *bid_qty_exact,
                        ask_qty_exact: *ask_qty_exact,
                        bid_exact: *bid_exact,
                        ask_exact: *ask_exact,
                    },
                    Priority::MARKET,
                )
            }
            RuntimeExternalEvent::MarketOrderBook {
                instrument,
                sequence,
                bids,
                asks,
            } => {
                let valid_levels = |levels: &[qx_core::MarketDepthLevel], bids: bool| {
                    if levels.is_empty() || levels.len() > 100 {
                        return Err("盘口每侧必须包含 1..=100 个价格档位".to_string());
                    }
                    for level in levels {
                        level.validate()?;
                    }
                    for pair in levels.windows(2) {
                        let ordering = pair[0]
                            .exact_price()?
                            .checked_cmp(pair[1].exact_price()?)
                            .ok_or_else(|| "盘口价格无法比较".to_string())?;
                        if if bids {
                            !ordering.is_gt()
                        } else {
                            !ordering.is_lt()
                        } {
                            return Err("盘口档位未按价格方向严格排序".into());
                        }
                    }
                    Ok(())
                };
                if *sequence == 0 {
                    return Err(QxError::BusinessViolation("盘口 sequence 必须为正".into()));
                }
                valid_levels(bids, true).map_err(QxError::BusinessViolation)?;
                valid_levels(asks, false).map_err(QxError::BusinessViolation)?;
                let best_bid = bids[0].exact_price().map_err(QxError::BusinessViolation)?;
                let best_ask = asks[0].exact_price().map_err(QxError::BusinessViolation)?;
                if best_bid
                    .checked_cmp(best_ask)
                    .is_none_or(|ordering| ordering.is_gt())
                {
                    return Err(QxError::BusinessViolation(
                        "盘口最优买价高于最优卖价".into(),
                    ));
                }
                (
                    EventKind::MarketOrderBook {
                        instrument: instrument.clone(),
                        sequence: *sequence,
                        bids: bids.clone(),
                        asks: asks.clone(),
                    },
                    Priority::MARKET,
                )
            }
            RuntimeExternalEvent::AccountBalanceSnapshot {
                account_id,
                venue_id,
                balances,
            } => {
                if account_id.trim().is_empty() || venue_id.trim().is_empty() {
                    return Err(QxError::ReconcileRequired(
                        "账户余额快照缺少 account_id 或 venue_id".into(),
                    ));
                }
                let balances = normalize_balances(balances.clone())?;
                (
                    EventKind::AccountBalanceSnapshot {
                        account_id: account_id.clone(),
                        venue_id: venue_id.clone(),
                        balances,
                    },
                    Priority::FEEDBACK,
                )
            }
            RuntimeExternalEvent::AccountPositionSnapshot {
                account_id,
                venue_id,
                positions,
            } => {
                if account_id.trim().is_empty() || venue_id.trim().is_empty() {
                    return Err(QxError::ReconcileRequired(
                        "账户持仓快照缺少 account_id 或 venue_id".into(),
                    ));
                }
                let positions = normalize_positions(positions.clone())?;
                (
                    EventKind::AccountPositionSnapshot {
                        account_id: account_id.clone(),
                        venue_id: venue_id.clone(),
                        positions,
                    },
                    Priority::FEEDBACK,
                )
            }
            RuntimeExternalEvent::FundingRateSnapshot { snapshot } => {
                if snapshot.instrument.to_string().trim().is_empty()
                    || snapshot.next_funding_timestamp_ms == Some(0)
                {
                    return Err(QxError::ReconcileRequired(
                        "资金费率快照 instrument 或下一结算时间非法".into(),
                    ));
                }
                (
                    EventKind::FundingRateSnapshot {
                        instrument: snapshot.instrument.clone(),
                        funding_rate_bps: snapshot.funding_rate_bps,
                        next_funding_timestamp_ms: snapshot.next_funding_timestamp_ms,
                    },
                    Priority::FEEDBACK,
                )
            }
            RuntimeExternalEvent::AccountCashflow { cashflow } => {
                validate_cashflow(cashflow)?;
                (
                    EventKind::AccountCashflow {
                        cashflow: cashflow.clone(),
                    },
                    Priority::FEEDBACK,
                )
            }
            RuntimeExternalEvent::FeeAdjustment {
                account_id,
                venue_id,
                order_id,
                instrument,
                fees,
            } => {
                if account_id.trim().is_empty() || venue_id.trim().is_empty() || fees.is_empty() {
                    return Err(QxError::ReconcileRequired(
                        "手续费调整缺少 account/venue/fees".into(),
                    ));
                }
                let order = self.oms.get(*order_id).ok_or_else(|| {
                    QxError::ReconcileRequired(format!("手续费调整对应未知订单 {order_id}"))
                })?;
                if order.account_id != *account_id || order.instrument != *instrument {
                    return Err(QxError::ReconcileRequired(format!(
                        "手续费调整订单归属不匹配 order={order_id} account={account_id} instrument={instrument}"
                    )));
                }
                if fees.iter().any(|fee| {
                    fee.amount.raw() != 0
                        && fee
                            .currency
                            .as_deref()
                            .is_none_or(|currency| currency.trim().is_empty())
                }) {
                    return Err(QxError::ReconcileRequired("非零手续费调整缺少币种".into()));
                }
                (
                    EventKind::FeeAdjustment {
                        account_id: account_id.clone(),
                        venue_id: venue_id.clone(),
                        order_id: *order_id,
                        instrument: instrument.clone(),
                        fees: fees.clone(),
                    },
                    Priority::APPLY,
                )
            }
            RuntimeExternalEvent::Accepted {
                client_order_id,
                venue_order_id,
            } => (
                EventKind::Accepted {
                    client_order_id: *client_order_id,
                    venue_order_id: venue_order_id.clone(),
                },
                Priority::FEEDBACK,
            ),
            RuntimeExternalEvent::Fill { fill } => {
                let mut fill = fill.clone();
                self.normalize_fill_account(&mut fill)?;
                (EventKind::Filled { fill }, Priority::APPLY)
            }
            RuntimeExternalEvent::FillWithSpec { fill, .. } => {
                let mut fill = (**fill).clone();
                self.normalize_fill_account(&mut fill)?;
                (EventKind::Filled { fill }, Priority::APPLY)
            }
            RuntimeExternalEvent::Cancelled { client_order_id } => (
                EventKind::Cancelled {
                    client_order_id: *client_order_id,
                },
                Priority::FEEDBACK,
            ),
            RuntimeExternalEvent::CancelPending { client_order_id } => (
                EventKind::CancelPending {
                    client_order_id: *client_order_id,
                },
                Priority::COMMAND,
            ),
            RuntimeExternalEvent::ReconcileRequired { client_order_id } => (
                EventKind::ReconcileRequired {
                    client_order_id: *client_order_id,
                },
                Priority::FEEDBACK,
            ),
        };
        let primary = self.append_fact(
            event_ts,
            receive_ts,
            priority,
            source_seq,
            correlation_id.clone(),
            EventFact {
                metadata: metadata.clone(),
                kind,
            },
        )?;
        let engine_ts = primary.ts;
        let mut derived_seqs = Vec::new();

        match event {
            RuntimeExternalEvent::MarketQuote {
                instrument,
                bid: _bid,
                ask,
                bid_qty: _bid_qty,
                ask_qty: _ask_qty,
                bid_qty_exact: _bid_qty_exact,
                ask_qty_exact: _ask_qty_exact,
                bid_exact: _bid_exact,
                ask_exact: _ask_exact,
            } => {
                self.marks.insert(instrument, ask);
            }
            RuntimeExternalEvent::MarketOrderBook { .. } => {}
            RuntimeExternalEvent::AccountBalanceSnapshot {
                account_id,
                venue_id,
                balances,
            } => {
                self.account_balances
                    .insert((account_id, venue_id), normalize_balances(balances)?);
            }
            RuntimeExternalEvent::AccountPositionSnapshot {
                account_id,
                venue_id,
                positions,
            } => {
                self.account_positions
                    .insert((account_id, venue_id), normalize_positions(positions)?);
            }
            RuntimeExternalEvent::FundingRateSnapshot { snapshot } => {
                self.funding_rates
                    .insert(snapshot.instrument.clone(), snapshot);
            }
            RuntimeExternalEvent::AccountCashflow { cashflow } => {
                let entry_id = self.apply_cashflow(&cashflow, engine_ts)?;
                let entry = self
                    .ledger
                    .entry(entry_id)
                    .cloned()
                    .ok_or_else(|| QxError::Invariant("Cashflow Ledger entry 丢失".into()))?;
                let derived = self.append_at_engine(
                    engine_ts,
                    receive_ts,
                    Priority::APPLY,
                    source_seq,
                    correlation_id.clone(),
                    EventFact {
                        metadata: metadata.derived(format!("ledger:{entry_id}")),
                        kind: EventKind::LedgerApplied { entry },
                    },
                )?;
                derived_seqs.push(derived.seq);
            }
            RuntimeExternalEvent::FeeAdjustment {
                account_id,
                order_id,
                instrument,
                fees,
                ..
            } => {
                let ids = self.ledger.apply_fee_adjustment(
                    &account_id,
                    order_id,
                    &instrument,
                    &fees,
                    engine_ts,
                )?;
                for id in ids {
                    let entry = self.ledger.entry(id).cloned().ok_or_else(|| {
                        QxError::Invariant("FeeAdjustment Ledger entry 丢失".into())
                    })?;
                    let derived = self.append_at_engine(
                        engine_ts,
                        receive_ts,
                        Priority::APPLY,
                        source_seq,
                        correlation_id.clone(),
                        EventFact {
                            metadata: metadata.derived(format!("ledger:{id}")),
                            kind: EventKind::LedgerApplied { entry },
                        },
                    )?;
                    derived_seqs.push(derived.seq);
                }
            }
            RuntimeExternalEvent::Accepted {
                client_order_id, ..
            } => {
                self.apply_accepted(client_order_id)?;
            }
            RuntimeExternalEvent::Fill { fill } => {
                let mut fill = fill;
                self.normalize_fill_account(&mut fill)?;
                self.seen_fills.insert(fill_key(&fill));
                let ids = self.apply_fill(&fill)?;
                for id in ids {
                    let entry = self
                        .ledger
                        .entry(id)
                        .cloned()
                        .ok_or_else(|| QxError::Invariant("Ledger entry 丢失".into()))?;
                    let derived = self.append_at_engine(
                        engine_ts,
                        receive_ts,
                        Priority::APPLY,
                        source_seq,
                        correlation_id.clone(),
                        EventFact {
                            metadata: metadata.derived(format!("ledger:{id}")),
                            kind: EventKind::LedgerApplied { entry },
                        },
                    )?;
                    derived_seqs.push(derived.seq);
                }
            }
            RuntimeExternalEvent::FillWithSpec { fill, spec } => {
                let mut fill = *fill;
                let spec = *spec;
                self.normalize_fill_account(&mut fill)?;
                self.seen_fills.insert(fill_key(&fill));
                let ids = self.apply_fill_with_spec(&fill, &spec)?;
                for id in ids {
                    let entry = self
                        .ledger
                        .entry(id)
                        .cloned()
                        .ok_or_else(|| QxError::Invariant("Ledger entry 丢失".into()))?;
                    let derived = self.append_at_engine(
                        engine_ts,
                        receive_ts,
                        Priority::APPLY,
                        source_seq,
                        correlation_id.clone(),
                        EventFact {
                            metadata: metadata.derived(format!("ledger:{id}")),
                            kind: EventKind::LedgerApplied { entry },
                        },
                    )?;
                    derived_seqs.push(derived.seq);
                }
            }
            RuntimeExternalEvent::Cancelled { client_order_id } => {
                self.apply_cancelled(client_order_id)?;
            }
            RuntimeExternalEvent::CancelPending { client_order_id } => {
                self.apply_cancel_pending(client_order_id)?;
            }
            RuntimeExternalEvent::ReconcileRequired { client_order_id } => {
                self.apply_reconcile_required(client_order_id)?;
            }
        }
        self.persist()?;
        let receipt = RuntimeIngestReceipt {
            derived_seqs,
            engine_ts,
            deduplicated: false,
        };
        Ok(receipt)
    }
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
}
