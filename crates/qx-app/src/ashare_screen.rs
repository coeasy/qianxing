//! Rust-owned deterministic screening metrics for normalized A-share BarFrames.
//!
//! Provider-specific normalization stays at the Python edge. Return, drawdown,
//! volume aggregation and candidate ordering are shared application behavior.

use crate::error::{AppError, AppErrorCategory};
use qx_datastruct::BarFrame;
use serde::{Deserialize, Serialize};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenInput {
    frames: Vec<ScreenFrameInput>,
    min_return_bps: i128,
    min_avg_volume_raw: i128,
    limit: Option<usize>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct ScreenFrameInput {
    bars_json: String,
    #[serde(default)]
    bars_path: Option<String>,
}

#[derive(Serialize)]
struct ScreenCandidate {
    instrument: String,
    source: String,
    bars: usize,
    start_ts: u64,
    end_ts: u64,
    total_return_bps: i128,
    max_drawdown_bps: i128,
    average_volume_raw: i128,
    source_hash: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    bars_path: Option<String>,
    #[serde(skip)]
    input_order: usize,
}

/// Screen normalized BarFrame documents using the same deterministic Rust logic
/// regardless of which SDK supplied them. The Python adapter only reads files and
/// carries their paths through the result for optional job-manifest generation.
pub fn screen_bar_frames(payload: &str) -> Result<String, AppError> {
    let input: ScreenInput = serde_json::from_str(payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("A 股筛选请求 JSON 无效: {error}"),
        )
    })?;
    if input.limit == Some(0) {
        return Err(invalid_input("limit 必须大于 0"));
    }

    let mut candidates = Vec::with_capacity(input.frames.len());
    for (input_order, item) in input.frames.into_iter().enumerate() {
        let frame = BarFrame::from_json(&item.bars_json)
            .map_err(|error| invalid_input(format!("BarFrame 校验失败: {error:?}")))?;
        let first = *frame
            .close_raw
            .first()
            .expect("validated BarFrame is non-empty");
        if first <= 0 || frame.close_raw.iter().any(|close| *close <= 0) {
            return Err(invalid_input(format!(
                "{} 的收盘价必须始终大于 0",
                frame.instrument
            )));
        }
        if frame.volume_raw.iter().any(|volume| *volume < 0) {
            return Err(invalid_input(format!(
                "{} 的成交量不能为负数",
                frame.instrument
            )));
        }

        let last = *frame
            .close_raw
            .last()
            .expect("validated BarFrame is non-empty");
        let return_numerator = last
            .checked_sub(first)
            .and_then(|value| value.checked_mul(10_000))
            .ok_or_else(|| invalid_input("区间收益计算溢出"))?;
        // Python's integer // is floor division; div_euclid preserves that wire
        // contract for negative returns instead of Rust's truncation toward zero.
        let total_return_bps = return_numerator.div_euclid(first);

        let volume_sum = frame
            .volume_raw
            .iter()
            .try_fold(0_i128, |sum, volume| sum.checked_add(*volume))
            .ok_or_else(|| invalid_input("平均成交量计算溢出"))?;
        let average_volume_raw = volume_sum / frame.volume_raw.len() as i128;

        let mut peak = first;
        let mut max_drawdown_bps = 0_i128;
        for close in &frame.close_raw {
            peak = peak.max(*close);
            let drawdown = close
                .checked_sub(peak)
                .and_then(|value| value.checked_mul(10_000))
                .ok_or_else(|| invalid_input("最大回撤计算溢出"))?
                .div_euclid(peak);
            max_drawdown_bps = max_drawdown_bps.min(drawdown);
        }

        if total_return_bps < input.min_return_bps || average_volume_raw < input.min_avg_volume_raw
        {
            continue;
        }
        candidates.push(ScreenCandidate {
            instrument: frame.instrument.to_string(),
            source: frame.source.0.clone(),
            bars: frame.len(),
            start_ts: frame.ts[0],
            end_ts: *frame.ts.last().expect("validated BarFrame is non-empty"),
            total_return_bps,
            max_drawdown_bps,
            average_volume_raw,
            source_hash: format!("{:016x}", frame.digest()),
            bars_path: item.bars_path,
            input_order,
        });
    }

    candidates.sort_by(|left, right| {
        right
            .total_return_bps
            .cmp(&left.total_return_bps)
            .then_with(|| right.average_volume_raw.cmp(&left.average_volume_raw))
            .then_with(|| left.input_order.cmp(&right.input_order))
    });
    if let Some(limit) = input.limit {
        candidates.truncate(limit);
    }
    serde_json::to_string(&candidates).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("A 股筛选结果无法序列化: {error}"),
        )
    })
}

fn invalid_input(message: impl std::fmt::Display) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message.to_string())
}

#[cfg(test)]
mod tests {
    use super::screen_bar_frames;
    use serde_json::{json, Value};

    fn request(closes: &[i128], volumes: &[i128], instrument: &str) -> Value {
        json!({
            "frames": [{
                "bars_json": json!({
                    "schema_version": 1,
                    "instrument": instrument,
                    "source": "fixture",
                    "ts": (1..=closes.len() as u64).collect::<Vec<_>>(),
                    "open_raw": closes,
                    "high_raw": closes,
                    "low_raw": closes,
                    "close_raw": closes,
                    "volume_raw": volumes,
                }).to_string(),
                "bars_path": "fixture.json"
            }],
            "min_return_bps": -10_000,
            "min_avg_volume_raw": 0,
            "limit": null
        })
    }

    #[test]
    fn negative_return_uses_floor_and_paths_survive_projection() {
        let payload = request(&[3_000, 2_999], &[1, 2], "000001.SZSE").to_string();
        let result: Value = serde_json::from_str(&screen_bar_frames(&payload).unwrap()).unwrap();
        assert_eq!(result[0]["total_return_bps"], -4);
        assert_eq!(result[0]["max_drawdown_bps"], -4);
        assert_eq!(result[0]["average_volume_raw"], 1);
        assert_eq!(result[0]["bars_path"], "fixture.json");
    }

    #[test]
    fn zero_close_and_zero_limit_fail_closed() {
        let zero_close = request(&[0, 1], &[1, 2], "000001.SZSE").to_string();
        assert!(screen_bar_frames(&zero_close)
            .unwrap_err()
            .message()
            .contains("收盘价"));
        let mut zero_limit = request(&[1, 2], &[1, 2], "000001.SZSE");
        zero_limit["limit"] = json!(0);
        assert!(screen_bar_frames(&zero_limit.to_string()).is_err());
    }
}
