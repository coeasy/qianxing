//! 应用层稳定输入/输出类型（T2-1）。
//!
//! 这里的类型是**应用层的公共面**：CLI、Python SDK、HTTP API 三个入口都只经它们
//! 进出用例，所以它们必须比任何单个入口活得久。三条纪律：
//!
//! 1. **带版本**：每个 spec 有 `schema_version`，不匹配就 [`AppErrorCategory::InvalidInput`]，
//!    不做"尽力而为"的字段猜测。
//! 2. **严格**：`deny_unknown_fields`。多一个键就是拼错了字段名，静默忽略等于让
//!    用户以为设上了。
//! 3. **定点原值显式**：金额/数量走 `*_raw`（i128 定点），与仓内 wire 风格一致；
//!    应用层不接受浮点，也不替调用方做单位换算。
//!
//! 还有第 4 条只对身份字段成立：`run_id`/`dataset_id` 必须落在 `[A-Za-z0-9._-]` 里。
//! 它们会变成产物**文件名**的一部分，也会出现在三个入口的 URL/命令行里；放行 `/`、`\`
//! 或 `..` 等于让调用方决定往哪写文件。这条限制写在 `validate()` 里而不是"写文件时
//! 顺手 sanitize"：sanitize 会把两个不同的 run_id 静默映射到同一个文件，
//! 而**拒绝**会让调用方当场知道自己的身份串不合法。

use crate::error::{AppError, AppErrorCategory};
use qx_strategy::BuiltinStrategyKind;
use serde::{Deserialize, Serialize};

/// `DatasetSpec` 的 schema 版本。改形状必须抬它，否则旧调用方会静默按新语义读。
pub const DATASET_SPEC_SCHEMA_VERSION: u32 = 1;

/// `BacktestSpec` 的 schema 版本。
pub const BACKTEST_SPEC_SCHEMA_VERSION: u32 = 1;

/// Bar 回测对样本量的下限：少于这个数，"指标"不是指标而是噪声。
///
/// 它是一道**档位闸**而不是礼貌检查：样本不足时正确行为是
/// [`AppErrorCategory::FidelityInsufficient`]，不是给一个看起来像结论的数。
pub const MIN_BACKTEST_BARS: usize = 2;

/// 身份串的合法字符集：ASCII 字母、数字、`.`、`-`、`_`。
///
/// 私有：这是一条**校验规则**，不是公共 API——放出去就等于承诺这个集合不变，
/// 而它随时可能因为多一个入口的路径约定而调整。
fn is_identity_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 128
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '.' || c == '-' || c == '_')
}

/// `ValidateDataset` 的输入。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DatasetSpec {
    pub schema_version: u32,
    /// 数据集身份。会随裁决一起回来，供调用方对齐注册表。
    pub dataset_id: String,
    /// BarFrame JSON 的路径。相对路径按进程当前目录解析（与仓内其余入口同口径）。
    pub bars_path: String,
}

impl DatasetSpec {
    /// 构造一份当前版本的 spec。
    pub fn new(dataset_id: impl Into<String>, bars_path: impl Into<String>) -> Self {
        Self {
            schema_version: DATASET_SPEC_SCHEMA_VERSION,
            dataset_id: dataset_id.into(),
            bars_path: bars_path.into(),
        }
    }

    /// 形状自检。**只查"这份 spec 自己站不站得住"**，不碰磁盘。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != DATASET_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DatasetSpec schema_version={} 本版本只认 {}",
                    self.schema_version, DATASET_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        if !is_identity_token(&self.dataset_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "DatasetSpec.dataset_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符，实际 {:?}",
                    self.dataset_id
                ),
            ));
        }
        if self.bars_path.trim().is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "DatasetSpec.bars_path 不能为空",
            ));
        }
        Ok(())
    }

    /// 从 JSON 读入（严格：多余键即拒）。
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("DatasetSpec JSON 无效: {error}"),
            )
        })
    }
}

/// 内置 Bar 策略的声明（应用层面）。刻意只暴露**确定会改结果**的那几格：
/// 窗口、数量与身份。费用/撮合/保证金是回测口径，由 spec 的其余段决定。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BuiltinStrategySpec {
    pub kind: BuiltinStrategyKind,
    pub strategy_id: String,
    /// 每笔下单数量（定点原值）。
    pub quantity_raw: i128,
    pub fast_window: usize,
    pub slow_window: usize,
    /// Indicator period used by RSI/channel/volatility strategies.
    #[serde(default = "default_strategy_period")]
    pub period: usize,
    /// Signal threshold in basis points, where used by the selected strategy.
    #[serde(default = "default_strategy_threshold_bps")]
    pub threshold_bps: i128,
}

fn default_strategy_period() -> usize {
    14
}

fn default_strategy_threshold_bps() -> i128 {
    100
}

impl BuiltinStrategySpec {
    pub fn new(kind: BuiltinStrategyKind, strategy_id: impl Into<String>) -> Self {
        Self {
            kind,
            strategy_id: strategy_id.into(),
            quantity_raw: qx_core::SCALE,
            fast_window: 5,
            slow_window: 20,
            period: default_strategy_period(),
            threshold_bps: default_strategy_threshold_bps(),
        }
    }

    /// 形状自检与领域策略旋钮清单保持一致；不生效的旋钮不阻断本次运行。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.strategy_id.trim().is_empty() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.strategy_id 不能为空",
            ));
        }
        if self.quantity_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.quantity_raw 必须为正",
            ));
        }
        use qx_strategy::builtin_signal::BuiltinSignalKnob::{
            FastWindow, Period, SlowWindow, ThresholdBps,
        };
        let uses = |knob| self.kind.uses_signal_knob(knob);
        if (uses(FastWindow) || uses(SlowWindow))
            && (self.fast_window == 0
                || self.slow_window == 0
                || self.fast_window >= self.slow_window)
        {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec 快慢窗口必须为正且快线短于慢线",
            ));
        }
        if uses(Period) && self.period < 2 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.period 至少为 2",
            ));
        }
        if uses(ThresholdBps) && self.threshold_bps < 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BuiltinStrategySpec.threshold_bps 不得为负",
            ));
        }
        Ok(())
    }
}

/// `RunBacktest` 的输入。
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BacktestSpec {
    pub schema_version: u32,
    /// 运行身份。同时是**correlation id**：报错、产物、日志三处用它对齐。
    pub run_id: String,
    pub instrument: String,
    pub bars_path: String,
    /// 结算币种。它决定现金腿落在哪本账簿，所以必填——不给默认值。
    pub settlement_currency: String,
    /// 本金（定点原值）。V11 Q72 之后这是必答题：藏在默认值里会让整条收益率
    /// 压在一个调用方毫不知情的常数上。
    pub initial_cash_raw: i128,
    pub seed: u64,
    /// 产物目录。四份产物（`*.run.json` / `*.summary.json` / `*.equity.csv` / `*.fills.csv`）
    /// 都落在它下面。
    pub output_dir: String,
    pub strategy: BuiltinStrategySpec,
}

impl BacktestSpec {
    /// 构造一份当前版本的 spec（其余字段按调用方给的填）。
    #[allow(clippy::too_many_arguments)] // 与产物身份逐格对应，合并成参数包会让必答项变成可选。
    pub fn new(
        run_id: impl Into<String>,
        instrument: impl Into<String>,
        bars_path: impl Into<String>,
        settlement_currency: impl Into<String>,
        initial_cash_raw: i128,
        seed: u64,
        output_dir: impl Into<String>,
        strategy: BuiltinStrategySpec,
    ) -> Self {
        Self {
            schema_version: BACKTEST_SPEC_SCHEMA_VERSION,
            run_id: run_id.into(),
            instrument: instrument.into(),
            bars_path: bars_path.into(),
            settlement_currency: settlement_currency.into(),
            initial_cash_raw,
            seed,
            output_dir: output_dir.into(),
            strategy,
        }
    }

    /// 形状自检。**只查"这份 spec 自己站不站得住"**，不碰磁盘。
    pub fn validate(&self) -> Result<(), AppError> {
        if self.schema_version != BACKTEST_SPEC_SCHEMA_VERSION {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "BacktestSpec schema_version={} 本版本只认 {}",
                    self.schema_version, BACKTEST_SPEC_SCHEMA_VERSION
                ),
            ));
        }
        for (field, value) in [
            ("instrument", &self.instrument),
            ("bars_path", &self.bars_path),
            ("settlement_currency", &self.settlement_currency),
            ("output_dir", &self.output_dir),
        ] {
            if value.trim().is_empty() {
                return Err(AppError::new(
                    AppErrorCategory::InvalidInput,
                    format!("BacktestSpec.{field} 不能为空"),
                ));
            }
        }
        if !is_identity_token(&self.run_id) {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!(
                    "BacktestSpec.run_id 必须是 [A-Za-z0-9._-] 且不超过 128 字符（它会变成产物文件名），实际 {:?}",
                    self.run_id
                ),
            ));
        }
        if qx_core::InstrumentId::parse(&self.instrument).is_none() {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BacktestSpec.instrument 非法: {}", self.instrument),
            ));
        }
        if self.initial_cash_raw <= 0 {
            return Err(AppError::new(
                AppErrorCategory::InvalidInput,
                "BacktestSpec.initial_cash_raw 必须为正",
            ));
        }
        self.strategy.validate()
    }

    /// 从 JSON 读入（严格：多余键即拒）。
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("BacktestSpec JSON 无效: {error}"),
            )
        })
    }

    /// 稳定 JSON 形态（规范化输出，供三个入口逐字节比对）。
    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self).map_err(|error| {
            AppError::new(
                AppErrorCategory::InternalInvariant,
                format!("BacktestSpec 序列化失败: {error}"),
            )
        })
    }
}

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
