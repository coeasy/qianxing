use crate::*;
use qx_app::{CallerCapability, RunContext, TradeControlSpec, TradeWorkflow};

impl ApiService {
    pub(super) fn submit_workflow_command(
        &self,
        body: &str,
        ts: u64,
        authenticated_operator: Option<&str>,
        workflow: TradeWorkflow,
    ) -> ApiResponse {
        let spec = match TradeControlSpec::from_json(body) {
            Ok(spec) => spec,
            Err(error) => return app::respond(Err(error)),
        };
        if spec.workflow != workflow {
            return app::respond(Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::InvalidInput,
                "Paper/Live 路由与请求 workflow 不一致",
            )));
        }
        let authenticated = authenticated_operator.filter(|id| !id.trim().is_empty());
        let granted = match (&self.policy, authenticated) {
            (Some(policy), Some(operator_id)) => match policy.permission(operator_id) {
                Some(permission) => permission,
                None => {
                    return app::respond(Err(qx_app::AppError::new(
                        qx_app::AppErrorCategory::PermissionDenied,
                        "认证主体未被授予控制权限",
                    )));
                }
            },
            (Some(_), None) => {
                return app::respond(Err(qx_app::AppError::new(
                    qx_app::AppErrorCategory::PermissionDenied,
                    "Paper/Live 控制命令需要认证 Operator",
                )));
            }
            (None, _) if workflow == TradeWorkflow::Live => {
                return app::respond(Err(qx_app::AppError::new(
                    qx_app::AppErrorCategory::PermissionDenied,
                    "Live 控制命令要求配置 Operator 权限策略和客户端认证",
                )));
            }
            (None, _) => spec.command.permission,
        };
        let capability = match workflow {
            TradeWorkflow::Paper => CallerCapability::Paper,
            TradeWorkflow::Live => CallerCapability::Live,
        };
        let context = RunContext::new(capability, &spec.command.request_id);
        let command = match qx_app::validate_trade_control(&spec, &context, granted) {
            Ok(command) => command,
            Err(error) => return app::respond(Err(error)),
        };
        let command_json = match serde_json::to_string(&command) {
            Ok(document) => document,
            Err(error) => {
                return app::respond(Err(qx_app::AppError::new(
                    qx_app::AppErrorCategory::InternalInvariant,
                    format!("编码已验证控制命令失败: {error}"),
                )));
            }
        };
        self.submit_command(&command_json, ts, authenticated)
    }

    pub(super) fn persist_submitted_command(
        &self,
        command: ControlCommand,
        granted: Permission,
        ts: u64,
    ) -> ApiResponse {
        if let Some(submitter) = &self.control_submitter {
            let queued_command = command.clone();
            let (plane, audit) = match submitter(command, granted, ts) {
                Ok(result) => result,
                Err(ControlSubmitError::Unavailable(error)) => {
                    return ApiResponse::json(
                        503,
                        error_json(&format!("control_state_unavailable: {error}")),
                    );
                }
                Err(ControlSubmitError::Rejected(error)) => {
                    let status = match error {
                        ControlError::Forbidden => 403,
                        ControlError::Invalid(_) => 400,
                        _ => 409,
                    };
                    return ApiResponse::json(status, error_json(&format!("{error:?}")));
                }
            };
            match lock_state(&self.state) {
                Ok(mut state) => state.control = plane,
                Err(error) => return ApiResponse::json(503, error_json(&error)),
            }
            if let Some(enqueuer) = &self.command_enqueuer {
                // 入队失败不推翻已经成立的受理：命令已写进控制面，worker 每轮按
                // `pending()` 补入同一条命令。但"补入了"是假设，不是事实——失败必须
                // 有一条能被看见的通道，所以计数 + 落日志，而不是 `let _ =` 吞掉（#189）。
                if let Err(error) = enqueuer(queued_command, ts) {
                    self.metrics
                        .command_enqueue_failures_total
                        .fetch_add(1, Ordering::Relaxed);
                    eprintln!("[qx-api] 控制命令入队失败，等 worker 补入: {error}");
                }
            }
            return serialized_json_response(202, &audit);
        }
        let mut state = match lock_state(&self.state) {
            Ok(state) => state,
            Err(error) => return ApiResponse::json(503, error_json(&error)),
        };
        let result = state.control.submit_as(command, granted, ts);
        match result {
            Ok(audit) => serialized_json_response(202, &audit),
            Err(error) => {
                let status = match error {
                    ControlError::Forbidden => 403,
                    ControlError::Invalid(_) => 400,
                    _ => 409,
                };
                ApiResponse::json(status, error_json(&format!("{error:?}")))
            }
        }
    }
}
