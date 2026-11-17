//! Rebuild and run bounded Rust Bar strategy bias diagnostics from a verified run.

use qx_app::BacktestSpec;
use qx_core::{Fnv1a, InstrumentId, RunManifest};
use qx_datastruct::BarFrame;
use qx_strategy::diagnostics::{
    diagnose_future_perturbations, diagnose_startup_trajectory, diagnose_startup_windows,
};
use qx_strategy::{BuiltinStrategy, MarketEvent, Strategy, StrategyContext};
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

const MAX_DIAGNOSTIC_BARS: usize = 65_536;

pub(crate) fn generate_builtin_bar_diagnostics(
    summary_path: &Path,
    spec_path: &Path,
    output_path: &Path,
    write_html: bool,
) -> Result<(), String> {
    generate_builtin_bar_diagnostics_with_readout(
        summary_path,
        spec_path,
        output_path,
        write_html,
        true,
    )
}

pub(crate) fn generate_builtin_bar_diagnostics_quiet(
    summary_path: &Path,
    spec_path: &Path,
    output_path: &Path,
) -> Result<(), String> {
    generate_builtin_bar_diagnostics_with_readout(
        summary_path,
        spec_path,
        output_path,
        false,
        false,
    )
}

fn generate_builtin_bar_diagnostics_with_readout(
    summary_path: &Path,
    spec_path: &Path,
    output_path: &Path,
    write_html: bool,
    print_readout: bool,
) -> Result<(), String> {
    let summary_bytes = std::fs::read(summary_path)
        .map_err(|error| format!("读取回测摘要失败 {}: {error}", summary_path.display()))?;
    let summary: Value = serde_json::from_slice(&summary_bytes)
        .map_err(|error| format!("回测摘要 JSON 无效 {}: {error}", summary_path.display()))?;
    if summary.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err("strategy diagnose-bar 当前只接受 qx-app Bar 回测 schema v1 摘要".into());
    }
    let spec_bytes = std::fs::read(spec_path)
        .map_err(|error| format!("读取回测规格失败 {}: {error}", spec_path.display()))?;
    let spec_text =
        std::str::from_utf8(&spec_bytes).map_err(|error| format!("回测规格不是 UTF-8: {error}"))?;
    let spec = BacktestSpec::from_json(spec_text).map_err(|error| error.to_string())?;
    spec.validate().map_err(|error| error.to_string())?;

    if spec.portfolio.is_some() || spec.strategy.kind.needs_reference_leg() {
        return Err("自动 Rust Bar 诊断当前要求单策略、单标的且无需参考腿的内置策略".into());
    }
    if spec.strategy.target_notional_raw.is_some()
        || spec.strategy.target_available_margin_bps.is_some()
        || spec.strategy.target_volatility_bps.is_some()
        || spec.strategy.target_kelly_fraction_bps.is_some()
    {
        return Err("动态 sizing 依赖逐时点账户状态；当前诊断不重建持仓/购买力，已拒绝生成".into());
    }

    let run_id = text(&summary, "/run_id")?;
    let instrument_text = text(&summary, "/instrument")?;
    let strategy_id = text(&summary, "/strategy_id")?;
    let summary_fingerprint = text(&summary, "/data_fingerprint")?;
    if run_id != spec.run_id
        || instrument_text != spec.instrument
        || strategy_id != spec.strategy.strategy_id
    {
        return Err("回测摘要与提供的 BacktestSpec 身份不一致".into());
    }

    let canonical_spec = spec.to_json().map_err(|error| error.to_string())?;
    let mut config_hasher = Fnv1a::new();
    config_hasher.write_text(&canonical_spec);
    let config_hash = format!("{:016x}", config_hasher.finish());
    if text(&summary, "/config_hash")? != config_hash {
        return Err("回测摘要 config_hash 与 BacktestSpec 不匹配".into());
    }

    let manifest_path = PathBuf::from(text(&summary, "/run_manifest")?);
    let manifest_bytes = std::fs::read(&manifest_path)
        .map_err(|error| format!("读取 RunManifest 失败 {}: {error}", manifest_path.display()))?;
    let manifest_text = std::str::from_utf8(&manifest_bytes)
        .map_err(|error| format!("RunManifest 不是 UTF-8: {error}"))?;
    let manifest = RunManifest::from_json(manifest_text)?;
    if manifest.run_id != run_id
        || manifest.config_hash != config_hash
        || manifest.data_fingerprint != summary_fingerprint
        || manifest.result_hash != text(&summary, "/result_hash")?
        || manifest.clock_start != number(&summary, "/clock_start")?
        || manifest.clock_end != number(&summary, "/clock_end")?
    {
        return Err("回测摘要与 RunManifest 的运行身份不一致".into());
    }

    let frame_path = PathBuf::from(&spec.bars_path);
    let frame_bytes = std::fs::read(&frame_path)
        .map_err(|error| format!("读取 BarFrame 失败 {}: {error}", frame_path.display()))?;
    let frame_text = std::str::from_utf8(&frame_bytes)
        .map_err(|error| format!("BarFrame 不是 UTF-8: {error}"))?;
    let frame =
        BarFrame::from_json(frame_text).map_err(|error| format!("BarFrame 无效: {error:?}"))?;
    let expected_instrument = InstrumentId::parse(&spec.instrument)
        .ok_or_else(|| format!("BacktestSpec.instrument 无效: {}", spec.instrument))?;
    if frame.instrument != expected_instrument {
        return Err("BarFrame instrument 与 BacktestSpec 不匹配".into());
    }
    if frame.len() > MAX_DIAGNOSTIC_BARS {
        return Err(format!(
            "Rust 策略诊断最多接受 {MAX_DIAGNOSTIC_BARS} 根 Bar；当前 {} 根，请使用更小的可复核区间",
            frame.len()
        ));
    }
    let content_fingerprint = qx_data::fingerprint_bar_frame(&frame)?;
    let composed_fingerprint = format!("barframe:{content_fingerprint}");
    if summary_fingerprint != composed_fingerprint
        || number(&summary, "/bars")? != frame.len() as u64
        || number(&summary, "/clock_start")? != *frame.ts.first().ok_or("BarFrame 为空")?
        || number(&summary, "/clock_end")? != *frame.ts.last().ok_or("BarFrame 为空")?
    {
        return Err("当前 BarFrame 与已保存运行的内容指纹、行数或时间范围不一致".into());
    }
    let output_paths = crate::report_output_paths::report_artifact_paths(summary_path)?;
    let equity = crate::report_html::read_equity_analysis_csv(&output_paths.equity_csv)?;
    let fills = crate::report_html::read_fills_csv(&output_paths.fills_csv)?;
    if equity.len() as u64 != number(&summary, "/equity_points")?
        || equity.last().map(|point| point.1) != Some(number_i128(&summary, "/final_equity_raw")?)
        || fills.len() as u64 != number(&summary, "/fills")?
    {
        return Err("权益/成交产物的行数或末值与回测摘要不一致".into());
    }

    let protected = [summary_path, spec_path, &frame_path, &manifest_path];
    if protected
        .iter()
        .any(|source| same_path(source, output_path))
    {
        return Err("诊断输出不能覆盖回测摘要、BacktestSpec、BarFrame 或 RunManifest".into());
    }
    if write_html
        && same_path(
            &crate::report_output_paths::report_artifact_paths(summary_path)?.html,
            output_path,
        )
    {
        return Err("诊断 JSON 输出不能覆盖同次生成的 HTML 报告".into());
    }

    let mut events = Vec::with_capacity(frame.len());
    for index in 0..frame.len() {
        events.push(MarketEvent::Bar {
            instrument: frame.instrument.clone(),
            ts: frame.ts[index],
            open_raw: frame.open_raw[index],
            high_raw: frame.high_raw[index],
            low_raw: frame.low_raw[index],
            close_raw: frame.close_raw[index],
            volume_raw: frame.volume_raw[index],
            exact_ohlcv: frame.exact_ohlcv.as_ref().map(|values| values[index]),
        });
    }
    if events.len() < 2 {
        return Err("策略未来数据扰动诊断至少需要两根 Bar".into());
    }

    let account_id = spec.account_id.as_deref().unwrap_or("app");
    let strategy_version = builtin_strategy_version(&spec.strategy);
    let context = StrategyContext {
        strategy_id: strategy_id.to_string(),
        strategy_version: strategy_version.clone(),
        account_id: account_id.to_string(),
        venue_id: expected_instrument.venue.to_string(),
        // Diagnostic identity is the declared dataset-content fingerprint used by report and
        // EvidenceBundle attachment validators. The RunManifest's composed namespace prefix is
        // checked above but is not repeated in this attachment field.
        data_fingerprint: content_fingerprint.clone(),
        as_of: frame.ts[0],
        positions_exact: std::collections::BTreeMap::new(),
        cash_exact: std::collections::BTreeMap::new(),
        available_margin_exact: None,
        positions: BTreeMap::new(),
        cash: BTreeMap::from([(spec.settlement_currency.clone(), spec.initial_cash_raw)]),
        available_margin_raw: None,
        risk_state: "backtest_diagnostic_fixed_context".into(),
    };
    let runtime_strategy = qx_runtime::StrategyRuntimeConfig {
        id: Some(strategy_id.to_string()),
        version: strategy_version,
        target_qty: spec.strategy.quantity_raw,
        builtin_strategy: Some(spec.strategy.kind.name().to_string()),
        builtin_fast_window: Some(spec.strategy.fast_window),
        builtin_slow_window: Some(spec.strategy.slow_window),
        builtin_period: Some(spec.strategy.period),
        builtin_threshold_bps: Some(spec.strategy.threshold_bps),
        ..qx_runtime::StrategyRuntimeConfig::default()
    };
    let strategy_config =
        qx_runtime::builtin_strategy_config_from_runtime(&runtime_strategy, &expected_instrument)
            .map_err(|error| format!("重建内置策略失败: {error}"))?;

    let anchor = spec
        .strategy
        .fast_window
        .max(spec.strategy.slow_window)
        .max(spec.strategy.period)
        .clamp(20, 250);
    let startup_windows = [anchor, anchor * 2, anchor * 4];
    let make_strategy = || -> Result<Box<dyn Strategy>, String> {
        Ok(Box::new(BuiltinStrategy::new(strategy_config.clone())?))
    };
    let startup = diagnose_startup_windows(&context, &events, &startup_windows, make_strategy)?;
    let trajectory =
        diagnose_startup_trajectory(&context, &events, &startup_windows, 96, make_strategy)?;
    let lookahead = diagnose_future_perturbations(&context, &events, 16, make_strategy)?;
    let windows = startup
        .windows
        .iter()
        .map(|window| {
            json!({
                "requested_events": window.requested_bars,
                "available_events": window.available_bars,
                "history_truncated": window.history_truncated,
                "decision": window.decision,
                "changed_decision_fields": window.changed_fields,
                "intermediate_state": window.intermediate_state,
                "changed_intermediate_fields": window.changed_intermediate_fields,
            })
        })
        .collect::<Vec<_>>();
    let diagnostics = json!({
        "schema_version": 1,
        "kind": "qianxing-strategy-bias-diagnostics",
        "strategy": "rust_builtin",
        "worker_kind": "rust_builtin_replay",
        "run_id": run_id,
        "strategy_id": strategy_id,
        "strategy_version": strategy_config.strategy_version,
        "instrument": instrument_text,
        "data_fingerprint": content_fingerprint,
        "config_hash": config_hash,
        "input_kind": "bar",
        "input_bars": events.len(),
        "as_of": events.last().map(MarketEvent::ts),
        "visible_bars_at_as_of": events.len(),
        "lookahead": lookahead,
        "startup": {
            "status": startup.status,
            "reference_window_events": startup.reference_window_bars,
            "decision_timestamp": startup.decision_timestamp,
            "windows": windows,
            "trajectory": trajectory,
            "interpretation": startup.interpretation,
            "context_assumption": "Each replay uses the run's fixed initial cash snapshot and empty positions. Portfolio and dynamic sizing strategies are rejected; event-time account state is not reconstructed."
        },
        "data_visibility": {
            "status": "visibility_unknown",
            "reason": "the run artifact does not contain a verified provider PIT revision ledger with effective_at and available_at facts"
        }
    });
    if write_html {
        crate::report_command::write_app_backtest_diagnostic_html(
            summary_path,
            &summary,
            &spec,
            &diagnostics,
        )?;
    }
    let encoded = serde_json::to_string_pretty(&diagnostics)
        .map_err(|error| format!("序列化策略诊断失败: {error}"))?;
    if let Some(parent) = output_path
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
    {
        std::fs::create_dir_all(parent)
            .map_err(|error| format!("创建诊断输出目录失败: {error}"))?;
    }
    std::fs::write(output_path, format!("{encoded}\n"))
        .map_err(|error| format!("写入策略诊断失败 {}: {error}", output_path.display()))?;
    if print_readout {
        println!(
            "[Strategy Diagnostics] run_id={} strategy={} bars={} future_targets_checked={} pit=visibility_unknown output={}",
            text(&diagnostics, "/run_id")?,
            text(&diagnostics, "/strategy_id")?,
            events.len(),
            diagnostics.pointer("/lookahead/perturbation_targets_checked").and_then(Value::as_u64).unwrap_or(0),
            output_path.display()
        );
    }
    Ok(())
}

fn text<'a>(value: &'a Value, pointer: &str) -> Result<&'a str, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .ok_or_else(|| format!("回测摘要缺少有效字段 {pointer}"))
}

fn number(value: &Value, pointer: &str) -> Result<u64, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_u64)
        .ok_or_else(|| format!("回测摘要缺少有效字段 {pointer}"))
}

fn number_i128(value: &Value, pointer: &str) -> Result<i128, String> {
    value
        .pointer(pointer)
        .and_then(Value::as_i64)
        .map(i128::from)
        .or_else(|| {
            value
                .pointer(pointer)
                .and_then(Value::as_u64)
                .map(i128::from)
        })
        .ok_or_else(|| format!("回测摘要缺少有效字段 {pointer}"))
}

fn same_path(left: &Path, right: &Path) -> bool {
    let left = left.canonicalize().unwrap_or_else(|_| left.to_path_buf());
    let right = right.canonicalize().unwrap_or_else(|_| right.to_path_buf());
    left == right
}

pub(crate) fn builtin_strategy_version(spec: &qx_app::BuiltinStrategySpec) -> String {
    if spec.target_kelly_fraction_bps.is_some() {
        format!("builtin-{}-kelly-v1", spec.kind.name())
    } else if spec.target_volatility_bps.is_some() {
        format!("builtin-{}-atr-budget-v1", spec.kind.name())
    } else if spec.target_available_margin_bps.is_some() {
        format!("builtin-{}-cash-fraction-v1", spec.kind.name())
    } else if spec.target_notional_raw.is_some() {
        format!("builtin-{}-notional-v1", spec.kind.name())
    } else {
        format!("builtin-{}-v1", spec.kind.name())
    }
}
