//! CCXT OHLCV acquisition followed by the shared builtin Bar backtest.

use crate::{
    python_interpreter, read_runtime_config, run_builtin_backtest_via_app,
    unified_default_runtime_path, write_backtest_artifact,
};
use qx_adapter::{CcxtProcessClient, CcxtRpc};
use std::path::{Path, PathBuf};

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
    if timeframe.ends_with('M') {
        return Err(
            "CCXT 月线 timeframe 没有固定 interval_ms，当前 DatasetManifestV2 缺口复核不支持月线；请使用固定长度周期"
                .into(),
        );
    }
    let interval_ms = crate::timeframe_to_ms(timeframe)?;
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
    let manifest = qx_data::manifest_from_bar_frame(
        &normalized,
        &format!("strategy-bars:{}", normalized.instrument),
        qx_data::BARFRAME_DATASET_VERSION,
        interval_ms,
        "UTC",
        qx_data::BARFRAME_DATASET_VERSION,
        &query_source,
    )?;
    let manifest_payload = manifest.to_json()?;
    let cache_key = qx_data::sha256_hex_bytes(manifest_payload.as_bytes());
    let runtime_path = runtime_config_path
        .map(Path::to_path_buf)
        .unwrap_or_else(unified_default_runtime_path);
    let runtime = read_runtime_config(&runtime_path)?;
    let data_root = PathBuf::from(&runtime.storage.data_dir);
    let data_root = if data_root.is_absolute() {
        data_root
    } else {
        std::env::current_dir()
            .map_err(|error| format!("读取当前目录失败: {error}"))?
            .join(data_root)
    };
    let dataset_root = data_root.join("datasets").join("ccxt").join(cache_key);
    std::fs::create_dir_all(&dataset_root)
        .map_err(|error| format!("创建 CCXT 不可变数据快照目录失败: {error}"))?;
    let frame_path = dataset_root.join("bars.json");
    let manifest_path = dataset_root.join("manifest.v2.json");
    write_backtest_artifact(&frame_path, &normalized.to_json(), "CCXT BarFrame 快照")?;
    write_backtest_artifact(&manifest_path, &manifest_payload, "CCXT DatasetManifestV2")?;
    run_builtin_backtest_via_app(
        strategy_name,
        &frame_path,
        spec_path,
        quantity,
        runtime_config_path,
        Some(&manifest_path),
    )
}
