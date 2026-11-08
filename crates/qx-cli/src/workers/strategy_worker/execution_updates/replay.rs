//! EventLog order and fill replay into Strategy execution update DTOs.

use super::*;
use qx_core::{Event, EventKind, OrderStatus};

#[allow(clippy::collapsible_match)] // Keep the ownership lookup and long reconciliation transition together.
pub(super) fn replay_execution_updates(
    events: &[Event],
    strategy_id: &str,
    from_seq: u64,
    through_seq: u64,
) -> Result<ReplayedExecutionUpdates, String> {
    let mut oms = qx_zhenlu::Oms::new();
    let mut updates = ReplayedExecutionUpdates::default();
    for event in events.iter().filter(|event| event.seq <= through_seq) {
        match &event.kind {
            EventKind::OrderSubmitted { order }
                if order_strategy_id(order) == Some(strategy_id) =>
            {
                oms.insert_replayed(order.clone())
                    .map_err(|error| format!("重放 Strategy OrderSubmitted 失败: {error:?}"))?;
                if event.seq >= from_seq {
                    append_update(
                        &mut updates.order_updates,
                        &oms,
                        order.client_id,
                        strategy_id,
                        event,
                        None,
                    )?;
                }
            }
            EventKind::Accepted {
                client_order_id, ..
            } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    oms.accept(*client_order_id)
                        .map_err(|error| format!("重放 Strategy Accepted 失败: {error:?}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            EventKind::Rejected {
                client_order_id,
                reason,
            } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    let order = oms
                        .get_mut(*client_order_id)
                        .ok_or_else(|| "重放 Strategy Rejected 时订单消失".to_string())?;
                    order
                        .status
                        .transition(OrderStatus::Rejected)
                        .map_err(|error| format!("重放 Strategy Rejected 状态失败: {error}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            Some(reason),
                        )?;
                    }
                }
            }
            EventKind::CancelPending { client_order_id } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    let order = oms
                        .get_mut(*client_order_id)
                        .ok_or_else(|| "重放 Strategy CancelPending 时订单消失".to_string())?;
                    order
                        .status
                        .transition(OrderStatus::CancelPending)
                        .map_err(|error| {
                            format!("重放 Strategy CancelPending 状态失败: {error}")
                        })?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            EventKind::Cancelled { client_order_id } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    oms.cancel(*client_order_id)
                        .map_err(|error| format!("重放 Strategy Cancelled 失败: {error:?}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            EventKind::Filled { fill } => {
                if is_strategy_order(&oms, fill.order_id, strategy_id) {
                    let pre_fill_order = oms
                        .get(fill.order_id)
                        .cloned()
                        .ok_or_else(|| "重放 Strategy Filled 时订单消失".to_string())?;
                    oms.apply_fill(fill)
                        .map_err(|error| format!("重放 Strategy Filled 失败: {error:?}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            fill.order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                        updates.fill_updates.push(
                            StrategyFillUpdateV2::from_fill(
                                &pre_fill_order,
                                fill,
                                strategy_id,
                                event.seq,
                            )
                            .map_err(|error| format!("构造 StrategyFillUpdateV2 失败: {error}"))?,
                        );
                    }
                }
            }
            EventKind::ReconcileRequired { client_order_id } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    let order = oms
                        .get_mut(*client_order_id)
                        .ok_or_else(|| "重放 Strategy ReconcileRequired 时订单消失".to_string())?;
                    order
                        .status
                        .transition(OrderStatus::Unknown)
                        .map_err(|error| format!("重放 Strategy Unknown 状态失败: {error}"))?;
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            EventKind::OrderStatusReconciled {
                client_order_id,
                status,
                filled_raw,
                filled_exact,
                ..
            } => {
                if is_strategy_order(&oms, *client_order_id, strategy_id) {
                    let order = oms.get_mut(*client_order_id).ok_or_else(|| {
                        "重放 Strategy OrderStatusReconciled 时订单消失".to_string()
                    })?;
                    let fills_match = if let Some(amounts) = order.exact_amounts {
                        *filled_exact == Some(amounts.filled)
                    } else {
                        order.filled.raw() == *filled_raw
                    };
                    if !fills_match {
                        return Err(format!(
                            "Strategy OrderStatusReconciled 的成交量与本地不一致 order_id={client_order_id}"
                        ));
                    }
                    let path = order.status.reconciliation_path(*status).ok_or_else(|| {
                        format!(
                            "Strategy OrderStatusReconciled 非法状态迁移 order_id={client_order_id} {:?}->{status:?}",
                            order.status
                        )
                    })?;
                    for next in path {
                        order
                            .status
                            .transition(next)
                            .map_err(|error| format!("重放 Strategy 对账状态迁移失败: {error}"))?;
                    }
                    if event.seq >= from_seq {
                        append_update(
                            &mut updates.order_updates,
                            &oms,
                            *client_order_id,
                            strategy_id,
                            event,
                            None,
                        )?;
                    }
                }
            }
            _ => {}
        }
    }
    sort_updates(&mut updates.order_updates);
    sort_fill_updates(&mut updates.fill_updates);
    Ok(updates)
}

fn order_strategy_id(order: &Order) -> Option<&str> {
    order.trace.as_ref()?.strategy_id.as_deref()
}

fn is_strategy_order(oms: &qx_zhenlu::Oms, client_order_id: u64, strategy_id: &str) -> bool {
    oms.get(client_order_id)
        .and_then(order_strategy_id)
        .is_some_and(|owner| owner == strategy_id)
}

fn append_update(
    updates: &mut Vec<StrategyOrderStatusUpdate>,
    oms: &qx_zhenlu::Oms,
    client_order_id: u64,
    strategy_id: &str,
    event: &Event,
    reason: Option<&str>,
) -> Result<(), String> {
    let order = oms
        .get(client_order_id)
        .ok_or_else(|| format!("Strategy EventLog update 缺少 OMS 订单 {client_order_id}"))?;
    updates.push(
        StrategyOrderStatusUpdate::from_order(
            order,
            strategy_id,
            event.seq,
            event.ts,
            order.status,
            reason,
        )
        .map_err(|error| format!("构造 StrategyOrderStatusUpdate 失败: {error}"))?,
    );
    Ok(())
}

pub(super) fn sort_updates(updates: &mut [StrategyOrderStatusUpdate]) {
    updates.sort_by(|left, right| {
        (
            left.ts,
            left.order.account_id.as_str(),
            left.order.instrument.as_str(),
            left.event_seq,
            left.order.order_id,
        )
            .cmp(&(
                right.ts,
                right.order.account_id.as_str(),
                right.order.instrument.as_str(),
                right.event_seq,
                right.order.order_id,
            ))
    });
}

pub(super) fn sort_fill_updates(updates: &mut [StrategyFillUpdateV2]) {
    updates.sort_by(|left, right| {
        (
            left.ts,
            left.order.account_id.as_str(),
            left.order.instrument.as_str(),
            left.event_seq,
            left.order.order_id,
        )
            .cmp(&(
                right.ts,
                right.order.account_id.as_str(),
                right.order.instrument.as_str(),
                right.event_seq,
                right.order.order_id,
            ))
    });
}

#[cfg(test)]
#[allow(clippy::field_reassign_with_default)] // Replay fixtures mutate config fields to make each scenario readable.
mod tests {
    use super::*;
    use qx_core::{InstrumentId, Quantity, Side};
    use qx_runtime::{RuntimeConfig, StrategyCheckpointStateMigrationV1};

    fn order(client_id: u64, strategy_id: &str) -> Order {
        Order {
            client_id,
            instrument: InstrumentId::parse("BTCUSDT.OKX").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(1_000_000_000),
            limit: None,
            status: OrderStatus::Submitted,
            filled: Quantity::ZERO,
            account_id: "acct-a".into(),
            trace: Some(qx_core::OrderTrace {
                strategy_id: Some(strategy_id.into()),
                signal_id: Some(1),
                intent_id: Some(client_id),
                rule_version: Some("v1".into()),
            }),
            policy: None,
            exact_amounts: None,
        }
    }

    #[test]
    fn account_snapshots_replay_for_non_python_strategy_hosts() {
        let mut config: RuntimeConfig = serde_json::from_str(include_str!(
            "../../../../../../deploy/qianxing.runtime.strategy-backtest.example.json"
        ))
        .expect("strategy runtime example parses");
        config.strategy.account_id = Some("main".into());
        config.strategy.venue_id = Some("paper".into());
        config.strategy.python_strategy_spec = None;
        let events = [Event::new(
            0,
            10,
            0,
            EventKind::AccountBalanceSnapshot {
                account_id: "main".into(),
                venue_id: "paper".into(),
                balances: Vec::new(),
            },
        )];

        let updates = replay_account_updates(&events, &config, 0, 0);

        assert_eq!(updates.len(), 1);
        assert_eq!(updates[0].event_seq, 0);
        assert_eq!(updates[0].event, "account_snapshot");
    }

    #[test]
    fn cursor_advancement_is_monotonic_and_durable_shape_round_trips() {
        let mut state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("acct-a.okx".into(), 8)]),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: None,
            committed_fencing_token: 0,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        advance_cursor(
            &mut state,
            &[
                CursorRange {
                    log_name: "acct-a.okx".into(),
                    from_seq: 3,
                    through_seq: 5,
                },
                CursorRange {
                    log_name: "acct-b.binance".into(),
                    from_seq: 0,
                    through_seq: 2,
                },
            ],
        )
        .unwrap();
        assert_eq!(state.next_seq_by_log["acct-a.okx"], 8);
        assert_eq!(state.next_seq_by_log["acct-b.binance"], 3);
        let encoded = serde_json::to_string(&state).unwrap();
        assert_eq!(
            serde_json::from_str::<CursorState>(&encoded).unwrap(),
            state
        );
    }

    #[test]
    fn cursor_load_recovers_backup_after_interrupted_atomic_replace() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!("qx-strategy-cursor-{}-{nonce}", std::process::id()));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let backup = backup_path(&path);
        let state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("acct-a.okx".into(), 17)]),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: None,
            committed_fencing_token: 0,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        save_cursor(&path, &state).unwrap();
        std::fs::rename(&path, &backup).unwrap();
        assert_eq!(load_cursor(&path).unwrap(), state);

        let updated = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("acct-a.okx".into(), 18)]),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: None,
            committed_fencing_token: 0,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        save_cursor(&path, &updated).unwrap();
        assert_eq!(load_cursor(&path).unwrap(), updated);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn prepared_evaluation_round_trips_and_recovers_interrupted_replace() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-prepared-evaluation-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.prepared-evaluation.json");
        let command = ControlCommand {
            command_id: 17,
            request_id: "strategy:worker-a:17".into(),
            operator_id: "worker-a".into(),
            reason: "test decision".into(),
            kind: qx_control::CommandKind::SubmitOrder,
            target: "17".into(),
            payload: BTreeMap::new(),
            permission: qx_control::Permission::Trading,
            dry_run: false,
        };
        let mut evaluation = PreparedEvaluationV3 {
            schema_version: PREPARED_EVALUATION_SCHEMA_VERSION,
            worker_id: "worker-a".into(),
            job_id: "job-a".into(),
            run_id: 23,
            input_sha256: "1".repeat(64),
            facts_sha256: "2".repeat(64),
            config_sha256: "3".repeat(64),
            lease_fencing_token: 9,
            ranges: vec![CursorRange {
                log_name: "acct-a.okx".into(),
                from_seq: 4,
                through_seq: 8,
            }],
            orders: vec![order(17, "worker-a")],
            commands: vec![command],
            spread_group_id: None,
            checkpoint: None,
            completed_command_ids: BTreeSet::from([17]),
            command_results: BTreeMap::from([(17, "ORDER_INTENT_ACCEPTED".into())]),
            payload_sha256: String::new(),
        };
        evaluation.payload_sha256 = prepared_evaluation_payload_sha256(&evaluation).unwrap();
        save_prepared_evaluation(&path, &evaluation).unwrap();
        assert_eq!(
            load_prepared_evaluation(&path).unwrap(),
            Some(evaluation.clone())
        );

        let legacy_path = directory.join("legacy.prepared-evaluation.json");
        let mut legacy = evaluation.clone();
        legacy.schema_version = LEGACY_PREPARED_EVALUATION_SCHEMA_V2;
        legacy.payload_sha256.clear();
        let mut legacy_json = serde_json::to_value(legacy).unwrap();
        legacy_json
            .as_object_mut()
            .unwrap()
            .remove("command_results");
        std::fs::write(&legacy_path, serde_json::to_vec(&legacy_json).unwrap()).unwrap();
        let migrated = load_prepared_evaluation(&legacy_path).unwrap().unwrap();
        assert_eq!(migrated.schema_version, PREPARED_EVALUATION_SCHEMA_VERSION);
        assert!(migrated.command_results.is_empty());

        let mut invalid_result = evaluation.clone();
        invalid_result
            .command_results
            .insert(999, "ORDER_INTENT_ACCEPTED".into());
        assert!(save_prepared_evaluation(&path, &invalid_result).is_err());

        let unknown_path = directory.join("unknown-result.prepared-evaluation.json");
        let mut unknown_result = evaluation.clone();
        unknown_result.completed_command_ids.clear();
        unknown_result
            .command_results
            .insert(17, "OUTCOME_UNKNOWN: simulated storage interruption".into());
        unknown_result.payload_sha256 =
            prepared_evaluation_payload_sha256(&unknown_result).unwrap();
        save_prepared_evaluation(&unknown_path, &unknown_result).unwrap();
        assert_eq!(
            load_prepared_evaluation(&unknown_path).unwrap(),
            Some(unknown_result)
        );
        let capped_error = unknown_command_result(&"λ".repeat(10_000));
        assert!(capped_error.len() <= MAX_PREPARED_COMMAND_RESULT_BYTES);

        let backup = prepared_evaluation_backup_path(&path);
        std::fs::rename(&path, &backup).unwrap();
        assert_eq!(
            load_prepared_evaluation(&path).unwrap(),
            Some(evaluation.clone())
        );

        let mut invalid = evaluation.clone();
        invalid.facts_sha256 = "bad".into();
        assert!(save_prepared_evaluation(&path, &invalid).is_err());
        clear_prepared_evaluation(&path).unwrap();

        let committed_path = directory.join("committed.prepared-evaluation.json");
        save_prepared_evaluation(&committed_path, &evaluation).unwrap();
        let committed_backup = prepared_evaluation_backup_path(&committed_path);
        std::fs::rename(&committed_path, &committed_backup).unwrap();
        let cursor_path = directory.join("committed.execution-cursor.json");
        let mut cursor = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: None,
            committed_fencing_token: 0,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        mark_run_committed(&mut cursor, &cursor_path, evaluation.run_id, 1).unwrap();
        assert!(clear_committed_prepared_evaluation(&cursor, &committed_path).unwrap());
        assert!(load_prepared_evaluation(&committed_path).unwrap().is_none());
        assert!(!committed_backup.exists());
        std::fs::remove_file(legacy_path).unwrap();
        std::fs::remove_file(unknown_path).unwrap();
        std::fs::remove_file(cursor_path).unwrap();
        std::fs::remove_dir(directory).unwrap();
    }

    #[test]
    fn class_checkpoint_and_execution_cursor_commit_as_one_recoverable_snapshot() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-strategy-checkpoint-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let mut strategy = StrategyRuntimeConfig::default();
        strategy.id = Some("class-strategy".into());
        strategy.version = "1.2.0".into();
        strategy.python_module = Some("example_strategy".into());
        strategy.python_strategy_spec = Some(serde_json::json!({"parameters": {"window": 4}}));
        let mut state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: None,
            committed_fencing_token: 0,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        let checkpoint = StrategyCheckpointV1 {
            schema_version: 1,
            strategy_id: "class-strategy".into(),
            strategy_version: "1.2.0".into(),
            adapter_state: serde_json::json!({"schema_version": 1}),
            strategy_state: serde_json::json!({"counter": 2}),
        };
        let ranges = vec![CursorRange {
            log_name: "acct-a.okx".into(),
            from_seq: 0,
            through_seq: 8,
        }];

        commit_strategy_checkpoint(
            &mut state,
            &path,
            &strategy,
            77,
            1,
            &ranges,
            checkpoint.clone(),
        )
        .unwrap();

        let recovered = load_cursor(&path).unwrap();
        assert_eq!(recovered, state);
        assert_eq!(recovered.next_seq_by_log["acct-a.okx"], 9);
        assert_eq!(strategy_checkpoint(&recovered), Some(&checkpoint));
        assert!(is_committed_run(&recovered, 77));
        assert_eq!(recovered.committed_fencing_token, 1);
        assert_eq!(
            recovered.checkpoint_sha256.as_deref(),
            Some(checkpoint_sha256(&checkpoint).unwrap().as_str())
        );
        validate_checkpoint_binding(&recovered, &strategy).unwrap();

        let legacy_path = directory.join("legacy.execution-cursor.json");
        let mut legacy_json = serde_json::to_value(&recovered).unwrap();
        let legacy_object = legacy_json.as_object_mut().unwrap();
        legacy_object.insert("schema_version".into(), serde_json::json!(1));
        legacy_object.remove("checkpoint_sha256");
        legacy_object.remove("committed_fencing_token");
        std::fs::write(&legacy_path, serde_json::to_vec(&legacy_json).unwrap()).unwrap();
        let migrated = load_cursor(&legacy_path).unwrap();
        assert_eq!(migrated.schema_version, CURSOR_SCHEMA_VERSION);
        assert_eq!(migrated.committed_run_id, Some(77));
        assert_eq!(migrated.committed_fencing_token, 0);
        assert_eq!(migrated.checkpoint_sha256, recovered.checkpoint_sha256);
        let migration = migrated.migration.as_ref().unwrap();
        assert_eq!(
            migration.source_schema_version,
            LEGACY_CURSOR_SCHEMA_VERSION
        );
        assert_eq!(migration.target_schema_version, CURSOR_SCHEMA_VERSION);
        assert_eq!(migration.result, CURSOR_V1_TO_V3_MIGRATION_RESULT);

        let corrupted_path = directory.join("corrupted.execution-cursor.json");
        let mut corrupted = serde_json::to_value(&recovered).unwrap();
        corrupted["checkpoint_sha256"] = serde_json::json!("0".repeat(64));
        std::fs::write(&corrupted_path, serde_json::to_vec(&corrupted).unwrap()).unwrap();
        assert!(load_cursor(&corrupted_path)
            .unwrap_err()
            .contains("checksum 缺失或不匹配"));

        let mut changed_strategy = strategy;
        changed_strategy.version = "1.3.0".into();
        assert!(validate_checkpoint_binding(&recovered, &changed_strategy)
            .unwrap_err()
            .contains("不匹配"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cursor_fencing_uses_run_identity_and_rejects_only_stale_same_run_owners() {
        let state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: Some(77),
            committed_fencing_token: 9,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        assert!(validate_run_fence(&state, 77, 8)
            .unwrap_err()
            .contains("旧 owner"));
        assert!(validate_run_fence(&state, 76, 100).is_ok());
        assert!(validate_run_fence(&state, 77, 10).is_ok());
        assert!(validate_run_fence(&state, 78, 1).is_ok());
        assert!(validate_run_fence(&state, 78, 0).is_err());
    }

    #[test]
    fn cursor_commit_generation_accepts_descending_stable_run_ids_and_fences_stale_snapshots() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-cursor-run-order-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let mut state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: None,
            committed_fencing_token: 0,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        mark_run_committed(&mut state, &path, 77, 1).unwrap();
        let stale_snapshot = state.clone();
        mark_run_committed(&mut state, &path, 76, 1).unwrap();
        assert_eq!(state.committed_run_id, Some(76));
        assert_eq!(state.commit_generation, 2);
        assert!(save_cursor(&path, &stale_snapshot)
            .unwrap_err()
            .contains("较新提交"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cursor_v2_migrates_to_monotone_commit_generation() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-cursor-v2-migration-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let previous = CursorState {
            schema_version: PREVIOUS_CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: Some(77),
            committed_fencing_token: 3,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        std::fs::write(&path, serde_json::to_vec(&previous).unwrap()).unwrap();
        let migrated = load_cursor(&path).unwrap();
        assert_eq!(migrated.schema_version, CURSOR_SCHEMA_VERSION);
        assert_eq!(migrated.committed_run_id, Some(77));
        assert_eq!(migrated.commit_generation, 1);
        assert_eq!(
            migrated.migration.as_ref().unwrap().result,
            CURSOR_V2_TO_V3_MIGRATION_RESULT
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cursor_write_lock_rejects_an_owner_fenced_out_after_its_snapshot() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-cursor-owner-fence-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let mut current = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: None,
            committed_fencing_token: 0,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        mark_run_committed(&mut current, &path, 77, 1).unwrap();
        let mut stale_owner = current.clone();
        current.committed_fencing_token = 2;
        save_cursor(&path, &current).unwrap();

        let error = save_cursor(&path, &stale_owner).unwrap_err();
        assert!(error.contains("旧 fencing token"));
        assert_eq!(load_cursor(&path).unwrap().committed_fencing_token, 2);
        stale_owner.committed_fencing_token = 0;
        assert!(save_cursor(&path, &stale_owner)
            .unwrap_err()
            .contains("旧 fencing token"));
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn c_abi_checkpoint_binding_tracks_verified_plugin_digest() {
        let mut strategy = StrategyRuntimeConfig::default();
        strategy.id = Some("native-strategy".into());
        strategy.version = "2.0.0".into();
        strategy.c_abi_library = Some("strategy.dll".into());
        strategy.c_abi_sha256 = Some("a".repeat(64));
        let checkpoint = StrategyCheckpointV1 {
            schema_version: 1,
            strategy_id: "native-strategy".into(),
            strategy_version: "2.0.0".into(),
            adapter_state: serde_json::json!({ "schema_version": 1, "state_mode": "c-abi" }),
            strategy_state: serde_json::json!({ "counter": 4 }),
        };
        let state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("account.okx".into(), 9)]),
            strategy_checkpoint: Some(checkpoint.clone()),
            strategy_binding_sha256: Some(strategy_binding_sha256(&strategy).unwrap()),
            checkpoint_sha256: Some(checkpoint_sha256(&checkpoint).unwrap()),
            committed_run_id: Some(77),
            committed_fencing_token: 1,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        validate_checkpoint_binding_for_mode(&state, &strategy, true).unwrap();

        strategy.c_abi_sha256 = Some("b".repeat(64));
        assert!(
            validate_checkpoint_binding_for_mode(&state, &strategy, true)
                .unwrap_err()
                .contains("不匹配")
        );
    }

    #[test]
    fn external_process_checkpoint_binding_tracks_artifact_digest_and_fails_closed() {
        let mut strategy = StrategyRuntimeConfig::default();
        strategy.id = Some("external-strategy".into());
        strategy.version = "1.0.0".into();
        strategy.external_executable = Some("strategy-worker.exe".into());
        strategy.strategy_artifact_sha256 = Some("c".repeat(64));
        let checkpoint = StrategyCheckpointV1 {
            schema_version: 1,
            strategy_id: "external-strategy".into(),
            strategy_version: "1.0.0".into(),
            adapter_state: serde_json::json!({
                "schema_version": 1,
                "state_mode": "external-process"
            }),
            strategy_state: serde_json::json!({ "processed_requests": 4 }),
        };
        let state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("account.okx".into(), 5)]),
            strategy_checkpoint: Some(checkpoint.clone()),
            strategy_binding_sha256: Some(strategy_binding_sha256(&strategy).unwrap()),
            checkpoint_sha256: Some(checkpoint_sha256(&checkpoint).unwrap()),
            committed_run_id: Some(5),
            committed_fencing_token: 1,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        validate_checkpoint_binding_for_mode(&state, &strategy, true).unwrap();

        strategy.strategy_artifact_sha256 = Some("d".repeat(64));
        assert!(
            validate_checkpoint_binding_for_mode(&state, &strategy, true)
                .unwrap_err()
                .contains("不匹配")
        );
        strategy.strategy_artifact_sha256 = Some("c".repeat(64));
        strategy.external_args.push("--mode=changed".into());
        assert!(
            validate_checkpoint_binding_for_mode(&state, &strategy, true)
                .unwrap_err()
                .contains("不匹配")
        );
        assert!(
            validate_checkpoint_binding_for_mode(&state, &strategy, false)
                .unwrap_err()
                .contains("不支持恢复")
        );
    }

    #[test]
    fn authorized_revision_migration_replaces_only_strategy_state_and_persists_receipt() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-strategy-checkpoint-revision-migration-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");

        let mut source_strategy = StrategyRuntimeConfig::default();
        source_strategy.id = Some("migrated-class".into());
        source_strategy.version = "1.0.0".into();
        source_strategy.python_module = Some("strategy_module".into());
        source_strategy.python_strategy_spec = Some(serde_json::json!({
            "parameters": {"window": 4}
        }));
        let source_checkpoint = StrategyCheckpointV1 {
            schema_version: 1,
            strategy_id: "migrated-class".into(),
            strategy_version: "1.0.0".into(),
            adapter_state: serde_json::json!({
                "schema_version": 1,
                "bars_seen": 9,
                "last_bar_ts": 90,
                "warmup_completed": true,
                "update_cursors": {}
            }),
            strategy_state: serde_json::json!({"old_counter": 8}),
        };
        let source_binding = strategy_binding_sha256(&source_strategy).unwrap();
        let source_checkpoint_digest = checkpoint_sha256(&source_checkpoint).unwrap();
        let mut state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::from([("acct-a.okx".into(), 17)]),
            strategy_checkpoint: Some(source_checkpoint.clone()),
            strategy_binding_sha256: Some(source_binding.clone()),
            checkpoint_sha256: Some(source_checkpoint_digest.clone()),
            committed_run_id: Some(88),
            committed_fencing_token: 3,
            commit_generation: 7,
            migration: None,
            deployment_revision: Some(1),
            deployment_migration: None,
        };
        save_cursor(&path, &state).unwrap();

        let mut target_strategy = source_strategy.clone();
        target_strategy.version = "2.0.0".into();
        target_strategy.python_strategy_spec = Some(serde_json::json!({
            "parameters": {"window": 6}
        }));
        let target_strategy_state = serde_json::json!({"new_counter": 13});
        let mut migration = StrategyCheckpointStateMigrationV1 {
            schema_version: 1,
            source_deployment_revision: 1,
            target_deployment_revision: 2,
            source_strategy_binding_sha256: source_binding.clone(),
            source_checkpoint_sha256: source_checkpoint_digest.clone(),
            target_strategy_state: target_strategy_state.clone(),
        };
        let mut invalid_strategy = target_strategy.clone();
        migration.source_checkpoint_sha256 = "0".repeat(64);
        invalid_strategy.checkpoint_state_migration = Some(migration.clone());
        assert!(bind_cursor_to_deployment_revision(
            &mut state,
            &path,
            &invalid_strategy,
            true,
            Some(2),
        )
        .unwrap_err()
        .contains("not authorized"));
        assert_eq!(state.strategy_checkpoint.as_ref(), Some(&source_checkpoint));
        assert_eq!(load_cursor(&path).unwrap(), state);

        migration.source_checkpoint_sha256 = source_checkpoint_digest.clone();
        target_strategy.checkpoint_state_migration = Some(migration.clone());
        let migration_source = state.clone();
        let backup = backup_path(&path);
        std::fs::create_dir(&backup).unwrap();
        let persistence_error =
            bind_cursor_to_deployment_revision(&mut state, &path, &target_strategy, true, Some(2))
                .unwrap_err();
        assert!(
            persistence_error.contains("清理策略执行事实游标旧恢复副本失败"),
            "{persistence_error}"
        );
        assert_eq!(state, migration_source);
        assert_eq!(load_cursor(&path).unwrap(), migration_source);
        assert!(!path
            .with_extension(format!("cursor.tmp.{}", std::process::id()))
            .exists());
        std::fs::remove_dir(&backup).unwrap();

        bind_cursor_to_deployment_revision(&mut state, &path, &target_strategy, true, Some(2))
            .unwrap();

        let converted = state.strategy_checkpoint.as_ref().unwrap();
        assert_eq!(converted.strategy_id, "migrated-class");
        assert_eq!(converted.strategy_version, "2.0.0");
        assert_eq!(converted.strategy_state, target_strategy_state);
        assert_eq!(converted.adapter_state, source_checkpoint.adapter_state);
        assert_eq!(state.next_seq_by_log["acct-a.okx"], 17);
        assert_eq!(state.deployment_revision, Some(2));
        assert_eq!(state.commit_generation, 7);
        let receipt = state.deployment_migration.as_ref().unwrap();
        assert_eq!(receipt.source_revision, Some(1));
        assert_eq!(receipt.target_revision, 2);
        assert_eq!(receipt.result, CURSOR_DEPLOYMENT_STATE_CONVERTED);
        let target_checkpoint_digest = checkpoint_sha256(converted).unwrap();
        assert_eq!(
            receipt.converter_sha256.as_deref(),
            Some(qx_strategy::sha256_hex(&serde_json::to_vec(&migration).unwrap()).as_str())
        );
        assert_eq!(
            receipt.source_checkpoint_sha256.as_deref(),
            Some(source_checkpoint_digest.as_str())
        );
        assert_eq!(
            receipt.target_checkpoint_sha256.as_deref(),
            Some(target_checkpoint_digest.as_str())
        );
        assert_eq!(load_cursor(&path).unwrap(), state);

        let before_stale_rebind = state.clone();
        assert!(bind_cursor_to_deployment_revision(
            &mut state,
            &path,
            &source_strategy,
            true,
            Some(1),
        )
        .unwrap_err()
        .contains("stale Strategy deployment revision"));
        assert_eq!(state, before_stale_rebind);
        assert_eq!(load_cursor(&path).unwrap(), before_stale_rebind);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn failed_checkpoint_cursor_write_preserves_committed_state_and_cleans_temp_file() {
        let nonce = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let directory = std::env::current_dir()
            .unwrap()
            .join("target")
            .join(format!(
                "qx-strategy-checkpoint-write-failure-{}-{nonce}",
                std::process::id()
            ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("worker.execution-cursor.json");
        let mut strategy = StrategyRuntimeConfig::default();
        strategy.id = Some("class-strategy".into());
        strategy.version = "1.2.0".into();
        strategy.python_module = Some("example_strategy".into());
        strategy.python_strategy_spec = Some(serde_json::json!({"parameters": {"window": 4}}));
        let old_checkpoint = StrategyCheckpointV1 {
            schema_version: 1,
            strategy_id: "class-strategy".into(),
            strategy_version: "1.2.0".into(),
            adapter_state: serde_json::json!({"schema_version": 1, "last_bar_ts": 10}),
            strategy_state: serde_json::json!({"counter": 2}),
        };
        let mut state = CursorState {
            schema_version: CURSOR_SCHEMA_VERSION,
            next_seq_by_log: BTreeMap::new(),
            strategy_checkpoint: None,
            strategy_binding_sha256: None,
            checkpoint_sha256: None,
            committed_run_id: None,
            committed_fencing_token: 0,
            commit_generation: 0,
            migration: None,
            deployment_revision: None,
            deployment_migration: None,
        };
        let initial_ranges = vec![CursorRange {
            log_name: "acct-a.okx".into(),
            from_seq: 0,
            through_seq: 8,
        }];
        commit_strategy_checkpoint(
            &mut state,
            &path,
            &strategy,
            77,
            1,
            &initial_ranges,
            old_checkpoint.clone(),
        )
        .unwrap();
        let committed = state.clone();

        // A directory at the backup filename deterministically makes the
        // pre-commit backup cleanup fail on Windows and Unix alike.
        let backup = backup_path(&path);
        std::fs::create_dir(&backup).unwrap();
        let mut failed_checkpoint = old_checkpoint;
        failed_checkpoint.strategy_state = serde_json::json!({"counter": 3});
        let next_ranges = vec![CursorRange {
            log_name: "acct-a.okx".into(),
            from_seq: 9,
            through_seq: 11,
        }];
        let error = commit_strategy_checkpoint(
            &mut state,
            &path,
            &strategy,
            78,
            2,
            &next_ranges,
            failed_checkpoint,
        )
        .unwrap_err();

        assert!(error.contains("清理策略执行事实游标旧恢复副本失败"));
        assert_eq!(state, committed);
        assert_eq!(load_cursor(&path).unwrap(), committed);
        let temporary = path.with_extension(format!("cursor.tmp.{}", std::process::id()));
        assert!(
            !temporary.exists(),
            "failed cursor temp file must be removed"
        );
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn execution_ranges_are_part_of_idempotency_without_changing_market_manifest() {
        let (mut job, mut run) =
            qx_scheduler::live_strategy_job(qx_scheduler::LiveStrategyJobInput {
                job_version: "strategy-v1",
                worker_id: "strategy-a",
                instrument: Some("BTCUSDT.OKX"),
                data_fingerprint: 99,
                trading_day: "2026-10-25".into(),
                lease_now: 1_800_000_000,
                environment: "paper",
            });
        let market_run_id = run.run_id;
        let range = CursorRange {
            log_name: "acct-a.okx".into(),
            from_seq: 12,
            through_seq: 14,
        };
        attach_ranges_to_job(&mut job, &mut run, std::slice::from_ref(&range)).unwrap();
        assert_ne!(run.run_id, market_run_id);
        assert_eq!(run.run_id, job.stable_key(&run.trading_day));
        assert_eq!(run.manifest_digest, Some(99));
        assert_eq!(ranges_from_job(&job).unwrap(), vec![range]);
        assert!(job.idempotency_key.contains(":execution:"));
    }

    #[test]
    fn replay_reconstructs_prior_order_state_and_emits_only_the_requested_strategy_range() {
        let events = vec![
            Event::new(
                0,
                10,
                5,
                EventKind::OrderSubmitted {
                    order: order(11, "strategy-a"),
                },
            ),
            Event::new(
                1,
                11,
                5,
                EventKind::Accepted {
                    client_order_id: 11,
                    venue_order_id: Some("venue-11".into()),
                },
            ),
            Event::new(
                2,
                12,
                5,
                EventKind::OrderSubmitted {
                    order: order(12, "strategy-a"),
                },
            ),
            Event::new(
                3,
                13,
                5,
                EventKind::Rejected {
                    client_order_id: 12,
                    reason: "venue rejected".into(),
                },
            ),
            Event::new(
                4,
                14,
                5,
                EventKind::OrderSubmitted {
                    order: order(13, "another-strategy"),
                },
            ),
        ];
        let updates = replay_execution_updates(&events, "strategy-a", 1, 4)
            .unwrap()
            .order_updates;
        assert_eq!(updates.len(), 3);
        assert_eq!(updates[0].event_seq, 1);
        assert_eq!(
            updates[0].status,
            qx_strategy::StrategyOrderStatus::Accepted
        );
        assert_eq!(updates[1].event_seq, 2);
        assert_eq!(
            updates[1].status,
            qx_strategy::StrategyOrderStatus::Submitted
        );
        assert_eq!(updates[2].event_seq, 3);
        assert_eq!(
            updates[2].status,
            qx_strategy::StrategyOrderStatus::Rejected
        );
        assert_eq!(updates[2].reason.as_deref(), Some("venue rejected"));

        let initial = replay_execution_updates(&events, "strategy-a", 0, 0)
            .unwrap()
            .order_updates;
        assert_eq!(initial.len(), 1);
        assert_eq!(initial[0].event_seq, 0);
        assert_eq!(
            initial[0].status,
            qx_strategy::StrategyOrderStatus::Submitted
        );
    }

    #[test]
    fn replay_emits_event_log_bound_fill_with_pre_fill_order_snapshot() {
        let mut fill = qx_core::Fill {
            order_id: 11,
            qty: Quantity::from_raw(500_000_000),
            price: qx_core::Price::from_raw(25_000_000_000),
            ts: 12,
            account_id: "acct-a".into(),
            strategy_id: Some("strategy-a".into()),
            ..qx_core::Fill::default()
        };
        fill.venue_id = Some("OKX".into());
        let events = vec![
            Event::new(
                0,
                10,
                5,
                EventKind::OrderSubmitted {
                    order: order(11, "strategy-a"),
                },
            ),
            Event::new(
                1,
                11,
                5,
                EventKind::Accepted {
                    client_order_id: 11,
                    venue_order_id: Some("venue-11".into()),
                },
            ),
            Event::new(2, 12, 5, EventKind::Filled { fill }),
        ];

        let replayed = replay_execution_updates(&events, "strategy-a", 2, 2).unwrap();
        assert_eq!(replayed.order_updates.len(), 1);
        assert_eq!(replayed.order_updates[0].event_seq, 2);
        assert_eq!(replayed.fill_updates.len(), 1);
        assert_eq!(replayed.fill_updates[0].event_seq, 2);
        assert_eq!(replayed.fill_updates[0].ts, 12);
        assert_eq!(replayed.fill_updates[0].order.filled_raw, 0);
        assert_eq!(replayed.fill_updates[0].fill.qty.raw(), 500_000_000);
        assert_eq!(replayed.fill_updates[0].fill.order_id, 11);
    }

    #[test]
    fn frozen_range_retry_replays_multiple_partial_fills_without_absorbing_new_tail() {
        let partial_fill = |ts| qx_core::Fill {
            order_id: 11,
            qty: Quantity::from_raw(500_000_000),
            price: qx_core::Price::from_raw(25_000_000_000),
            ts,
            account_id: "acct-a".into(),
            strategy_id: Some("strategy-a".into()),
            ..qx_core::Fill::default()
        };
        let mut events = vec![
            Event::new(
                0,
                10,
                5,
                EventKind::OrderSubmitted {
                    order: order(11, "strategy-a"),
                },
            ),
            Event::new(
                1,
                11,
                5,
                EventKind::Accepted {
                    client_order_id: 11,
                    venue_order_id: Some("venue-11".into()),
                },
            ),
            Event::new(
                2,
                12,
                5,
                EventKind::Filled {
                    fill: partial_fill(12),
                },
            ),
            Event::new(
                3,
                13,
                5,
                EventKind::Filled {
                    fill: partial_fill(13),
                },
            ),
        ];

        let first_attempt = replay_execution_updates(&events, "strategy-a", 2, 3).unwrap();
        assert_eq!(first_attempt.order_updates.len(), 2);
        assert_eq!(first_attempt.fill_updates.len(), 2);
        assert_eq!(
            first_attempt
                .fill_updates
                .iter()
                .map(|update| (update.event_seq, update.order.filled_raw))
                .collect::<Vec<_>>(),
            [(2, 0), (3, 500_000_000)]
        );
        assert_eq!(
            first_attempt
                .order_updates
                .iter()
                .map(|update| (update.event_seq, update.order.filled_raw))
                .collect::<Vec<_>>(),
            [(2, 500_000_000), (3, 1_000_000_000)]
        );

        // The job retry reuses its frozen [2, 3] range even after the EventLog grows.
        events.push(Event::new(
            4,
            14,
            5,
            EventKind::OrderSubmitted {
                order: order(12, "strategy-a"),
            },
        ));
        let retry = replay_execution_updates(&events, "strategy-a", 2, 3).unwrap();
        assert_eq!(retry, first_attempt);
    }
}
