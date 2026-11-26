//! `ValidateDataset` 用例（T2-2，方案 §5 阶段 2）。
//!
//! 它回答一个问题：**这一档输入能不能支撑一个 Bar 回测？** 回答必须可复核——所以输出里
//! 有内容指纹（复核同一份数据）、有实际读到的根数、有"不适用"的理由清单。
//!
//! ## 为什么"不可用"不是错误
//!
//! 数据不足**不是**用例失败。调用方要的是"这份数据行不行"这个判断，而不是一条异常；
//! 把它做成 `Err` 会逼调用方用异常控制流表达一个正常的研究结论。所以
//! `usable=false` + 非空 `gaps` 是**成功返回**。
//!
//! 反过来，"数据在盘但读不出来"（文件不在、JSON 坏了、时间戳乱序）才是错误——那时没有
//! 任何判断可给。于是两个类别的分工是：
//!
//! - 读不到 / 解析不了 → `Err(DataUnavailable | InvalidInput)`
//! - 读到了但不够用 → `Ok(DatasetVerdict { usable: false, gaps })`
//!
//! ## `gaps` 只装**这一层真能看见**的理由
//!
//! 时间戳单调性是 [`BarFrame::from_json`] 自己就把住的（`FrameError::NotMonotonic`），
//! 所以走到这里的数据一定已经严格递增——本层再写一遍检查是**永远不触发**的死代码，
//! 而一段永不触发的检查比没有检查更坏：它让读者以为"乱序会在这里被挡下"，
//! 从而不会去问"到底谁在挡"。所以本层只报它真能判的那一条（样本量），
//! 单调性由 `qx-datastruct` 单源负责，乱序的输入在 `BarFrame::from_json` 处就是 `InvalidInput`。

use crate::cases::artifacts::read_text;
use crate::cases::{attach, guard::guard_panics};
use crate::context::{CallerCapability, RunContext};
use crate::error::{AppError, AppErrorCategory};
use crate::spec::{DatasetSpec, DatasetVerdict, MIN_BACKTEST_BARS};
use qx_datastruct::BarFrame;

/// 校验一份数据集是否足以支撑 Bar 回测。
///
/// 权限：需要 [`CallerCapability::Research`]（R 档，无外部副作用）。
/// 幂等：纯读，无副作用，可任意重复调用。
/// 取消：同步且无阻塞等待，没有取消点。
pub fn validate_dataset(
    spec: &DatasetSpec,
    context: &RunContext,
) -> Result<DatasetVerdict, AppError> {
    context.require(CallerCapability::Research, "数据集校验")?;
    let correlation_id = context.correlation_id().to_string();
    let outcome = guard_panics(&correlation_id, || validate_dataset_inner(spec));
    outcome.map_err(|error| attach(error, &correlation_id))
}

pub(super) struct DatasetInspection {
    pub verdict: DatasetVerdict,
    pub first_ts: Option<u64>,
    pub last_ts: Option<u64>,
    pub instrument: qx_core::InstrumentId,
}

pub(super) fn inspect_dataset(spec: &DatasetSpec) -> Result<DatasetInspection, AppError> {
    spec.validate()?;
    let payload = read_text(&spec.bars_path)?;
    let frame = BarFrame::from_json(&payload).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BarFrame 校验失败 {}: {error:?}", spec.bars_path),
        )
    })?;
    let fingerprint = qx_data::fingerprint_bar_frame(&frame).map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BarFrame 内容指纹计算失败：{error}"),
        )
    })?;
    inspection_from_validated_frame(&spec.dataset_id, &frame, &fingerprint)
}

/// Build the same readiness facts from the exact frame and manifest already verified by the
/// canonical dataset identity path. Do not recompute the manifest fingerprint here: it covers
/// the same immutable Bar rows and was produced during that verification.
pub(super) fn inspect_bar_frame(
    dataset_id: &str,
    frame: &BarFrame,
    manifest: &qx_data::DatasetManifest,
) -> Result<DatasetInspection, AppError> {
    frame.validate().map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BarFrame 校验失败: {error:?}"),
        )
    })?;
    manifest.validate().map_err(|error| {
        AppError::new(
            AppErrorCategory::InvalidInput,
            format!("BarFrame 数据集清单无效: {error}"),
        )
    })?;
    let instrument = frame.instrument.to_string();
    let first_ts = frame.ts.first().copied();
    let last_ts = frame.ts.last().copied();
    if manifest.dataset_id != format!("strategy-bars:{instrument}")
        || manifest.start_timestamp != first_ts.unwrap_or_default()
        || manifest.end_timestamp != last_ts.unwrap_or_default()
    {
        return Err(AppError::new(
            AppErrorCategory::InvalidInput,
            "BarFrame 数据集清单与已验证输入不匹配",
        ));
    }
    inspection_from_validated_frame(dataset_id, frame, &manifest.fingerprint)
}

fn inspection_from_validated_frame(
    dataset_id: &str,
    frame: &BarFrame,
    fingerprint: &str,
) -> Result<DatasetInspection, AppError> {
    let rows = frame.len();
    let content_fingerprint = format!("barframe:{fingerprint}");
    let mut gaps: Vec<String> = Vec::new();
    if rows < MIN_BACKTEST_BARS {
        gaps.push(format!(
            "样本过短：读到 {rows} 根 Bar，Bar 回测下限 {MIN_BACKTEST_BARS} 根——指标在这么短的样本上不是指标而是噪声"
        ));
    }
    Ok(DatasetInspection {
        verdict: DatasetVerdict {
            dataset_id: dataset_id.to_owned(),
            instrument: frame.instrument.to_string(),
            rows: rows as u64,
            content_fingerprint,
            usable: gaps.is_empty(),
            gaps,
        },
        first_ts: frame.ts.first().copied(),
        last_ts: frame.ts.last().copied(),
        instrument: frame.instrument.clone(),
    })
}

fn validate_dataset_inner(spec: &DatasetSpec) -> Result<DatasetVerdict, AppError> {
    inspect_dataset(spec).map(|inspection| inspection.verdict)
}
