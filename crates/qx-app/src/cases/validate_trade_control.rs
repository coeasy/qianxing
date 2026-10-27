//! Paper/Live command boundary over the existing Rust control and execution path.

use crate::{AppError, AppErrorCategory, CallerCapability, RunContext};
use qx_control::{
    cancel_order_target_from_command, order_from_submit_command, CommandKind, ControlCommand,
    Permission,
};
use serde::{Deserialize, Serialize};

pub const TRADE_CONTROL_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TradeWorkflow {
    Paper,
    Live,
}

/// Versioned SDK request. The API transport supplies the authenticated operator and grant;
/// fields in this request are intent only and never grant authority by themselves.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TradeControlSpec {
    pub schema_version: u32,
    pub workflow: TradeWorkflow,
    pub command: ControlCommand,
}

impl TradeControlSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("TradeControlSpec JSON 无效: {error}"),
            )
        })
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("TradeControlSpec 序列化失败: {error}"),
            )
        })
    }
}

/// Validate workflow capability, granted operator permission, and the existing executable
/// control-command contract. The returned command is then persisted/enqueued by qx-api and
/// consumed by the configured Rust Paper or venue worker.
pub fn validate_trade_control(
    spec: &TradeControlSpec,
    context: &RunContext,
    granted: Permission,
) -> Result<ControlCommand, AppError> {
    let correlation_id = context.correlation_id().to_string();
    let result = (|| {
        if spec.schema_version != TRADE_CONTROL_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "TradeControlSpec schema_version={} 本版本只认 {}",
                    spec.schema_version, TRADE_CONTROL_SCHEMA_VERSION
                ),
            ));
        }
        let required = match spec.workflow {
            TradeWorkflow::Paper => CallerCapability::Paper,
            TradeWorkflow::Live => CallerCapability::Live,
        };
        context.require(required, "Paper/Live 控制命令")?;
        if context.capability() != required {
            return Err(AppError::new(
                AppErrorCategory::PermissionDenied,
                "调用能力档与请求的 Paper/Live 工作流不一致",
            ));
        }
        spec.command
            .validate_as(granted)
            .map_err(|error| match error {
                qx_control::ControlError::Forbidden => AppError::new(
                    AppErrorCategory::PermissionDenied,
                    "认证主体没有执行该控制命令所需权限",
                ),
                other => AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("控制命令无效: {other:?}"),
                ),
            })?;
        if !spec.command.kind.executed() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("{:?} 在当前构建里没有 Rust 执行者", spec.command.kind),
            ));
        }
        if spec.workflow == TradeWorkflow::Live && spec.command.dry_run {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "Live 工作流拒绝 dry_run 命令；模拟运行请使用 Paper 工作流",
            ));
        }
        if spec.command.kind == CommandKind::SubmitOrder {
            order_from_submit_command(&spec.command).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("SubmitOrder 载荷无效: {error}"),
                )
            })?;
        } else if spec.command.kind == CommandKind::CancelOrder {
            cancel_order_target_from_command(&spec.command).map_err(|error| {
                AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("CancelOrder 载荷无效: {error:?}"),
                )
            })?;
        }
        Ok(spec.command.clone())
    })();
    result.map_err(|error| {
        if error.correlation_id().is_some() {
            error
        } else {
            error.with_correlation_id(correlation_id)
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::{InstrumentId, Order, OrderStatus, Quantity, Side};
    use std::collections::BTreeMap;

    fn submit_command(dry_run: bool) -> ControlCommand {
        let order = Order {
            client_id: 41,
            instrument: InstrumentId::parse("BTCUSDT.BINANCE").unwrap(),
            side: Side::Buy,
            qty: Quantity::from_raw(1),
            limit: None,
            status: OrderStatus::PendingSubmit,
            filled: Quantity::ZERO,
            account_id: "main".into(),
            trace: None,
            policy: None,
            exact_amounts: None,
        };
        ControlCommand {
            command_id: 41,
            request_id: "request-41".into(),
            operator_id: "operator".into(),
            reason: "SDK workflow test".into(),
            kind: CommandKind::SubmitOrder,
            target: "41".into(),
            payload: BTreeMap::from([(
                "order_json".into(),
                serde_json::to_string(&order).unwrap(),
            )]),
            permission: Permission::Trading,
            dry_run,
        }
    }

    fn spec(workflow: TradeWorkflow, dry_run: bool) -> TradeControlSpec {
        TradeControlSpec {
            schema_version: TRADE_CONTROL_SCHEMA_VERSION,
            workflow,
            command: submit_command(dry_run),
        }
    }

    #[test]
    fn paper_and_live_validate_then_return_the_existing_submit_order_command() {
        for (workflow, capability, dry_run) in [
            (TradeWorkflow::Paper, CallerCapability::Paper, false),
            (TradeWorkflow::Live, CallerCapability::Live, false),
        ] {
            let spec = spec(workflow, dry_run);
            let validated = validate_trade_control(
                &spec,
                &RunContext::new(capability, "request-41"),
                Permission::Trading,
            )
            .expect("valid SDK order command");
            assert_eq!(validated, spec.command);
        }
    }

    #[test]
    fn trade_control_rejects_wrong_capability_and_accepts_valid_cancel_commands() {
        let paper = spec(TradeWorkflow::Paper, false);
        assert_eq!(
            validate_trade_control(
                &paper,
                &RunContext::new(CallerCapability::Research, "request-41"),
                Permission::Trading,
            )
            .unwrap_err()
            .category(),
            AppErrorCategory::PermissionDenied
        );
        assert_eq!(
            validate_trade_control(
                &paper,
                &RunContext::new(CallerCapability::Paper, "request-41"),
                Permission::ReadOnly,
            )
            .unwrap_err()
            .category(),
            AppErrorCategory::PermissionDenied
        );
        let mut cancel = paper;
        cancel.command.kind = CommandKind::CancelOrder;
        cancel.command.target = "41".into();
        cancel.command.payload = BTreeMap::from([
            ("account_id".into(), "main".into()),
            ("venue_id".into(), "BINANCE".into()),
        ]);
        let accepted = validate_trade_control(
            &cancel,
            &RunContext::new(CallerCapability::Paper, "request-41"),
            Permission::Trading,
        )
        .expect("valid CancelOrder command");
        assert_eq!(accepted, cancel.command);

        cancel.command.payload.insert("ignored".into(), "x".into());
        assert_eq!(
            validate_trade_control(
                &cancel,
                &RunContext::new(CallerCapability::Paper, "request-41"),
                Permission::Trading,
            )
            .unwrap_err()
            .category(),
            AppErrorCategory::InvalidInput
        );
        let live_dry_run = spec(TradeWorkflow::Live, true);
        assert_eq!(
            validate_trade_control(
                &live_dry_run,
                &RunContext::new(CallerCapability::Live, "request-41"),
                Permission::Trading,
            )
            .unwrap_err()
            .category(),
            AppErrorCategory::InvalidInput
        );
    }
}
