use qx_app::{plan_algo_execution, AlgoExecutionSpec, CallerCapability, RunContext};
use qx_core::{FileLock, InstrumentId, Order, OrderStatus, VenueFamily};
use qx_execution::{AlgoExecutionState, AlgoParentOrder};
use qx_runtime::{AlgoExecutionCheckpointStore, RuntimeConfig, WorkerRole};
use serde::Deserialize;
use serde::Serialize;
use std::path::Path;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StartRequest {
    spec: AlgoExecutionSpec,
    parent_order: Order,
    child_order_id_base: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ParentRequest {
    parent_id: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ListRequest {
    #[serde(default)]
    after_parent_id: Option<String>,
    #[serde(default = "default_limit")]
    limit: usize,
}

#[derive(serde::Serialize)]
struct ListResponse {
    items: Vec<AlgoExecutionState>,
    next_after_parent_id: Option<String>,
    has_more: bool,
}

#[derive(Serialize)]
struct ParentSnapshot {
    state: AlgoExecutionState,
    filled_raw: i128,
    remaining_raw: i128,
    child_orders: Vec<Order>,
}

fn default_limit() -> usize {
    100
}

pub(crate) fn handle_algo_execution_request(
    config: &RuntimeConfig,
    operation: &str,
    body: &str,
) -> Result<String, (u16, String)> {
    let store = AlgoExecutionCheckpointStore::new(Path::new(&config.storage.data_dir));
    match operation {
        "start" => {
            let request: StartRequest = parse(body)?;
            request
                .parent_order
                .validate()
                .map_err(|error| (400, error))?;
            if request.parent_order.status != OrderStatus::PendingSubmit
                || request.parent_order.filled.raw() != 0
                || request.parent_order.qty.raw() != request.spec.total_quantity_raw
            {
                return Err((400, "父单必须为未提交、未成交且数量与算法计划一致".into()));
            }
            let account_id = request.parent_order.account_id.as_str();
            let paper_worker = config.workers.iter().find(|worker| {
                worker.enabled
                    && worker.role == WorkerRole::Execution
                    && worker.account_id.as_deref() == Some(account_id)
                    && VenueFamily::parse_option(worker.venue_id.as_deref())
                        == Some(VenueFamily::Paper)
                    && (worker.symbols.is_empty()
                        || worker.symbols.iter().any(|candidate| {
                            InstrumentId::parse(candidate).as_ref()
                                == Some(&request.parent_order.instrument)
                        }))
            });
            let paper_worker = paper_worker.ok_or_else(|| {
                (
                    409,
                    "没有匹配账户、Paper venue 与标的的已启用 Execution worker".into(),
                )
            })?;
            let context =
                RunContext::new(CallerCapability::Research, request.spec.parent_id.clone());
            let plan = plan_algo_execution(&request.spec, &context)
                .map_err(|error| (400, error.to_string()))?;
            let data_root = Path::new(&config.storage.data_dir);
            std::fs::create_dir_all(data_root).map_err(|error| (503, error.to_string()))?;
            let _admission = FileLock::acquire(data_root.join(".algo-execution-admission.lock"))
                .map_err(|error| (503, format!("算法执行身份锁不可用: {error:?}")))?;
            let log_name =
                super::required_account_event_log(paper_worker).map_err(|error| (503, error))?;
            let pipeline = super::open_account_pipeline(
                config,
                data_root,
                &log_name,
                qx_runtime::OutboxRecovery::ReadOnly,
            )
            .map_err(|error| (503, error))?;
            let first_child = request.child_order_id_base;
            let last_child = first_child
                .checked_add(plan.children.len().saturating_sub(1) as u64)
                .ok_or_else(|| (400, "算法子单 client id 范围溢出".into()))?;
            if pipeline
                .orders()
                .iter()
                .any(|order| (first_child..=last_child).contains(&order.client_id))
            {
                return Err((
                    409,
                    "算法子单 client id 与该账户 EventLog 中的订单冲突".into(),
                ));
            }
            let active = store
                .list_active(None, AlgoExecutionCheckpointStore::MAX_DISCOVERY_ENTRIES)
                .map_err(|error| (503, error))?;
            for existing in active
                .iter()
                .filter(|state| state.parent_order.account_id == request.parent_order.account_id)
            {
                let existing_last = existing
                    .child_order_id_base
                    .checked_add(existing.plan.len().saturating_sub(1) as u64)
                    .ok_or_else(|| (503, "已有算法子单 client id 范围溢出".into()))?;
                if first_child <= existing_last && existing.child_order_id_base <= last_child {
                    return Err((409, "算法子单 client id 与活跃父单范围冲突".into()));
                }
            }
            let state = AlgoExecutionState::new(AlgoParentOrder {
                parent_id: request.spec.parent_id,
                parent_order: request.parent_order,
                plan: plan.children,
                child_order_id_base: request.child_order_id_base,
            })
            .map_err(|error| (400, error))?;
            store.create(&state).map_err(|error| (409, error))?;
            serde_json::to_string(&state).map_err(|error| (500, error.to_string()))
        }
        "get" => {
            let request: ParentRequest = parse(body)?;
            let state = store
                .load_optional(&request.parent_id)
                .map_err(|error| (503, error))?
                .ok_or_else(|| (404, "算法父单不存在".into()))?;
            let worker = config
                .workers
                .iter()
                .find(|worker| {
                    worker.enabled
                        && worker.role == WorkerRole::Execution
                        && worker.account_id.as_deref()
                            == Some(state.parent_order.account_id.as_str())
                        && VenueFamily::parse_option(worker.venue_id.as_deref())
                            == Some(VenueFamily::Paper)
                })
                .ok_or_else(|| (503, "算法父单的 Paper Execution worker 配置不存在".into()))?;
            let log_name =
                super::required_account_event_log(worker).map_err(|error| (503, error))?;
            let pipeline = super::open_account_pipeline(
                config,
                Path::new(&config.storage.data_dir),
                &log_name,
                qx_runtime::OutboxRecovery::ReadOnly,
            )
            .map_err(|error| (503, error))?;
            let child_ids = state
                .child_order_ids
                .iter()
                .copied()
                .collect::<std::collections::BTreeSet<_>>();
            let child_orders = pipeline
                .orders()
                .into_iter()
                .filter(|order| child_ids.contains(&order.client_id))
                .collect::<Vec<_>>();
            if child_orders.len() != child_ids.len() {
                return Err((503, "算法子单 EventLog 与 checkpoint 不一致".into()));
            }
            let filled_raw = child_orders
                .iter()
                .try_fold(0_i128, |total, order| total.checked_add(order.filled.raw()))
                .ok_or_else(|| (503, "算法子单成交数量累加溢出".into()))?;
            let remaining_raw = state
                .parent_order
                .qty
                .raw()
                .checked_sub(filled_raw)
                .filter(|remaining| *remaining >= 0)
                .ok_or_else(|| (503, "子单成交数量超过父单数量".into()))?;
            serde_json::to_string(&ParentSnapshot {
                state,
                filled_raw,
                remaining_raw,
                child_orders,
            })
            .map_err(|error| (500, error.to_string()))
        }
        "cancel" => {
            let request: ParentRequest = parse(body)?;
            store
                .load_optional(&request.parent_id)
                .map_err(|error| (503, error))?
                .ok_or_else(|| (404, "算法父单不存在".into()))?;
            let state = store
                .request_cancel(&request.parent_id)
                .map_err(|error| (409, error))?;
            serde_json::to_string(&state).map_err(|error| (500, error.to_string()))
        }
        "list" => {
            let request: ListRequest = parse(body)?;
            if request.limit == 0 || request.limit > 1000 {
                return Err((400, "limit 必须在 1..=1000 之间".into()));
            }
            let mut items = store
                .list_active(request.after_parent_id.as_deref(), request.limit + 1)
                .map_err(|error| (503, error))?;
            let has_more = items.len() > request.limit;
            if has_more {
                items.truncate(request.limit);
            }
            let next_after_parent_id = has_more
                .then(|| items.last().map(|state| state.parent_id.clone()))
                .flatten();
            serde_json::to_string(&ListResponse {
                items,
                next_after_parent_id,
                has_more,
            })
            .map_err(|error| (500, error.to_string()))
        }
        _ => Err((404, "未知算法执行操作".into())),
    }
}

fn parse<T: for<'de> Deserialize<'de>>(body: &str) -> Result<T, (u16, String)> {
    serde_json::from_str(body).map_err(|error| (400, format!("请求 JSON 无效: {error}")))
}
