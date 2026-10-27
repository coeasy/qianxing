use super::{
    AlgoExecutionState, AlgoExecutionStatus, AlgoParentOrder, ALGO_EXECUTION_STATE_SCHEMA_VERSION,
};
use qx_core::{Order, OrderStatus};
use qx_zhenlu::MAX_ALGO_CHILDREN;

pub(super) fn validate_input(input: &AlgoParentOrder) -> Result<(), String> {
    input.parent_order.validate()?;
    if input.parent_id.trim().is_empty()
        || input.parent_order.status != OrderStatus::PendingSubmit
        || input.parent_order.filled.raw() != 0
        || input.plan.is_empty()
        || input.plan.len() > MAX_ALGO_CHILDREN
        || input.child_order_id_base == 0
    {
        return Err("算法父单、起始状态、client id 或计划规模无效".into());
    }
    let last_id = input
        .child_order_id_base
        .checked_add(input.plan.len().saturating_sub(1) as u64)
        .ok_or_else(|| "算法子单 client id 范围溢出".to_string())?;
    if last_id == 0 {
        return Err("算法子单 client id 必须为正".into());
    }
    let probe = AlgoExecutionState {
        schema_version: ALGO_EXECUTION_STATE_SCHEMA_VERSION,
        parent_id: input.parent_id.clone(),
        parent_order: input.parent_order.clone(),
        plan: input.plan.clone(),
        child_order_id_base: input.child_order_id_base,
        next_slice_position: 0,
        child_order_ids: Vec::new(),
        last_step_ts: None,
        status: AlgoExecutionStatus::Ready,
        cancel_requested: false,
        failure: None,
    };
    validate_checkpoint(&probe)
}

pub(super) fn validate_checkpoint(state: &AlgoExecutionState) -> Result<(), String> {
    if state.schema_version != ALGO_EXECUTION_STATE_SCHEMA_VERSION
        || state.parent_id.trim().is_empty()
        || state.parent_order.qty.raw() <= 0
        || state.parent_order.filled.raw() != 0
        || state.parent_order.status != OrderStatus::PendingSubmit
        || state.plan.is_empty()
        || state.plan.len() > MAX_ALGO_CHILDREN
        || state.next_slice_position > state.plan.len()
        || state.child_order_ids.len() != state.next_slice_position
        || state.failure.is_some()
            != matches!(
                state.status,
                AlgoExecutionStatus::Failed | AlgoExecutionStatus::ReconcileRequired
            )
    {
        return Err("算法执行 checkpoint 字段或状态不一致".into());
    }
    let last_id = state
        .child_order_id_base
        .checked_add(state.plan.len().saturating_sub(1) as u64)
        .ok_or_else(|| "算法子单 client id 范围溢出".to_string())?;
    if state.child_order_id_base == 0 || last_id == 0 {
        return Err("算法子单 client id 必须为正".into());
    }
    state.parent_order.validate()?;
    let mut total = 0_i128;
    let mut prior_ts = None;
    let mut prior_index = None;
    for slice in &state.plan {
        if slice.parent_id != state.parent_id
            || slice.quantity_raw <= 0
            || prior_ts.is_some_and(|ts| slice.scheduled_ts < ts)
            || prior_index.is_some_and(|index| slice.child_index <= index)
        {
            return Err("算法执行计划父单、数量、时间或子单序号无效".into());
        }
        total = total
            .checked_add(slice.quantity_raw)
            .ok_or_else(|| "算法子单数量求和溢出".to_string())?;
        prior_ts = Some(slice.scheduled_ts);
        prior_index = Some(slice.child_index);
    }
    if total != state.parent_order.qty.raw() {
        return Err(format!(
            "算法计划数量不守恒: parent={} plan={total} order={}",
            state.parent_id,
            state.parent_order.qty.raw()
        ));
    }
    for (position, client_order_id) in state.child_order_ids.iter().enumerate() {
        if *client_order_id != state.child_order_id_base + position as u64 {
            return Err("算法执行 checkpoint 子单 client id 序列漂移".into());
        }
    }
    Ok(())
}

pub(super) fn same_child_identity(existing: &Order, expected: &Order) -> bool {
    existing.same_submission_identity(expected)
}
