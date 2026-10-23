//! 运行证据包 CLI 适配：解码多输入帧后交由 qx-app 准备并持久化 bundle。

use crate::{build_identity, read_bar_frame_for_backtest};
use std::path::{Path, PathBuf};

/// 把一次已完成回测的可复核事实聚成统一 EvidenceBundle 的 `run` 世代并落盘。
///
/// 调用点必须在 `recompute_declared_backtest_input` 通过之后：那一步已经重算过每一份产物的
/// SHA-256，`artifact_digests_verified=true` 才是**真的**而不是一句声明。
pub(crate) fn write_run_evidence(
    summary_path: &Path,
    summary: &serde_json::Value,
    report_analysis: Option<&serde_json::Value>,
    strategy_diagnostics: Option<(&Path, &serde_json::Value)>,
) -> Result<PathBuf, String> {
    let dataset_kind =
        qx_app::artifact_store::run_evidence_summary_str(summary, &["input", "kind"])?;
    let components = if dataset_kind == "multi-barframe" {
        ["primary", "reference"]
            .into_iter()
            .map(|role| {
                let path = qx_app::artifact_store::run_evidence_summary_str(
                    summary,
                    &["input", "legs", role, "path"],
                )?;
                let path = Path::new(&path)
                    .canonicalize()
                    .map_err(|error| format!("解析 {role} BarFrame 路径失败: {error}"))?;
                let frame = read_bar_frame_for_backtest(&path)?;
                qx_app::artifact_store::run_evidence_dataset_component(
                    summary,
                    role,
                    frame.ts.len() as u64,
                    path.to_string_lossy().to_string(),
                )
            })
            .collect::<Result<Vec<_>, String>>()?
    } else {
        Vec::new()
    };
    let operator = std::env::var("QX_OPERATOR").unwrap_or_else(|_| "local".to_string());
    let (has_report_analysis, has_strategy_diagnostics, assembly) =
        qx_app::artifact_store::prepare_run_evidence_assembly(
            qx_app::artifact_store::RunEvidencePreparation {
                summary_path,
                summary,
                report_analysis,
                strategy_diagnostics,
                components,
                build_profile: build_identity::BUILD_PROFILE,
                target_triple: build_identity::TARGET_TRIPLE,
                operator: &operator,
            },
        )?;
    qx_app::artifact_store::persist_run_evidence_bundle(
        summary_path,
        has_report_analysis,
        has_strategy_diagnostics,
        assembly,
    )
}
