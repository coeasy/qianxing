//! 回测产物落盘：清单、工件与 `BacktestArtifacts` 载体。

use super::read_example_json;
pub(crate) use qx_app::artifact_store::BacktestInputProvenance;
#[cfg(test)]
pub(crate) use qx_app::artifact_store::{persist_backtest_artifacts, BacktestArtifacts};
use qx_datastruct::BarFrame;
use qx_guanxing::Bar;
use qx_xingban::DepthFrame;
use std::path::Path;

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
    qx_app::backtest_input_identity::barframe_dataset_identity(frame_path, frame)
}

/// 深度帧的**唯一**读点，理由同 [`barframe_dataset_identity`]。
pub(crate) fn read_depth_frame_for_backtest(path: &Path) -> Result<DepthFrame, String> {
    let payload = read_example_json(path, "深度数据帧")?;
    DepthFrame::from_json(&payload).map_err(|error| format!("{}: {error}", path.display()))
}

/// BarFrame 数据集的版本号：文件名后缀会一直跟着它，改它就是声明"换了另一种输入形状"。
pub(crate) const BARFRAME_DATASET_VERSION: &str = qx_data::BARFRAME_DATASET_VERSION;
