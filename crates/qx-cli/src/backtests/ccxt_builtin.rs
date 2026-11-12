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
    pub(crate) use_public_default_fees: bool,
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
        use_public_default_fees,
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
    let mut runtime = read_runtime_config(&runtime_path)?;
    if use_public_default_fees && runtime.strategy.cost_rules_path.is_some() {
        return Err(
            "--use-public-default-fees 不能与 runtime.strategy.cost_rules_path 同时使用；请选择一个明确成本来源"
                .into(),
        );
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
    let mut normalized = qx_datastruct::BarFrame::from_json(&payload)
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
    let provider_source = normalized
        .source
        .0
        .strip_prefix("ccxt:")
        .unwrap_or(&normalized.source.0);
    let query_source = format!("ccxt:{provider_source};requested_range={start_ms}..{end_ms}");
    // The frozen BarFrame is later reopened by the shared identity verifier. Persist the
    // provider and requested window in its own source field as well as the quality manifest,
    // so its registry version remains content-addressed even when a worker returns a fixture
    // or adapter-specific source label.
    normalized.source = qx_guanxing::DataSourceId::new(query_source.clone());
    // 复用 BarFrame 唯一的数据集身份版本；供应源、交易对、周期和请求窗口写入清单 lineage，
    // 精确内容 hash 与 canonical fingerprint 则区分交易所后续修订的历史数据。
    let snapshot = qx_app::ccxt_backtest_snapshot::persist_ccxt_backtest_snapshot(
        &normalized,
        interval_ms,
        &query_source,
        Path::new(&runtime.storage.data_dir),
    )?;
    let effective_spec_path = if use_public_default_fees && spec_path.is_none() {
        let path = generated_market_spec_path(&runtime, instrument)?;
        crate::run_ccxt_market_spec_with_client(&mut client, instrument, &path)?;
        Some(path)
    } else {
        spec_path.map(Path::to_path_buf)
    };
    if use_public_default_fees {
        let market_spec_path = effective_spec_path
            .as_deref()
            .ok_or_else(|| "公共默认费率模式缺少 market spec".to_string())?;
        let market_spec: serde_json::Value =
            serde_json::from_slice(&std::fs::read(market_spec_path).map_err(|error| {
                format!(
                    "读取 market spec {} 失败: {error}",
                    market_spec_path.display()
                )
            })?)
            .map_err(|error| format!("market spec JSON 非法: {error}"))?;
        let market_instrument = market_spec_instrument(&market_spec);
        if market_instrument.as_deref() != Some(instrument) {
            return Err(format!(
                "fee market spec 标的与回测请求不一致: requested={instrument} actual={}",
                market_instrument.as_deref().unwrap_or("<missing>")
            ));
        }
        let captured_at_unix_ms = market_spec
            .pointer("/fee_snapshot/captured_at_unix_ms")
            .and_then(serde_json::Value::as_u64)
            .ok_or_else(|| "market spec 缺少 fee_snapshot.captured_at_unix_ms".to_string())?;
        let cost_rules = public_default_cost_rules(&market_spec)?;
        let cost_path = generated_cost_rules_path(&runtime, instrument, captured_at_unix_ms)?;
        let cost_payload = serde_json::to_vec_pretty(&cost_rules)
            .map_err(|error| format!("编码冻结费率规则失败: {error}"))?;
        qx_app::artifact_store::sink::publish_bytes(
            &cost_path,
            &cost_payload,
            qx_app::artifact_store::sink::PublishMode::Idempotent,
        )
        .map_err(|error| format!("发布冻结费率规则 {} 失败: {error}", cost_path.display()))?;
        runtime.strategy.cost_rules_path = Some(cost_path.to_string_lossy().into_owned());
        println!(
            "[CCXT · Fees] source=public-market-default account_specific=false maker_bp={} taker_bp={} snapshot={}",
            cost_rules["maker_bp"],
            cost_rules["taker_bp"],
            cost_path.display()
        );
    }
    run_builtin_backtest_with_runtime_snapshot(
        &snapshot.bars_path,
        effective_spec_path.as_deref(),
        quantity,
        Some(&snapshot.manifest_path),
        &runtime_path,
        kind,
        runtime,
    )
}

fn generated_market_spec_path(
    runtime: &qx_runtime::RuntimeConfig,
    instrument: &str,
) -> Result<std::path::PathBuf, String> {
    let stamp = unix_timestamp_ms()?;
    let instrument_digest = qx_strategy::sha256_hex(instrument.as_bytes());
    Ok(ccxt_market_metadata_dir(runtime)?
        .join(format!("{}-{stamp}.json", &instrument_digest[..16])))
}

fn generated_cost_rules_path(
    runtime: &qx_runtime::RuntimeConfig,
    instrument: &str,
    captured_at_unix_ms: u64,
) -> Result<std::path::PathBuf, String> {
    let instrument_digest = qx_strategy::sha256_hex(instrument.as_bytes());
    Ok(ccxt_market_metadata_dir(runtime)?.join(format!(
        "{}-{captured_at_unix_ms}.costs.json",
        &instrument_digest[..16]
    )))
}

fn ccxt_market_metadata_dir(
    runtime: &qx_runtime::RuntimeConfig,
) -> Result<std::path::PathBuf, String> {
    let directory = research_data_root(runtime)?.join("ccxt-market-specs");
    std::fs::create_dir_all(&directory).map_err(|error| {
        format!(
            "创建 CCXT 市场快照目录 {} 失败: {error}",
            directory.display()
        )
    })?;
    directory.canonicalize().map_err(|error| {
        format!(
            "解析 CCXT 市场快照目录 {} 失败: {error}",
            directory.display()
        )
    })
}

fn research_data_root(runtime: &qx_runtime::RuntimeConfig) -> Result<std::path::PathBuf, String> {
    let configured = Path::new(&runtime.storage.data_dir);
    let root = if configured.is_absolute() {
        configured.to_path_buf()
    } else {
        std::env::current_dir()
            .map_err(|error| format!("读取当前目录失败: {error}"))?
            .join(configured)
    };
    std::fs::create_dir_all(&root)
        .map_err(|error| format!("创建研究数据目录 {} 失败: {error}", root.display()))?;
    root.canonicalize()
        .map_err(|error| format!("解析研究数据目录 {} 失败: {error}", root.display()))
}

fn unix_timestamp_ms() -> Result<u64, String> {
    let elapsed = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_err(|error| format!("读取系统 UTC 时间失败: {error}"))?;
    u64::try_from(elapsed.as_millis()).map_err(|_| "系统 UTC 毫秒时间戳溢出".into())
}

fn public_default_cost_rules(market_spec: &serde_json::Value) -> Result<serde_json::Value, String> {
    let fee_snapshot = market_spec
        .get("fee_snapshot")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| "market spec 缺少 fee_snapshot；不能估算交易费率".to_string())?;
    if fee_snapshot
        .get("source")
        .and_then(serde_json::Value::as_str)
        != Some("ccxt_market_metadata_default")
        || fee_snapshot
            .get("account_specific")
            .and_then(serde_json::Value::as_bool)
            != Some(false)
    {
        return Err("费率来源或范围不符合公开默认费率契约".into());
    }
    let maker_rate = fee_snapshot
        .get("maker_rate")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "CCXT 未提供可用的公开 maker 默认费率".to_string())?;
    let taker_rate = fee_snapshot
        .get("taker_rate")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| "CCXT 未提供可用的公开 taker 默认费率".to_string())?;
    let maker_bp = fee_rate_to_basis_points(maker_rate, "maker")?;
    let taker_bp = fee_rate_to_basis_points(taker_rate, "taker")?;
    Ok(serde_json::json!({
        "name": "ccxt-public-market-default",
        "maker_bp": maker_bp,
        "taker_bp": taker_bp,
        "latency_base_ns": 0,
        "latency_insert_ns": 0,
        "fee_snapshot": fee_snapshot,
    }))
}

fn market_spec_instrument(market_spec: &serde_json::Value) -> Option<String> {
    let instrument = market_spec.get("instrument")?;
    if let Some(text) = instrument.as_str() {
        return Some(text.to_string());
    }
    let symbol = instrument.get("symbol")?.as_str()?;
    let venue = instrument.get("venue")?.as_str()?;
    Some(format!("{symbol}.{venue}"))
}

fn fee_rate_to_basis_points(rate: &str, role: &str) -> Result<i64, String> {
    let decimal = qx_core::ExactDecimal::parse(rate)
        .map_err(|error| format!("CCXT {role} 费率非法: {error}"))?;
    let basis_points = decimal
        .to_raw(4)
        .map_err(|_| format!("CCXT {role} 费率 {rate} 不能无损映射到整数基点"))?;
    let basis_points = i64::try_from(basis_points)
        .map_err(|_| format!("CCXT {role} 费率 {rate} 超出整数基点范围"))?;
    if !(0..=10_000).contains(&basis_points) {
        return Err(format!(
            "CCXT {role} 费率 {rate} 换算后必须位于 0..=10000 bp"
        ));
    }
    Ok(basis_points)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn public_default_fee_rates_convert_only_when_exact_in_integer_basis_points() {
        assert_eq!(fee_rate_to_basis_points("0.001", "taker").unwrap(), 10);
        assert_eq!(fee_rate_to_basis_points("0.0008", "maker").unwrap(), 8);
        assert!(fee_rate_to_basis_points("0.00075", "maker")
            .unwrap_err()
            .contains("无损映射"));
        assert!(fee_rate_to_basis_points("-0.0001", "maker").is_err());
        assert!(fee_rate_to_basis_points("1.0001", "taker").is_err());
        assert_eq!(
            market_spec_instrument(&serde_json::json!({"instrument":"BTCUSDT.BINANCE"})),
            Some("BTCUSDT.BINANCE".into())
        );
        assert_eq!(
            market_spec_instrument(&serde_json::json!({
                "instrument":{"symbol":"BTCUSDT","venue":"BINANCE"}
            })),
            Some("BTCUSDT.BINANCE".into())
        );
    }

    #[test]
    fn public_default_cost_rules_require_both_public_rates_and_preserve_snapshot() {
        let market = serde_json::json!({
            "fee_snapshot": {
                "source": "ccxt_market_metadata_default",
                "captured_at_unix_ms": 1_800_000_000_000u64,
                "account_specific": false,
                "maker_rate": "0.0008",
                "taker_rate": "0.001"
            }
        });
        let rules = public_default_cost_rules(&market).unwrap();
        assert_eq!(rules["maker_bp"], 8);
        assert_eq!(rules["taker_bp"], 10);
        let parsed: qx_xingban::ExecutionCostRules = serde_json::from_value(rules.clone()).unwrap();
        assert_eq!(parsed.maker_bp, 8);
        assert_eq!(parsed.taker_bp, 10);
        assert_eq!(
            rules["fee_snapshot"]["captured_at_unix_ms"],
            1_800_000_000_000u64
        );
        let missing = serde_json::json!({
            "fee_snapshot": {
                "source": "ccxt_market_metadata_default",
                "account_specific": false,
                "maker_rate": null,
                "taker_rate": "0.001"
            }
        });
        assert!(public_default_cost_rules(&missing)
            .unwrap_err()
            .contains("maker"));
    }
}
