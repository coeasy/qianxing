//! CLI adapters for versioned research and event report reads.

use super::*;

pub(super) fn read_bar_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = BacktestOutcome::from_json(&read_document(outcome_path)?)?;
    let report = read_run_report_v1(&outcome, &context(&outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Bar 研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_depth_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = DepthBacktestOutcome::from_json(&read_document(outcome_path)?)?;
    let report = read_depth_run_report_v1(&outcome, &context(&outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("深度研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_provider_bar_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome: ProviderBacktestOutcome = serde_json::from_str(&read_document(outcome_path)?)
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("Provider Bar 回测结果 JSON 无效: {error}"),
            )
        })?;
    let report = read_provider_backtest_report_v1(&outcome, &context(&outcome.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Provider Bar 研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_provider_depth_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome: ProviderDepthBacktestOutcome = serde_json::from_str(&read_document(outcome_path)?)
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("Provider 深度回测结果 JSON 无效: {error}"),
            )
        })?;
    let report =
        read_provider_depth_backtest_report_v1(&outcome, &context(&outcome.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("Provider 深度研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_account_report(outcome_path: &Path) -> Result<String, AppError> {
    let payload = read_document(outcome_path)?;
    let outcome: serde_json::Value = serde_json::from_str(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("多账户结果 JSON 无效: {error}"),
        )
    })?;
    let batch_id = outcome
        .get("batch_id")
        .and_then(serde_json::Value::as_str)
        .unwrap_or("multi-account-report");
    let report = read_multi_account_report_v1(&outcome, &context(batch_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多账户研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_account_joint_report(outcome_path: &Path) -> Result<String, AppError> {
    let input: serde_json::Value =
        serde_json::from_str(&read_document(outcome_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("联合账户报告请求 JSON 无效: {error}"),
            )
        })?;
    let (outcome_value, options_value) = if let Some(object) = input.as_object() {
        if let Some(outcome) = object.get("outcome") {
            if object
                .keys()
                .any(|key| !matches!(key.as_str(), "outcome" | "joint_account_event_pages"))
            {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    "联合账户报告请求包含未知字段",
                ));
            }
            (
                outcome.clone(),
                object.get("joint_account_event_pages").cloned(),
            )
        } else {
            (input.clone(), None)
        }
    } else {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "联合账户报告请求必须是 JSON 对象",
        ));
    };
    let outcome: MultiAccountJointBacktestOutcome =
        serde_json::from_value(outcome_value).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("联合多账户结果 JSON 无效: {error}"),
            )
        })?;
    let options = options_value
        .map(|pages| {
            serde_json::from_value::<MultiAccountJointReportReadOptionsV1>(serde_json::json!({
                "joint_account_event_pages": pages
            }))
        })
        .transpose()
        .map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("联合账户事件分页选项无效: {error}"),
            )
        })?
        .unwrap_or_default();
    let report = read_multi_account_joint_report_v1_with_options(
        &outcome,
        &context(&outcome.run_id),
        &options,
    )?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("联合多账户研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_leg_report(outcome_path: &Path) -> Result<String, AppError> {
    let outcome = MultiLegBacktestOutcome::from_json(&read_document(outcome_path)?)?;
    let report = read_multi_leg_report_facts_v1(&outcome, &context(&outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多腿研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_leg_research_report(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiLegResearchReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多腿完整研究报告请求 JSON 无效: {error}"),
            )
        })?;
    let report = read_multi_leg_research_report_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多腿完整研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_leg_event_page(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiLegEventReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多腿事件报告读取请求 JSON 无效: {error}"),
            )
        })?;
    let report = read_multi_leg_event_report_page_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多腿事件报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_market_event_page(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiMarketEventReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多市场事件报告读取请求 JSON 无效: {error}"),
            )
        })?;
    let report =
        read_multi_market_event_report_page_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场事件报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_market_depth_event_page(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiMarketDepthEventReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多市场深度事件报告读取请求 JSON 无效: {error}"),
            )
        })?;
    let report =
        read_multi_market_depth_event_report_page_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场深度事件报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_market_research_report(request_path: &Path) -> Result<String, AppError> {
    let request: qx_app::MultiMarketResearchReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多市场完整研究报告请求 JSON 无效: {error}"),
            )
        })?;
    let report = read_multi_market_research_report_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场完整研究报告 DTO 序列化失败: {error}"),
        )
    })
}

pub(super) fn read_multi_market_depth_research_report(
    request_path: &Path,
) -> Result<String, AppError> {
    let request: qx_app::MultiMarketDepthResearchReportReadRequestV1 =
        serde_json::from_str(&read_document(request_path)?).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("多市场深度完整研究报告请求 JSON 无效: {error}"),
            )
        })?;
    let report =
        read_multi_market_depth_research_report_v1(&request, &context(&request.outcome.run_id))?;
    serde_json::to_string(&report).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("多市场深度完整研究报告 DTO 序列化失败: {error}"),
        )
    })
}
