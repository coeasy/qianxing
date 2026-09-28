//! 实时策略的"这一根 Bar 是否已经处理过"状态：BarFrame 指纹与它的落盘去重记录。
//!
//! 与 `workers.rs` 的关系：worker 循环负责调度与下单，本模块只回答两个问题——
//! 当前可消费的闭合 Bar 指纹是多少、上一轮处理过的指纹是多少。两者都是纯读写文件，
//! 不碰队列、账本或 Venue，因此可以独立于 worker 测试。
//! （V12 R1 从 `workers.rs` 原样搬家，行为逐字相同。）

use super::*;

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
                ))
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
