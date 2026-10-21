//! CCXT OHLCV acquisition followed by the shared builtin Bar backtest.

use super::BuiltinStrategyKind;
use crate::{
    python_interpreter, read_runtime_config, run_builtin_backtest_with_runtime_snapshot,
    unified_default_runtime_path,
};
use qx_adapter::{CcxtProcessClient, CcxtRpc};
use std::path::Path;

pub(crate) struct CcxtBuiltinBacktestInput<'a> {
    pub(crate) ccxt_config_path: &'a Path,
    pub(crate) strategy_name: &'a str,
    pub(crate) instrument: &'a str,
    pub(crate) timeframe: &'a str,
    pub(crate) start_ms: u64,
    pub(crate) end_ms: u64,
    pub(crate) spec_path: Option<&'a Path>,
    pub(crate) quantity: i64,
    pub(crate) runtime_config_path: Option<&'a Path>,
}

pub(crate) fn run_ccxt_builtin_backtest(input: CcxtBuiltinBacktestInput<'_>) -> Result<(), String> {
    let CcxtBuiltinBacktestInput {
        ccxt_config_path,
        strategy_name,
        instrument,
        timeframe,
        start_ms,
        end_ms,
        spec_path,
        quantity,
        runtime_config_path,
    } = input;
    if end_ms < start_ms {
        return Err("CCXT 内置策略回测 end_ms 不能早于 start_ms".into());
    }
    if quantity <= 0 {
        return Err("内置策略 quantity 必须为正整数".into());
    }
    let kind = BuiltinStrategyKind::parse(strategy_name)?;
    if timeframe.ends_with('M') {
        return Err(
            "CCXT 月线 timeframe 没有固定 interval_ms，当前 DatasetManifestV2 缺口复核不支持月线；请使用固定长度周期"
                .into(),
        );
    }
    let interval_ms = crate::timeframe_to_ms(timeframe)?;
    // Decode and freeze runtime settings before starting the external CCXT worker. A malformed
    // runtime must fail closed without network/process side effects, and data/run identity use the
    // same immutable configuration snapshot even if an operator edits the source file meanwhile.
    let runtime_path = runtime_config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(unified_default_runtime_path);
    let runtime = read_runtime_config(&runtime_path)?;
    let python = python_interpreter();
    let mut client = CcxtProcessClient::spawn(&python, &ccxt_config_path.to_string_lossy(), None)
        .map_err(|error| format!("启动公共 CCXT Worker 失败: {error}"))?;
    let result = client
        .call(serde_json::json!({
            "op": "fetch_ohlcv",
            "instrument": instrument,
            "timeframe": timeframe,
            "start_ms": start_ms,
            "end_ms": end_ms,
        }))
        .map_err(|error| format!("CCXT OHLCV 查询失败: {error}"))?;
    let frame = result
        .get("frame")
        .ok_or_else(|| "CCXT OHLCV 响应缺少 frame".to_string())?;
    let payload = serde_json::to_string(frame)
        .map_err(|error| format!("编码 CCXT BarFrame 失败: {error}"))?;
    let normalized = qx_datastruct::BarFrame::from_json(&payload)
        .map_err(|error| format!("CCXT BarFrame 校验失败: {error:?}"))?;
    if normalized.instrument.to_string() != instrument {
        return Err(format!(
            "CCXT OHLCV 返回 instrument 与请求不一致: requested={instrument} actual={}",
            normalized.instrument
        ));
    }
    if normalized.ts.first().is_some_and(|first| *first < start_ms)
        || normalized.ts.last().is_some_and(|last| *last > end_ms)
    {
        return Err(format!(
            "CCXT OHLCV 返回 Bar 超出请求窗口: requested={start_ms}..{end_ms} actual={}..{}",
            normalized.ts.first().copied().unwrap_or_default(),
            normalized.ts.last().copied().unwrap_or_default(),
        ));
    }
    if normalized
        .ts
        .windows(2)
        .any(|pair| pair[1] - pair[0] < interval_ms)
    {
        return Err(format!(
            "CCXT OHLCV timeframe 与返回 Bar 间隔不符: timeframe={timeframe} interval_ms={interval_ms}"
        ));
    }
    let query_source = format!(
        "{};requested_range={start_ms}..{end_ms}",
        normalized.source.0
    );
    // 复用 BarFrame 唯一的数据集身份版本；供应源、交易对、周期和请求窗口写入清单 lineage，
    // 精确内容 hash 与 canonical fingerprint 则区分交易所后续修订的历史数据。
    let snapshot = qx_app::ccxt_backtest_snapshot::persist_ccxt_backtest_snapshot(
        &normalized,
        interval_ms,
        &query_source,
        Path::new(&runtime.storage.data_dir),
    )?;
    run_builtin_backtest_with_runtime_snapshot(
        &snapshot.bars_path,
        spec_path,
        quantity,
        Some(&snapshot.manifest_path),
        &runtime_path,
        kind,
        runtime,
    )
}
