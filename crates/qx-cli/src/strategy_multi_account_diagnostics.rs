//! Generate account-bound strategy bias diagnostics for an isolated Bar batch.

use qx_app::{MultiAccountBacktestResult, MultiAccountBacktestSpec};
use qx_core::Fnv1a;
use qx_datastruct::BarFrame;
use serde_json::{json, Value};
use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

const MAX_BATCH_DIAGNOSTIC_BARS: usize = 262_144;
const MAX_DIAGNOSTIC_BYTES: usize = 1024 * 1024;
static STAGING_SEQUENCE: AtomicU64 = AtomicU64::new(0);

struct StagingDirectory(PathBuf);

impl Drop for StagingDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

pub(crate) fn generate_multi_account_builtin_bar_diagnostics(
    result_path: &Path,
    spec_path: &Path,
    output_path: &Path,
) -> Result<(), String> {
    validate_output_path(output_path)?;
    let result_bytes = std::fs::read(result_path)
        .map_err(|error| format!("读取多账户结果失败 {}: {error}", result_path.display()))?;
    let result: MultiAccountBacktestResult = serde_json::from_slice(&result_bytes)
        .map_err(|error| format!("多账户回测结果 JSON 无效: {error}"))?;
    let spec_bytes = std::fs::read(spec_path)
        .map_err(|error| format!("读取多账户规格失败 {}: {error}", spec_path.display()))?;
    let spec_text = std::str::from_utf8(&spec_bytes)
        .map_err(|error| format!("多账户规格不是 UTF-8: {error}"))?;
    let spec = MultiAccountBacktestSpec::from_json(spec_text).map_err(|error| error.to_string())?;
    validate_batch_identity(&result, &spec)?;
    validate_output_identity(output_path, result_path, spec_path, &spec, &result)?;

    let output_parent = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    std::fs::create_dir_all(output_parent)
        .map_err(|error| format!("创建诊断输出目录失败 {}: {error}", output_parent.display()))?;
    let staging = reserve_staging_directory(output_parent)?;

    let members = spec
        .runs
        .iter()
        .map(|member| (member.account_id.as_deref().unwrap_or_default(), member))
        .collect::<BTreeMap<_, _>>();
    let mut total_bars = 0_usize;
    let mut accounts = Vec::new();
    for (index, run) in result.runs.iter().enumerate() {
        let Some(outcome) = &run.outcome else {
            continue;
        };
        let member = members
            .get(run.account_id.as_str())
            .ok_or_else(|| format!("成功账户 {} 不在输入规格中", run.account_id))?;
        if outcome.account_id.as_deref() != Some(run.account_id.as_str())
            || outcome.run_id != member.run_id
            || outcome.instrument != member.instrument
        {
            return Err(format!(
                "账户 {} 的 outcome 与输入 BacktestSpec 身份不一致",
                run.account_id
            ));
        }
        let frame_bytes = std::fs::read(&member.bars_path)
            .map_err(|error| format!("读取账户 {} 的 BarFrame 失败: {error}", run.account_id))?;
        let frame_text = std::str::from_utf8(&frame_bytes)
            .map_err(|error| format!("账户 {} 的 BarFrame 不是 UTF-8: {error}", run.account_id))?;
        let frame = BarFrame::from_json(frame_text)
            .map_err(|error| format!("账户 {} 的 BarFrame 无效: {error:?}", run.account_id))?;
        total_bars = total_bars
            .checked_add(frame.len())
            .ok_or("多账户诊断 Bar 数量溢出")?;
        if total_bars > MAX_BATCH_DIAGNOSTIC_BARS {
            return Err(format!(
                "多账户策略诊断最多接受 {MAX_BATCH_DIAGNOSTIC_BARS} 根成功账户 Bar；当前至少 {total_bars} 根"
            ));
        }

        let member_path = staging.0.join(format!("account-{index}.spec.json"));
        std::fs::write(
            &member_path,
            member.to_json().map_err(|error| error.to_string())?,
        )
        .map_err(|error| format!("写入账户诊断规格失败: {error}"))?;
        let diagnostic_path = staging.0.join(format!("account-{index}.diagnostics.json"));
        crate::strategy_diagnostics::generate_builtin_bar_diagnostics_quiet(
            Path::new(&outcome.artifacts.summary),
            &member_path,
            &diagnostic_path,
        )?;
        let diagnostic: Value = serde_json::from_slice(
            &std::fs::read(&diagnostic_path)
                .map_err(|error| format!("读取账户 {} 的诊断结果失败: {error}", run.account_id))?,
        )
        .map_err(|error| format!("账户 {} 的诊断结果 JSON 无效: {error}", run.account_id))?;
        accounts.push(json!({
            "account_id": &run.account_id,
            "report": diagnostic,
        }));
    }
    if accounts.is_empty() {
        return Err("多账户结果没有成功账户，无法生成策略诊断".into());
    }
    let diagnostics = json!({
        "schema_version": 1,
        "kind": "qianxing-multi-account-strategy-bias-diagnostics",
        "accounts": accounts,
    });
    let encoded = serde_json::to_vec_pretty(&diagnostics)
        .map_err(|error| format!("序列化多账户诊断失败: {error}"))?;
    if encoded.len() > MAX_DIAGNOSTIC_BYTES {
        return Err(format!(
            "多账户策略诊断超过 {} MiB 上限: {} bytes",
            MAX_DIAGNOSTIC_BYTES / (1024 * 1024),
            encoded.len()
        ));
    }
    std::fs::write(output_path, &encoded)
        .map_err(|error| format!("写入多账户诊断失败 {}: {error}", output_path.display()))?;
    println!(
        "[Strategy Diagnostics] batch_id={} accounts={} bars={} output={}",
        result.batch_id,
        accounts_len(&diagnostics),
        total_bars,
        output_path.display()
    );
    Ok(())
}

pub(crate) fn validate_batch_identity(
    result: &MultiAccountBacktestResult,
    spec: &MultiAccountBacktestSpec,
) -> Result<(), String> {
    if result.batch_id != spec.batch_id
        || result.settlement_currency
            != spec
                .runs
                .first()
                .map(|run| run.settlement_currency.as_str())
                .unwrap_or_default()
    {
        return Err("多账户结果与规格的 batch_id/settlement_currency 不一致".into());
    }
    let expected = spec
        .runs
        .iter()
        .map(|run| run.account_id.as_deref().unwrap_or_default())
        .collect::<BTreeSet<_>>();
    let actual = result
        .runs
        .iter()
        .map(|run| run.account_id.as_str())
        .collect::<BTreeSet<_>>();
    let succeeded = result
        .runs
        .iter()
        .filter(|run| run.outcome.is_some())
        .count();
    if expected != actual
        || result.runs.len() != spec.runs.len()
        || result.summary.accounts_total != result.runs.len()
        || result.summary.accounts_succeeded != succeeded
        || result.summary.accounts_failed != result.runs.len() - succeeded
        || result.summary.complete != (succeeded == result.runs.len())
    {
        return Err("多账户结果的账户集合或汇总计数与规格/逐账户状态不一致".into());
    }
    let mut canonical = spec.clone();
    canonical
        .runs
        .sort_by(|left, right| left.account_id.cmp(&right.account_id));
    for run in &mut canonical.runs {
        run.bars_path.clear();
        run.output_dir.clear();
    }
    let canonical = canonical.to_json().map_err(|error| error.to_string())?;
    let mut hasher = Fnv1a::new();
    hasher.write_text(&canonical);
    if result.spec_fingerprint != format!("{:016x}", hasher.finish()) {
        return Err("多账户结果 spec_fingerprint 与输入规格不一致".into());
    }
    Ok(())
}

fn validate_output_path(output: &Path) -> Result<(), String> {
    if !output
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| extension.eq_ignore_ascii_case("json"))
    {
        return Err(format!(
            "多账户诊断输出必须以 .json 结尾: {}",
            output.display()
        ));
    }
    Ok(())
}

fn validate_output_identity(
    output: &Path,
    result_path: &Path,
    spec_path: &Path,
    spec: &MultiAccountBacktestSpec,
    result: &MultiAccountBacktestResult,
) -> Result<(), String> {
    for protected in [result_path, spec_path] {
        if crate::report_output_paths::same_output_target(output, protected) {
            return Err(format!("诊断输出不能覆盖输入文件: {}", protected.display()));
        }
    }
    for member in &spec.runs {
        if crate::report_output_paths::same_output_target(output, Path::new(&member.bars_path)) {
            return Err(format!(
                "诊断输出不能覆盖账户 {} 的行情输入",
                member.account_id.as_deref().unwrap_or("unknown")
            ));
        }
    }
    for run in &result.runs {
        if let Some(outcome) = &run.outcome {
            for artifact in [
                &outcome.artifacts.run_manifest,
                &outcome.artifacts.summary,
                &outcome.artifacts.equity,
                &outcome.artifacts.fills,
            ]
            .into_iter()
            .chain(outcome.artifacts.event_trace.iter())
            {
                if crate::report_output_paths::same_output_target(output, Path::new(artifact)) {
                    return Err(format!("诊断输出不能覆盖账户运行工件: {artifact}"));
                }
            }
        }
    }
    Ok(())
}

fn reserve_staging_directory(parent: &Path) -> Result<StagingDirectory, String> {
    for _ in 0..100 {
        let sequence = STAGING_SEQUENCE.fetch_add(1, Ordering::Relaxed);
        let candidate = parent.join(format!(
            ".qianxing-multi-account-diagnostics-{}-{sequence}",
            std::process::id()
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => return Ok(StagingDirectory(candidate)),
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(format!("创建多账户诊断暂存目录失败: {error}")),
        }
    }
    Err("无法分配唯一的多账户诊断暂存目录".into())
}

fn accounts_len(diagnostics: &Value) -> usize {
    diagnostics
        .get("accounts")
        .and_then(Value::as_array)
        .map_or(0, Vec::len)
}
