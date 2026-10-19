//! 回测产物落盘：清单、工件与 `BacktestArtifacts` 载体。

use super::{read_example_json, recompute_multi_barframe_input, verify_declared_run_record};
use crate::contract_adapters;
pub(crate) use qx_app::artifact_store::{
    persist_backtest_artifacts, BacktestArtifacts, BacktestInputProvenance,
};
use qx_data::JsonBarFrameProvider;
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_xingban::DepthFrame;
use std::path::{Path, PathBuf};

/// BarFrame 文件的**唯一**入口读点：`strategy backtest` 与 `qx report` 复核走同一个函数，
/// 所以"当时读成的形状"与"事后重读成的形状"不可能被写成两件事。
pub(crate) fn read_bar_frame_for_backtest(path: &Path) -> Result<BarFrame, String> {
    let payload = read_example_json(path, "策略回测 BarFrame ")?;
    BarFrame::from_json(&payload)
        .map_err(|error| format!("策略回测 BarFrame 校验失败 {}: {error:?}", path.display()))
}

/// BarFrame → 数据集身份的**唯一**读点：`strategy backtest` 用它登记与复核，`qx report` 用它把
/// 产物上声明的指纹重算一遍。两处各解一遍 JSON 时，"声明的输入"与"重算的输入"会读成两个答案。
pub(crate) fn barframe_dataset_identity(
    frame_path: &Path,
    frame: &BarFrame,
) -> Result<(Vec<Bar>, qx_data::DatasetManifest), String> {
    let bars: Vec<Bar> = frame.into();
    let provider =
        JsonBarFrameProvider::new(frame.source.0.clone(), BARFRAME_DATASET_VERSION, frame_path);
    let (provider_bars, manifest) = provider.load_bars_with_manifest(
        &format!("strategy-bars:{}", frame.instrument),
        &frame.instrument.to_string(),
        bars.first().map(|bar| bar.ts).unwrap_or(1),
        bars.last().map(|bar| bar.ts).unwrap_or(1),
    )?;
    if provider_bars.len() != bars.len()
        || !provider_bars.iter().zip(&bars).all(|(ingest, market)| {
            contract_adapters::ingest_bar_matches_market_bar(ingest, market)
        })
    {
        return Err("qx-data Provider 与 BarFrame 列式输入不一致，拒绝开始回测".into());
    }
    Ok((bars, manifest))
}

/// 深度帧的**唯一**读点，理由同 [`barframe_dataset_identity`]。
pub(crate) fn read_depth_frame_for_backtest(path: &Path) -> Result<DepthFrame, String> {
    let payload = read_example_json(path, "深度数据帧")?;
    DepthFrame::from_json(&payload).map_err(|error| format!("{}: {error}", path.display()))
}

/// 策略链那一档输入的身份：BarFrame 文件 + 注册表用过的指纹。
pub(crate) fn barframe_input_provenance(
    frame_path: &Path,
    manifest: &qx_data::DatasetManifest,
) -> BacktestInputProvenance {
    BacktestInputProvenance {
        kind: "barframe",
        path: frame_path.to_string_lossy().into_owned(),
        dataset_id: manifest.dataset_id.clone(),
        dataset_version: manifest.version.clone(),
        fingerprint: manifest.fingerprint.clone(),
    }
}

/// 深度链那一档输入的身份。它不进数据集注册表（那套 key 是 `strategy-bars:<instrument>`），
/// 所以 id 与 version 由这一档输入自己的形状命名。
pub(crate) fn depth_frame_input_provenance(
    frame_path: &Path,
    frame: &DepthFrame,
) -> BacktestInputProvenance {
    BacktestInputProvenance {
        kind: "depth-frame",
        path: frame_path.to_string_lossy().into_owned(),
        dataset_id: format!("depth-bars:{}", frame.instrument),
        dataset_version: DEPTH_FRAME_DATASET_VERSION.to_string(),
        fingerprint: format!("{:016x}", frame.input_hash()),
    }
}

/// `input` 块在摘要里的唯一写法。

/// 复核摘要旁边那本 `*.run.json`：摘要写下的 `run_manifest` 指针必须指得到一份读得开、
/// 且与摘要那两格哈希同值的运行清单（V12 R4-i）。
///
/// 这一步之前没有任何生产读者：清单写着同一轮的身份，报告却只念摘要的自述，于是产物被拆开
/// 换掉一半（摘要留着、清单换成别轮的）也照样出报告。指针缺失、文件读不到、哈希对不上
/// 都判失败——它们都不是"没声明"，而是"两份产物在互相打脸"。
pub(crate) use qx_app::artifact_store::verify_declared_run_manifest;

/// 复核一份回测摘要声明的输入：按它写的路径重读同一份文件、走**同一个读点**重算指纹，再逐字段
/// 比对。声明与实况不符就报错，这就是 Q1b 要的那条"真会失败"的检查。
///
/// 返回 `None` 只有一种情况：这份摘要根本没写 `input` 块（旧 schema，或本来就不落产物的入口）。
/// 调用方不得把它说成"已核对"。
pub(crate) fn recompute_declared_backtest_input(
    summary: &serde_json::Value,
) -> Result<Option<BacktestInputProvenance>, String> {
    let Some(declared) = summary.get("input") else {
        verify_declared_run_record(summary)?;
        return Ok(None);
    };
    if declared.get("kind").and_then(serde_json::Value::as_str) == Some("multi-barframe") {
        return recompute_multi_barframe_input(summary).map(Some);
    }
    let field = |name: &str| -> Result<String, String> {
        declared
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(|value| value.to_string())
            .ok_or_else(|| format!("回测摘要的 input 块缺 {name}"))
    };
    // 先把声明读全，再按声明的路径重读：一块连字段都不全的摘要，报"缺 X"比报"文件读不到"
    // 更贴近读者要修的那件事。
    let kind = match field("kind")?.as_str() {
        "barframe" => "barframe",
        "depth-frame" => "depth-frame",
        other => return Err(format!("未知的回测输入种类: {other}")),
    };
    let declared = BacktestInputProvenance {
        kind,
        path: field("path")?,
        dataset_id: field("dataset_id")?,
        dataset_version: field("dataset_version")?,
        fingerprint: field("fingerprint")?,
    };
    let path = PathBuf::from(&declared.path);
    let recomputed = match kind {
        "barframe" => {
            let frame = read_bar_frame_for_backtest(&path)?;
            let (_, manifest) = barframe_dataset_identity(&path, &frame)?;
            barframe_input_provenance(&path, &manifest)
        }
        _ => depth_frame_input_provenance(&path, &read_depth_frame_for_backtest(&path)?),
    };
    for (name, (declared_value, actual_value)) in [
        ("dataset_id", (&declared.dataset_id, &recomputed.dataset_id)),
        (
            "dataset_version",
            (&declared.dataset_version, &recomputed.dataset_version),
        ),
        (
            "fingerprint",
            (&declared.fingerprint, &recomputed.fingerprint),
        ),
    ] {
        if declared_value != actual_value {
            return Err(format!(
                "回测产物声明的输入与实况不符: {name} 声明={declared_value} 重算={actual_value}（输入文件 {}）",
                recomputed.path
            ));
        }
    }
    // 输入身份对上之后，依次核对同轮 RunManifest 与 RunRecord（v5）：报告出口不需要另立读法，
    // 输入、配置身份、已完成状态与全部产物摘要都在这一条复核路径上收口。
    verify_declared_run_manifest(summary)?;
    verify_declared_run_record(summary)?;
    Ok(Some(recomputed))
}

/// BarFrame 数据集的版本号：文件名后缀会一直跟着它，改它就是声明"换了另一种输入形状"。
pub(crate) const BARFRAME_DATASET_VERSION: &str = qx_data::BARFRAME_DATASET_VERSION;
/// 深度帧输入的版本号（它没有注册表条目，只有这一档形状）。
pub(crate) const DEPTH_FRAME_DATASET_VERSION: &str = qx_data::DEPTHFRAME_DATASET_VERSION;
