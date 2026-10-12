use crate::error::{AppError, AppErrorCategory};
use serde::{Deserialize, Serialize};

/// `ValidateDataset` 的输出：一份**可复核**的裁决。
///
/// `gaps` 是"不适用/未计算"的理由清单——与路线图 §X2 同口径：指标在数据不足时
/// 给理由，**不填零**。空 `gaps` 才表示"这一档输入没有已知缺口"。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct DatasetVerdict {
    pub dataset_id: String,
    pub instrument: String,
    /// 实际读到的 Bar 根数。
    pub rows: u64,
    /// 内容指纹（`barframe:<16 位十六进制>`）——与 RunManifest 的合成身份同前缀。
    pub content_fingerprint: String,
    /// 这一档输入能否支撑一个 Bar 回测。`false` 时 `gaps` 必非空。
    pub usable: bool,
    /// 不适用/未计算的理由（**空**才表示没有已知缺口）。
    pub gaps: Vec<String>,
}

impl DatasetVerdict {
    /// 稳定 JSON 形态。
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("DatasetVerdict 序列化失败: {error}"),
            )
        })
    }
}

/// `RunBacktest` 的输出：一次运行的身份与产物落点。
///
/// 它刻意**只带指针不带内容**：产物正文在盘上，`result_hash` 与 `data_fingerprint`
/// 是复核用的两把钥匙。三个入口拿到的 `result_hash` 必须逐位相等（G1）。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacktestOutcome {
    pub run_id: String,
    pub instrument: String,
    /// 引擎给的稳定结果哈希（与 `RunManifest.result_hash` 同源，16 位十六进制）。
    pub result_hash: String,
    /// 合成数据身份：`barframe:<内容哈希>`。
    pub data_fingerprint: String,
    pub fills: u64,
    pub equity_points: u64,
    pub return_bps: i32,
    pub max_drawdown_bps: u32,
    /// 四份产物的路径：run manifest / summary / equity / fills。
    pub artifacts: BacktestArtifacts,
    /// Account ownership, omitted by outcomes created before account-aware specs.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub account_id: Option<String>,
}

/// 一次运行落盘的四份产物路径。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct BacktestArtifacts {
    pub run_manifest: String,
    pub summary: String,
    pub equity: String,
    pub fills: String,
}

impl BacktestOutcome {
    /// 稳定 JSON 形态（三个入口逐字节可比）。
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("BacktestOutcome 序列化失败: {error}"),
            )
        })
    }

    /// 从 JSON 读入（[`BacktestOutcome::to_json`] 的逆）。
    ///
    /// 它是 [`crate::cases::verify_run`] 的**输入面**：`app backtest` 的 stdout 直接喂给
    /// `app verify`，Python 与 HTTP 两个入口也一样。读不回来是调用方交错了文档——
    /// [`AppErrorCategory::InvalidInput`]，不是存储故障。
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BacktestOutcome JSON 无效: {error}"),
            )
        })
    }
}

/// `VerifyRun` 的输出：产物**在盘且互相自洽**的裁决。
///
/// `checks` 是逐条核过的口径（一句话一条），`mismatches` 是不一致的点。
/// `verified == mismatches.is_empty()`——**不**给一个"部分通过"的中间态：
/// 产物复核要么四份互相印证，要么有一处对不上。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct VerificationResult {
    pub run_id: String,
    /// 从产物里读回来的结果哈希（复核的对象）。
    pub result_hash: String,
    /// 从产物里读回来的数据身份。
    pub data_fingerprint: String,
    pub verified: bool,
    pub checks: Vec<String>,
    pub mismatches: Vec<String>,
}

impl VerificationResult {
    /// 稳定 JSON 形态。
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("VerificationResult 序列化失败: {error}"),
            )
        })
    }
}
