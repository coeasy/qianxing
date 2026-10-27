use qx_control::{order_from_submit_command, ControlCommand};
use qx_core::OrderStatus;
use qx_zhenlu::SpreadOrderGroupStore;

/// 多腿订单组提交屏障的**唯一实现**，由执行网关在写入任何事实之前调用。
///
/// 组一旦进入 `ReconcileRequired`（已有腿结果未知）或 `HedgeRequired`（已有确认敞口
/// 等待补偿），同组其余腿必须停止自动提交：未知敞口不能靠再加一条腿掩盖，必须先由
/// 对账/补偿链路给出确定事实。判定口径由 `SpreadOrderGroup::blocks_new_leg_submission`
/// 唯一持有。
///
/// 三条 fail-closed 纪律（V10 §4.10 把屏障从 CLI 下沉到这里的原因）：
/// 1. 命令声明了 `spread_group_id` 而提交路径没注入组存储 —— 无法证明组是干净的，拒绝；
/// 2. 注入了组存储但快照读不到 —— 拒绝；
/// 3. 快照处于待对账/待补偿态 —— 拒绝并列出未知腿。
///
/// 不带 `spread_group_id` 的单腿命令不受影响。
pub(crate) fn check_spread_group_barrier(
    store: Option<&dyn SpreadOrderGroupStore>,
    command: &ControlCommand,
) -> Result<(), String> {
    let Some(group_id) = command.payload.get("spread_group_id") else {
        return Ok(());
    };
    let Some(store) = store else {
        return Err(format!(
            "FAIL_CLOSED: 命令 {} 声明属于多腿订单组 {group_id}，但提交路径未注入组快照存储，禁止提交该腿",
            command.command_id
        ));
    };
    let Some(group) = store.load(group_id).map_err(|error| {
        format!("FAIL_CLOSED: 读取多腿订单组 {group_id} 快照失败，禁止提交腿: {error}",)
    })?
    else {
        return Err(format!(
            "FAIL_CLOSED: 命令 {} 属于多腿订单组 {group_id}，但快照不存在，禁止提交该腿",
            command.command_id
        ));
    };
    if group.blocks_new_leg_submission() {
        let unknown_legs = group
            .legs
            .iter()
            .filter(|leg| leg.order.status == OrderStatus::Unknown)
            .map(|leg| leg.leg_id.as_str())
            .collect::<Vec<_>>()
            .join(",");
        return Err(format!(
            "FAIL_CLOSED: 多腿订单组 {group_id} 状态为 {:?}，禁止提交剩余腿（未知腿: {unknown_legs}），须先完成对账/补偿",
            group.status
        ));
    }
    if let Some(in_flight_order_id) = group.submission_in_flight_order_id {
        let requested_order_id = order_from_submit_command(command)
            .ok()
            .map(|order| order.client_id);
        if requested_order_id != Some(in_flight_order_id) {
            return Err(format!(
                "RETRYABLE: 多腿订单组 {group_id} 的订单 {in_flight_order_id} 正在提交，当前腿等待其结果同步后重试"
            ));
        }
    }
    Ok(())
}
