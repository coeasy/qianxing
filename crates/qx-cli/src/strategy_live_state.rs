//! 实时策略触发身份：BarFrame、L1 Quote 或 Timer 桶指纹及其落盘去重记录。
//!
//! 与 `workers.rs` 的关系：worker 循环负责调度与下单，本模块只回答两个问题——
//! 当前可消费的闭合 Bar 指纹是多少、上一轮处理过的指纹是多少。两者都是纯读写文件，
//! 不碰队列、账本或 Venue，因此可以独立于 worker 测试。
//! （V12 R1 从 `workers.rs` 原样搬家，行为逐字相同。）

use crate::{live_frame_is_fresh, timeframe_to_ms};
use qx_core::InstrumentId;
use qx_datastruct::BarFrame;
use qx_runtime::{OutboxRecovery, RuntimeConfig, StrategyRuntimeConfig, WorkerRole};
use std::path::{Path, PathBuf};

/// 取当前实时策略可用的闭合 Bar 指纹。
///
/// 返回 `Ok(None)` 表示"这一轮不该跑"（未启用、快照还没到、Bar 未闭合或已过时），
/// 与 `Err`（配置/数据本身不合法）严格区分：前者静默跳过本轮，后者必须让 worker 退出。
pub(crate) fn live_strategy_snapshot_digest(
    strategy: &StrategyRuntimeConfig,
    now: u64,
) -> Result<Option<u64>, String> {
    if !strategy.live_enabled {
        return Ok(None);
    }
    let timeframe_ms = timeframe_to_ms(&strategy.live_timeframe)?;
    let max_staleness_ms = strategy
        .live_max_staleness_ms
        .unwrap_or_else(|| timeframe_ms.saturating_mul(3).max(timeframe_ms));
    let Some(path) = strategy.bars_snapshot_path.as_deref() else {
        return Err("实时策略缺少 bars_snapshot_path".into());
    };
    if !Path::new(path).exists() {
        return Ok(None);
    }
    let payload = std::fs::read_to_string(path)
        .map_err(|error| format!("读取实时策略 BarFrame 失败 {}: {error}", path))?;
    let frame = BarFrame::from_json(&payload)
        .map_err(|error| format!("实时策略 BarFrame 无效 {}: {error:?}", path))?;
    if !live_frame_is_fresh(
        &frame,
        timeframe_ms,
        strategy.live_closed_only,
        max_staleness_ms,
        now,
    ) {
        return Ok(None);
    }
    if strategy
        .instrument
        .as_deref()
        .and_then(InstrumentId::parse)
        .is_some_and(|instrument| instrument != frame.instrument)
    {
        return Err(format!(
            "实时策略 BarFrame instrument 不一致: strategy={} frame={}",
            strategy.instrument.as_deref().unwrap_or(""),
            frame.instrument
        ));
    }
    let mut digest = frame.digest();
    if let Some(reference_text) = strategy.builtin_reference_instrument.as_deref() {
        let reference_path = strategy
            .builtin_reference_bars_snapshot_path
            .as_deref()
            .ok_or_else(|| "双腿实时策略缺少对冲腿 BarFrame".to_string())?;
        let reference_payload = match std::fs::read_to_string(reference_path) {
            Ok(payload) => payload,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
            Err(error) => {
                return Err(format!(
                    "读取实时策略对冲腿 BarFrame 失败 {}: {error}",
                    reference_path
                ));
            }
        };
        let reference_frame = BarFrame::from_json(&reference_payload).map_err(|error| {
            format!("实时策略对冲腿 BarFrame 无效 {}: {error:?}", reference_path)
        })?;
        if !live_frame_is_fresh(
            &reference_frame,
            timeframe_ms,
            strategy.live_closed_only,
            max_staleness_ms,
            now,
        ) {
            return Ok(None);
        }
        let reference_instrument = InstrumentId::parse(reference_text)
            .ok_or_else(|| format!("实时策略对冲腿 instrument 非法: {reference_text}"))?;
        if reference_frame.instrument != reference_instrument {
            return Err(format!(
                "实时策略对冲腿 instrument 不一致: expected={} frame={}",
                reference_instrument, reference_frame.instrument
            ));
        }
        if reference_frame.ts.last() != frame.ts.last() {
            // 双腿快照必须处于同一闭合 Bar；市场 worker 的两次原子写入之间
            // 允许策略 worker 短暂跳过本轮，下一轮会重新观察。
            return Ok(None);
        }
        digest ^= reference_frame.digest().rotate_left(1);
    }
    Ok(Some(digest))
}

/// Runtime Tick/OrderBook strategies use the latest durable market EventLog fact as
/// their scheduler trigger. Timer strategies use a deterministic scheduler bucket.
pub(crate) fn live_strategy_input_digest(
    config: &RuntimeConfig,
    root: &Path,
    strategy: &StrategyRuntimeConfig,
    now: u64,
) -> Result<Option<u64>, String> {
    let data_level = strategy
        .python_strategy_spec
        .as_ref()
        .and_then(|spec| spec.get("data_level"))
        .and_then(serde_json::Value::as_str)
        .unwrap_or("bar");
    if data_level == "timer" {
        return live_strategy_timer_digest(strategy, now);
    }
    if !matches!(data_level, "tick" | "orderbook") {
        return live_strategy_snapshot_digest(strategy, now);
    }
    if !strategy.live_enabled {
        return Ok(None);
    }
    let instrument_text = strategy
        .instrument
        .as_deref()
        .ok_or_else(|| "实时行情事件策略缺少 instrument".to_string())?;
    let instrument = InstrumentId::parse(instrument_text)
        .ok_or_else(|| format!("实时行情事件策略 instrument 非法: {instrument_text}"))?;
    let timeframe_ms = timeframe_to_ms(&strategy.live_timeframe)?;
    let max_staleness_ms = strategy
        .live_max_staleness_ms
        .unwrap_or_else(|| timeframe_ms.saturating_mul(3).max(timeframe_ms));
    let sources = live_market_event_logs(config, root, &instrument)?;
    let mut latest: Option<(u64, String, u64, serde_json::Value)> = None;
    for (log_name, currency) in sources {
        let pipeline = qx_service::pipeline::open_runtime_pipeline(
            config,
            root,
            log_name.clone(),
            currency,
            OutboxRecovery::ReadOnly,
        )
        .map_err(|error| format!("读取实时市场事件策略行情 EventLog 失败 {log_name}: {error}"))?;
        for event in pipeline.log().events() {
            let payload = match (&event.kind, data_level) {
                (
                    qx_core::EventKind::MarketQuote {
                        instrument: quote_instrument,
                        bid,
                        ask,
                        bid_qty,
                        ask_qty,
                        bid_exact,
                        ask_exact,
                        bid_qty_exact,
                        ask_qty_exact,
                    },
                    "tick",
                ) if quote_instrument == &instrument && event.ts <= now => {
                    serde_json::json!({
                        "stream_id": log_name,
                        "event_seq": event.seq,
                        "source_seq": event.source_seq,
                        "ts": event.ts,
                        "instrument": quote_instrument.to_string(),
                        "bid_raw": bid.raw(),
                        "ask_raw": ask.raw(),
                        "bid_qty_raw": bid_qty.raw(),
                        "ask_qty_raw": ask_qty.raw(),
                        "bid_exact": bid_exact.map(|value| value.to_string()),
                        "ask_exact": ask_exact.map(|value| value.to_string()),
                        "bid_qty_exact": bid_qty_exact.map(|value| value.to_string()),
                        "ask_qty_exact": ask_qty_exact.map(|value| value.to_string()),
                    })
                }
                (
                    qx_core::EventKind::MarketOrderBook {
                        instrument: book_instrument,
                        sequence,
                        bids,
                        asks,
                    },
                    "orderbook",
                ) if book_instrument == &instrument && event.ts <= now => {
                    let encode_side = |levels: &[qx_core::MarketDepthLevel]| {
                        levels
                            .iter()
                            .map(|level| {
                                serde_json::json!({
                                    "price_raw": level.price.raw(),
                                    "qty_raw": level.quantity.raw(),
                                    "price_exact": level.price_exact.map(|value| value.to_string()),
                                    "qty_exact": level.quantity_exact.map(|value| value.to_string()),
                                })
                            })
                            .collect::<Vec<_>>()
                    };
                    serde_json::json!({
                        "stream_id": log_name,
                        "event_seq": event.seq,
                        "source_seq": event.source_seq,
                        "ts": event.ts,
                        "instrument": book_instrument.to_string(),
                        "sequence": sequence,
                        "bids": encode_side(bids),
                        "asks": encode_side(asks),
                    })
                }
                _ => continue,
            };
            let candidate = (event.ts, log_name.clone(), event.seq, payload);
            if latest.as_ref().is_none_or(|current| {
                (&candidate.0, &candidate.1, &candidate.2) > (&current.0, &current.1, &current.2)
            }) {
                latest = Some(candidate);
            }
        }
    }
    let Some((ts, _stream, _seq, payload)) = latest else {
        return Ok(None);
    };
    if now.saturating_sub(ts) > max_staleness_ms {
        return Ok(None);
    }
    let encoded = serde_json::to_vec(&payload)
        .map_err(|error| format!("编码实时市场事件策略指纹失败: {error}"))?;
    let digest = qx_strategy::sha256_hex(&encoded);
    u64::from_str_radix(&digest[..16], 16)
        .map(Some)
        .map_err(|error| format!("解析实时市场事件策略指纹失败: {error}"))
}

fn live_strategy_timer_digest(
    strategy: &StrategyRuntimeConfig,
    now: u64,
) -> Result<Option<u64>, String> {
    if !strategy.live_enabled {
        return Ok(None);
    }
    let timeframe_ms = timeframe_to_ms(&strategy.live_timeframe)?;
    let time_bucket = now / timeframe_ms;
    let payload = serde_json::to_vec(&serde_json::json!({
        "schema_version": 1,
        "kind": "runtime_strategy_timer",
        "strategy_id": strategy.id,
        "strategy_version": strategy.version,
        "instrument": strategy.instrument,
        "python_module": strategy.python_module,
        "strategy_artifact_sha256": strategy.strategy_artifact_sha256,
        "python_strategy_spec": strategy.python_strategy_spec,
        "timeframe_ms": timeframe_ms,
        "time_bucket": time_bucket,
    }))
    .map_err(|error| format!("编码 Runtime Timer 指纹失败: {error}"))?;
    let digest = qx_strategy::sha256_hex(&payload);
    u64::from_str_radix(&digest[..16], 16)
        .map(Some)
        .map_err(|error| format!("解析 Runtime Timer 指纹失败: {error}"))
}

pub(crate) fn live_market_event_logs(
    config: &RuntimeConfig,
    root: &Path,
    instrument: &InstrumentId,
) -> Result<Vec<(String, String)>, String> {
    let mut sources = std::collections::BTreeMap::new();
    for worker in config.workers.iter().filter(|worker| {
        worker.enabled
            && worker.role == WorkerRole::MarketData
            && worker
                .symbols
                .iter()
                .any(|symbol| InstrumentId::parse(symbol).as_ref() == Some(instrument))
    }) {
        let currency = qx_runtime::worker_settlement_currency(worker);
        for log_name in [
            qx_service::market_bridges::worker_scoped_event_log_name("", &worker.id),
            qx_service::market_bridges::ccxt_market_event_log_name(worker),
        ] {
            if crate::event_log_exists(config, root, &log_name)? {
                sources.insert(log_name, currency.clone());
            }
        }
    }
    if sources.len() > 1 {
        return Err(format!(
            "Runtime market-data instrument {instrument} 同时命中多个行情 EventLog；请明确只保留一个 MarketData 写入源"
        ));
    }
    Ok(sources.into_iter().collect())
}

pub(crate) fn live_strategy_digest_state_path(root: &Path, worker_id: &str) -> PathBuf {
    root.join("strategy-live-state")
        .join(format!("{worker_id}.digest"))
}

pub(crate) fn load_live_strategy_digest(path: &Path) -> Result<Option<u64>, String> {
    match std::fs::read_to_string(path) {
        Ok(value) => value
            .trim()
            .parse::<u64>()
            .map(Some)
            .map_err(|error| format!("实时策略 digest 状态非法 {}: {error}", path.display())),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(error) => Err(format!(
            "读取实时策略 digest 状态失败 {}: {error}",
            path.display()
        )),
    }
}

pub(crate) fn save_live_strategy_digest(path: &Path, digest: u64) -> Result<(), String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("实时策略 digest 路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建实时策略 digest 目录失败: {error}"))?;
    let temporary = path.with_extension(format!("digest.tmp.{}", std::process::id()));
    std::fs::write(&temporary, digest.to_string())
        .map_err(|error| format!("写入实时策略 digest 失败: {error}"))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temporary, path)
            .map_err(|replacement| format!("提交实时策略 digest 失败: {error}; {replacement}"))?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn timer_trigger_digest_is_stable_inside_bucket_and_changes_at_boundary() {
        let strategy = StrategyRuntimeConfig {
            id: Some("scheduled".into()),
            version: "2.0".into(),
            live_enabled: true,
            live_timeframe: "1m".into(),
            python_module: Some("strategies.scheduled".into()),
            python_strategy_spec: Some(serde_json::json!({"data_level": "timer"})),
            ..StrategyRuntimeConfig::default()
        };

        let at_start = live_strategy_timer_digest(&strategy, 60_000).unwrap();
        let within_bucket = live_strategy_timer_digest(&strategy, 119_999).unwrap();
        let next_bucket = live_strategy_timer_digest(&strategy, 120_000).unwrap();

        assert_eq!(at_start, within_bucket);
        assert_ne!(at_start, next_bucket);
        assert_eq!(
            live_strategy_timer_digest(&strategy, 60_000).unwrap(),
            at_start
        );
    }

    #[test]
    fn timer_trigger_digest_is_disabled_with_live_strategy() {
        let strategy = StrategyRuntimeConfig {
            live_enabled: false,
            ..StrategyRuntimeConfig::default()
        };
        assert_eq!(live_strategy_timer_digest(&strategy, 60_000).unwrap(), None);
    }
}
