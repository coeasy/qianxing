use crate::*;

#[derive(Clone)]
pub(crate) struct LiveStrategyBarSpec {
    pub(crate) instrument: InstrumentId,
    pub(crate) timeframe: String,
    pub(crate) timeframe_ms: u64,
    pub(crate) max_staleness_ms: u64,
    pub(crate) history_limit: usize,
    pub(crate) closed_only: bool,
    pub(crate) snapshot_path: PathBuf,
}

pub(crate) fn timeframe_to_ms(timeframe: &str) -> Result<u64, String> {
    let value = timeframe.trim().to_ascii_lowercase();
    if value.len() < 2 {
        return Err(format!("CCXT timeframe 非法: {timeframe}"));
    }
    let (number, unit) = value.split_at(value.len() - 1);
    let number = number
        .parse::<u64>()
        .map_err(|_| format!("CCXT timeframe 数值非法: {timeframe}"))?;
    if number == 0 {
        return Err(format!("CCXT timeframe 必须为正数: {timeframe}"));
    }
    let unit_ms = match unit {
        "s" => 1_000,
        "m" => 60_000,
        "h" => 3_600_000,
        "d" => 86_400_000,
        "w" => 604_800_000,
        _ => return Err(format!("不支持的 CCXT timeframe 单位: {timeframe}")),
    };
    number
        .checked_mul(unit_ms)
        .ok_or_else(|| format!("CCXT timeframe 溢出: {timeframe}"))
}

pub(crate) fn live_strategy_bar_specs(
    config: &RuntimeConfig,
    runtime_config_path: &Path,
    worker: &WorkerConfig,
) -> Result<Vec<LiveStrategyBarSpec>, String> {
    let strategies = if config.strategies.is_empty() {
        vec![config.strategy.clone()]
    } else {
        config.strategies.clone()
    };
    let mut specs = Vec::new();
    for strategy in strategies {
        if !strategy.live_enabled {
            continue;
        }
        let instrument_text = strategy
            .instrument
            .as_deref()
            .ok_or_else(|| "实时策略必须配置 instrument".to_string())?;
        let instrument = InstrumentId::parse(instrument_text)
            .ok_or_else(|| format!("实时策略 instrument 非法: {instrument_text}"))?;
        let timeframe_ms = timeframe_to_ms(&strategy.live_timeframe)?;
        let max_staleness_ms = strategy
            .live_max_staleness_ms
            .unwrap_or_else(|| timeframe_ms.saturating_mul(3).max(timeframe_ms));
        let primary_matches = worker.symbols.iter().any(|symbol| {
            InstrumentId::parse(symbol).is_some_and(|configured| configured == instrument)
        });
        if primary_matches {
            let configured_path = strategy
                .bars_snapshot_path
                .as_deref()
                .ok_or_else(|| "实时策略必须配置 bars_snapshot_path".to_string())?;
            specs.push(LiveStrategyBarSpec {
                instrument,
                timeframe: strategy.live_timeframe.clone(),
                timeframe_ms,
                max_staleness_ms,
                history_limit: strategy.live_history_limit,
                closed_only: strategy.live_closed_only,
                snapshot_path: resolve_runtime_relative_path(runtime_config_path, configured_path),
            });
        }
        if let (Some(reference_text), Some(reference_path)) = (
            strategy.builtin_reference_instrument.as_deref(),
            strategy.builtin_reference_bars_snapshot_path.as_deref(),
        ) {
            let reference = InstrumentId::parse(reference_text)
                .ok_or_else(|| format!("实时策略对冲腿 instrument 非法: {reference_text}"))?;
            if worker.symbols.iter().any(|symbol| {
                InstrumentId::parse(symbol).is_some_and(|configured| configured == reference)
            }) {
                specs.push(LiveStrategyBarSpec {
                    instrument: reference,
                    timeframe: strategy.live_timeframe.clone(),
                    timeframe_ms,
                    max_staleness_ms,
                    history_limit: strategy.live_history_limit,
                    closed_only: strategy.live_closed_only,
                    snapshot_path: resolve_runtime_relative_path(
                        runtime_config_path,
                        reference_path,
                    ),
                });
            }
        }
    }
    specs.sort_by(|left, right| {
        left.instrument
            .to_string()
            .cmp(&right.instrument.to_string())
            .then(left.snapshot_path.cmp(&right.snapshot_path))
    });
    specs.dedup_by(|left, right| {
        left.instrument == right.instrument && left.snapshot_path == right.snapshot_path
    });
    Ok(specs)
}

pub(crate) fn live_frame_is_fresh(
    frame: &BarFrame,
    timeframe_ms: u64,
    closed_only: bool,
    max_staleness_ms: u64,
    now: u64,
) -> bool {
    let Some(&last_ts) = frame.ts.last() else {
        return false;
    };
    let freshness_ts = if closed_only {
        let Some(close_ts) = last_ts.checked_add(timeframe_ms) else {
            return false;
        };
        if close_ts > now {
            return false;
        }
        close_ts
    } else {
        if last_ts > now {
            return false;
        }
        last_ts
    };
    now.saturating_sub(freshness_ts) <= max_staleness_ms
}

pub(crate) fn closed_live_frame(
    frame: BarFrame,
    spec: &LiveStrategyBarSpec,
    now: u64,
) -> Result<Option<BarFrame>, String> {
    if frame.instrument != spec.instrument {
        return Err(format!(
            "实时 OHLCV instrument 不一致: expected={} actual={}",
            spec.instrument, frame.instrument
        ));
    }
    let visible = frame
        .ts
        .iter()
        .enumerate()
        .filter(|(_, ts)| {
            !spec.closed_only
                || ts
                    .checked_add(spec.timeframe_ms)
                    .is_some_and(|close_ts| close_ts <= now)
        })
        .collect::<Vec<_>>();
    if visible.is_empty() {
        return Ok(None);
    }
    let start = visible.len().saturating_sub(spec.history_limit);
    let visible = &visible[start..];
    let result = BarFrame {
        instrument: frame.instrument,
        source: frame.source,
        ts: visible.iter().map(|(_, ts)| **ts).collect(),
        open_raw: visible
            .iter()
            .map(|(index, _)| frame.open_raw[*index])
            .collect(),
        high_raw: visible
            .iter()
            .map(|(index, _)| frame.high_raw[*index])
            .collect(),
        low_raw: visible
            .iter()
            .map(|(index, _)| frame.low_raw[*index])
            .collect(),
        close_raw: visible
            .iter()
            .map(|(index, _)| frame.close_raw[*index])
            .collect(),
        volume_raw: visible
            .iter()
            .map(|(index, _)| frame.volume_raw[*index])
            .collect(),
    };
    result
        .validate()
        .map_err(|error| format!("实时 BarFrame 校验失败: {error:?}"))?;
    if !live_frame_is_fresh(
        &result,
        spec.timeframe_ms,
        spec.closed_only,
        spec.max_staleness_ms,
        now,
    ) {
        return Ok(None);
    }
    Ok(Some(result))
}

pub(crate) fn write_live_bar_snapshot(path: &Path, frame: &BarFrame) -> Result<u64, String> {
    let parent = path
        .parent()
        .ok_or_else(|| format!("实时 BarFrame 路径没有父目录: {}", path.display()))?;
    std::fs::create_dir_all(parent)
        .map_err(|error| format!("创建实时 BarFrame 目录失败 {}: {error}", parent.display()))?;
    let payload = frame.to_json();
    let temporary = path.with_extension(format!("barframe.tmp.{}", std::process::id()));
    std::fs::write(&temporary, payload)
        .map_err(|error| format!("写入实时 BarFrame 临时文件失败: {error}"))?;
    if let Err(error) = std::fs::rename(&temporary, path) {
        // Windows 无法直接覆盖已有文件；只在替换失败时删除旧快照，
        // Strategy worker 遇到短暂缺文件会等待下一轮，不会读取半份 JSON。
        let _ = std::fs::remove_file(path);
        std::fs::rename(&temporary, path).map_err(|replacement| {
            format!(
                "提交实时 BarFrame 失败 {}: initial={error}; replacement={replacement}",
                path.display()
            )
        })?;
    }
    Ok(frame.digest())
}
