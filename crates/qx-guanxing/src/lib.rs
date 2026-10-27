//! # qx-guanxing — 观星
//!
//! 数据平面：Bar/Tick、point-in-time 可见性、机器可读质量门。
//!
//! 前视偏差必须在**数据层封禁**，而不是靠策略自觉。
//! 每个 `DataView` 都提供 `as_of(ts)`，策略只能访问该时刻及之前的数据。

use qx_core::{ExactDecimal, ExactOhlcv, InstrumentId, Price, Quantity};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// 数据源血缘标识。
#[derive(Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Debug, Serialize, Deserialize)]
pub struct DataSourceId(pub String);

impl DataSourceId {
    pub fn new(s: impl Into<String>) -> Self {
        Self(s.into())
    }
}

/// K 线。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Bar {
    pub ts: u64,
    pub open: i128,
    pub high: i128,
    pub low: i128,
    pub close: i128,
    pub volume: i128,
    /// Optional exchange-precision values. Legacy raw columns remain a checked 1e-9 view.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub exact_ohlcv: Option<ExactOhlcv>,
}

impl Bar {
    pub fn new(ts: u64, open: i128, high: i128, low: i128, close: i128, volume: i128) -> Self {
        Self {
            ts,
            open,
            high,
            low,
            close,
            volume,
            exact_ohlcv: None,
        }
    }

    pub fn with_exact_ohlcv(mut self, exact_ohlcv: ExactOhlcv) -> Result<Self, String> {
        exact_ohlcv.validate()?;
        let raw = [self.open, self.high, self.low, self.close, self.volume];
        let exact = exact_ohlcv.raw_projection()?;
        for (raw, exact) in raw.into_iter().zip(exact) {
            if exact != raw {
                return Err("ExactOhlcv raw projection does not match Bar raw columns".into());
            }
        }
        self.exact_ohlcv = Some(exact_ohlcv);
        Ok(self)
    }

    pub fn open_exact(&self) -> ExactDecimal {
        self.exact_ohlcv
            .map(|values| values.open)
            .unwrap_or_else(|| exact_raw(self.open))
    }

    pub fn high_exact(&self) -> ExactDecimal {
        self.exact_ohlcv
            .map(|values| values.high)
            .unwrap_or_else(|| exact_raw(self.high))
    }

    pub fn low_exact(&self) -> ExactDecimal {
        self.exact_ohlcv
            .map(|values| values.low)
            .unwrap_or_else(|| exact_raw(self.low))
    }

    pub fn close_exact(&self) -> ExactDecimal {
        self.exact_ohlcv
            .map(|values| values.close)
            .unwrap_or_else(|| exact_raw(self.close))
    }

    pub fn volume_exact(&self) -> ExactDecimal {
        self.exact_ohlcv
            .map(|values| values.volume)
            .unwrap_or_else(|| exact_raw(self.volume))
    }
}

fn exact_raw(value: i128) -> ExactDecimal {
    ExactDecimal::from_parts(value, 9).expect("legacy Bar raw values use supported scale 9")
}

/// L1 最优报价。没有深度时不得声称具备队列位置或完整盘口能力。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QuoteTick {
    pub ts: u64,
    pub bid: Price,
    pub bid_qty: Quantity,
    pub ask: Price,
    pub ask_qty: Quantity,
    /// Exchange-native displayed quantity when it exceeds the legacy 1e9 scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_qty_exact: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_qty_exact: Option<ExactDecimal>,
    /// Exact venue quote retained when the exchange precision exceeds `Price`'s 1e9 scale.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bid_exact: Option<ExactDecimal>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ask_exact: Option<ExactDecimal>,
    pub source_seq: u64,
}

impl QuoteTick {
    pub fn new(
        ts: u64,
        bid: Price,
        bid_qty: Quantity,
        ask: Price,
        ask_qty: Quantity,
        source_seq: u64,
    ) -> Self {
        Self {
            ts,
            bid,
            bid_qty,
            ask,
            ask_qty,
            bid_qty_exact: None,
            ask_qty_exact: None,
            bid_exact: None,
            ask_exact: None,
            source_seq,
        }
    }

    pub fn with_exact_prices(mut self, bid_exact: ExactDecimal, ask_exact: ExactDecimal) -> Self {
        self.bid_exact = Some(bid_exact);
        self.ask_exact = Some(ask_exact);
        self
    }

    pub fn with_optional_exact_prices(
        mut self,
        bid_exact: Option<ExactDecimal>,
        ask_exact: Option<ExactDecimal>,
    ) -> Self {
        self.bid_exact = bid_exact;
        self.ask_exact = ask_exact;
        self
    }

    pub fn with_exact_sizes(
        mut self,
        bid_qty_exact: ExactDecimal,
        ask_qty_exact: ExactDecimal,
    ) -> Self {
        self.bid_qty_exact = Some(bid_qty_exact);
        self.ask_qty_exact = Some(ask_qty_exact);
        self
    }

    pub fn with_optional_exact_sizes(
        mut self,
        bid_qty_exact: Option<ExactDecimal>,
        ask_qty_exact: Option<ExactDecimal>,
    ) -> Self {
        self.bid_qty_exact = bid_qty_exact;
        self.ask_qty_exact = ask_qty_exact;
        self
    }

    pub fn mid(self) -> Option<Price> {
        if self.ask.raw() < self.bid.raw() {
            return None;
        }
        self.bid
            .checked_add(self.ask)?
            .checked_div(Price::from_i64(2))
    }

    pub fn is_crossed(self) -> bool {
        self.ask.raw() < self.bid.raw()
    }
}

/// 四级数据管线中的不可变原始记录元数据。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RawRecord {
    pub source: DataSourceId,
    pub event_time: u64,
    pub receive_time: u64,
    pub payload_hash: u64,
    pub schema_version: u32,
}

/// PIT 财务原始记录：`publish_time` 决定何时可见，`effective_time` 决定
/// 该报告描述的经济生效时点；两者都不能被研究层省略。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FinancialRecord {
    pub instrument: InstrumentId,
    pub report_period: String,
    pub publish_time: u64,
    pub effective_time: u64,
    pub revision: u32,
    pub source: DataSourceId,
    pub values: BTreeMap<String, i128>,
}

impl FinancialRecord {
    pub fn validate(&self) -> Result<(), QualityReport> {
        if self.report_period.trim().is_empty()
            || self.source.0.trim().is_empty()
            || self.values.is_empty()
            || self.values.keys().any(|key| key.trim().is_empty())
        {
            return Err(QualityReport {
                issues: vec![QualityIssue::InvalidMetadata],
            });
        }
        Ok(())
    }
}

/// 不可变 PIT 财务视图。记录按输入顺序不得隐式重排；查询时只返回
/// `publish_time <= as_of` 且 `effective_time <= as_of` 的每个标的最新修订。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FinancialView {
    records: Vec<FinancialRecord>,
    source: DataSourceId,
}

impl FinancialView {
    pub fn try_new(
        records: Vec<FinancialRecord>,
        source: DataSourceId,
    ) -> Result<Self, QualityReport> {
        if source.0.trim().is_empty() || records.is_empty() {
            return Err(QualityReport {
                issues: vec![QualityIssue::EmptyInput],
            });
        }
        let mut previous = None;
        for record in &records {
            record.validate()?;
            if record.source != source {
                return Err(QualityReport {
                    issues: vec![QualityIssue::InvalidMetadata],
                });
            }
            let key = (
                record.instrument.clone(),
                record.effective_time,
                record.publish_time,
                record.revision,
            );
            if previous.as_ref().is_some_and(|item| key <= *item) {
                return Err(QualityReport {
                    issues: vec![QualityIssue::NonMonotonic {
                        at: 0,
                        prev_ts: previous.map_or(0, |item| item.1),
                        ts: record.effective_time,
                    }],
                });
            }
            previous = Some(key);
        }
        Ok(Self { records, source })
    }

    pub fn source(&self) -> &DataSourceId {
        &self.source
    }

    pub fn all(&self) -> &[FinancialRecord] {
        &self.records
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let view: Self = serde_json::from_str(input).map_err(|error| error.to_string())?;
        Self::try_new(view.records, view.source)
            .map_err(|report| format!("FinancialView 质量校验失败: {:?}", report.issues))
    }

    pub fn as_of(&self, as_of: u64) -> BTreeMap<InstrumentId, BTreeMap<String, i128>> {
        let mut latest: BTreeMap<InstrumentId, &FinancialRecord> = BTreeMap::new();
        for record in &self.records {
            if record.publish_time > as_of || record.effective_time > as_of {
                continue;
            }
            let replace = latest.get(&record.instrument).is_none_or(|current| {
                (record.effective_time, record.publish_time, record.revision)
                    > (
                        current.effective_time,
                        current.publish_time,
                        current.revision,
                    )
            });
            if replace {
                latest.insert(record.instrument.clone(), record);
            }
        }
        latest
            .into_iter()
            .map(|(instrument, record)| (instrument, record.values.clone()))
            .collect()
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct DataViewMetadata {
    pub schema_version: u32,
    pub timezone: String,
    pub calendar_version: String,
    pub price_scale: i128,
    pub volume_scale: i128,
    pub data_hash: u64,
}

impl DataViewMetadata {
    fn default_for(source: &DataSourceId, bars: &[Bar]) -> Self {
        Self {
            schema_version: 1,
            timezone: "UTC".into(),
            calendar_version: "unspecified".into(),
            price_scale: qx_core::SCALE,
            volume_scale: qx_core::SCALE,
            data_hash: bars_digest(source, bars),
        }
    }

    pub fn validate(&self) -> Result<(), QualityReport> {
        if self.schema_version == 0
            || self.timezone.trim().is_empty()
            || self.calendar_version.trim().is_empty()
            || self.price_scale <= 0
            || self.volume_scale <= 0
        {
            return Err(QualityReport {
                issues: vec![QualityIssue::InvalidMetadata],
            });
        }
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Default, Serialize, Deserialize)]
pub struct ParameterSet(pub BTreeMap<String, i128>);

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct CandidateConfig {
    pub strategy_version: String,
    pub universe_version: String,
    pub feature_version: String,
    pub parameters: ParameterSet,
    pub data_fingerprint: String,
    pub intended_exposure: BTreeMap<InstrumentId, i128>,
    pub constraints: BTreeMap<String, i128>,
    pub execution_model: String,
    pub risk_model: String,
}

/// 数据视图：唯一被策略允许访问的行情入口。
#[derive(Clone, Serialize, Deserialize)]
pub struct DataView {
    bars: Vec<Bar>,
    source: DataSourceId,
    metadata: DataViewMetadata,
}

impl DataView {
    /// 在不接管行情所有权的情况下完成质量校验并生成数据元信息。
    ///
    /// 适用于只需要质量裁决和内容指纹的消费者，避免为了临时构造视图而复制整份 Bar 序列。
    pub fn metadata_for_borrowed(
        bars: &[Bar],
        source: &DataSourceId,
    ) -> Result<DataViewMetadata, QualityReport> {
        let report = QualityGate::check(bars);
        if matches!(report.verdict(), Verdict::Fail | Verdict::Quarantine) {
            return Err(report);
        }
        let metadata = DataViewMetadata::default_for(source, bars);
        metadata.validate()?;
        Ok(metadata)
    }

    /// 生产入口：拒绝未排序、重复或其他质量问题，不隐式修正原始事实。
    pub fn try_new(bars: Vec<Bar>, source: DataSourceId) -> Result<Self, QualityReport> {
        let metadata = Self::metadata_for_borrowed(&bars, &source)?;
        Ok(Self {
            bars,
            source,
            metadata,
        })
    }

    pub fn try_new_with_metadata(
        bars: Vec<Bar>,
        source: DataSourceId,
        metadata: DataViewMetadata,
    ) -> Result<Self, QualityReport> {
        let report = QualityGate::check(&bars);
        if matches!(report.verdict(), Verdict::Fail | Verdict::Quarantine) {
            return Err(report);
        }
        metadata.validate()?;
        let actual_hash = bars_digest(&source, &bars);
        if metadata.data_hash != actual_hash {
            return Err(QualityReport {
                issues: vec![QualityIssue::MetadataHashMismatch],
            });
        }
        Ok(Self {
            bars,
            source,
            metadata,
        })
    }

    /// 便捷构造入口，与 `try_new` 同样显式返回质量校验错误。
    pub fn new(bars: Vec<Bar>, source: DataSourceId) -> Result<Self, QualityReport> {
        Self::try_new(bars, source)
    }

    pub fn source(&self) -> &DataSourceId {
        &self.source
    }

    pub fn metadata(&self) -> &DataViewMetadata {
        &self.metadata
    }

    /// **只返回 ts 及之前的数据**——这是 point-in-time 的唯一入口。
    ///
    /// 返回切片而非副本：热路径上不做无谓拷贝。
    pub fn as_of(&self, ts: u64) -> &[Bar] {
        let n = self.bars.partition_point(|b| b.ts <= ts);
        &self.bars[..n]
    }

    pub fn all(&self) -> &[Bar] {
        &self.bars
    }

    pub fn to_json(&self) -> Result<String, String> {
        serde_json::to_string(self).map_err(|error| error.to_string())
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let view: Self = serde_json::from_str(input).map_err(|error| error.to_string())?;
        Self::try_new_with_metadata(view.bars, view.source, view.metadata)
            .map_err(|report| format!("DataView 质量校验失败: {:?}", report.issues))
    }

    pub fn len(&self) -> usize {
        self.bars.len()
    }

    pub fn is_empty(&self) -> bool {
        self.bars.is_empty()
    }
}

/// 质量问题：必须机器可读，不能只写日志。
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum QualityIssue {
    EmptyInput,
    NonMonotonic { at: usize, prev_ts: u64, ts: u64 },
    HighLessThanLow { at: usize },
    CloseOutOfRange { at: usize },
    ZeroVolume { at: usize },
    DuplicateTimestamp { at: usize },
    NegativePrice { at: usize },
    InvalidExactOhlcv { at: usize },
    InvalidMetadata,
    MetadataHashMismatch,
}

/// 质量判定：quarantine 可审计但不得进入实时信号；fail 必须阻断启动。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Verdict {
    Ok,
    Warn,
    Quarantine,
    Fail,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QualityReport {
    pub issues: Vec<QualityIssue>,
}

impl QualityReport {
    pub fn verdict(&self) -> Verdict {
        if self.issues.is_empty() {
            return Verdict::Ok;
        }
        let fatal = self.issues.iter().any(|i| {
            matches!(
                i,
                QualityIssue::EmptyInput
                    | QualityIssue::InvalidMetadata
                    | QualityIssue::MetadataHashMismatch
                    | QualityIssue::NonMonotonic { .. }
                    | QualityIssue::NegativePrice { .. }
                    | QualityIssue::HighLessThanLow { .. }
                    | QualityIssue::CloseOutOfRange { .. }
                    | QualityIssue::InvalidExactOhlcv { .. }
            )
        });
        if fatal {
            return Verdict::Fail;
        }
        if self
            .issues
            .iter()
            .any(|i| matches!(i, QualityIssue::DuplicateTimestamp { .. }))
        {
            return Verdict::Quarantine;
        }
        Verdict::Warn
    }
}

pub struct QualityGate;

impl QualityGate {
    pub fn check(bars: &[Bar]) -> QualityReport {
        let mut issues = Vec::new();
        if bars.is_empty() {
            issues.push(QualityIssue::EmptyInput);
        }
        for (i, b) in bars.iter().enumerate() {
            if b.exact_ohlcv.is_some_and(|exact| {
                exact.validate().is_err()
                    || exact.raw_projection().map_or(true, |raw| {
                        raw != [b.open, b.high, b.low, b.close, b.volume]
                    })
            }) {
                issues.push(QualityIssue::InvalidExactOhlcv { at: i });
            }
            if b.open < 0 || b.high < 0 || b.low < 0 || b.close < 0 || b.volume < 0 {
                issues.push(QualityIssue::NegativePrice { at: i });
            }
            let (high, low, close, volume_is_zero) = match b.exact_ohlcv {
                Some(exact) => (exact.high, exact.low, exact.close, exact.volume.is_zero()),
                None => (
                    ExactDecimal::from_parts(b.high, 9).expect("raw Bar scale is supported"),
                    ExactDecimal::from_parts(b.low, 9).expect("raw Bar scale is supported"),
                    ExactDecimal::from_parts(b.close, 9).expect("raw Bar scale is supported"),
                    b.volume == 0,
                ),
            };
            if high
                .checked_cmp(low)
                .is_some_and(|ordering| ordering.is_lt())
            {
                issues.push(QualityIssue::HighLessThanLow { at: i });
            }
            if close
                .checked_cmp(high)
                .is_some_and(|ordering| ordering.is_gt())
                || close
                    .checked_cmp(low)
                    .is_some_and(|ordering| ordering.is_lt())
            {
                issues.push(QualityIssue::CloseOutOfRange { at: i });
            }
            if volume_is_zero {
                issues.push(QualityIssue::ZeroVolume { at: i });
            }
            if i > 0 {
                let p = &bars[i - 1];
                if b.ts < p.ts {
                    issues.push(QualityIssue::NonMonotonic {
                        at: i,
                        prev_ts: p.ts,
                        ts: b.ts,
                    });
                } else if b.ts == p.ts {
                    issues.push(QualityIssue::DuplicateTimestamp { at: i });
                }
            }
        }
        QualityReport { issues }
    }
}

fn bars_digest(source: &DataSourceId, bars: &[Bar]) -> u64 {
    let mut hash = qx_core::Fnv1a::new();
    hash.write_text(&source.0);
    hash.write_u64(bars.len() as u64);
    for bar in bars {
        hash.write_u64(bar.ts);
        hash.write_i128(bar.open);
        hash.write_i128(bar.high);
        hash.write_i128(bar.low);
        hash.write_i128(bar.close);
        hash.write_i128(bar.volume);
        if let Some(exact) = bar.exact_ohlcv {
            hash.write_u64(1);
            for value in [exact.open, exact.high, exact.low, exact.close, exact.volume] {
                hash.write_text(&value.to_string());
            }
        }
    }
    hash.finish()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bars() -> Vec<Bar> {
        vec![
            Bar::new(10, 100, 110, 90, 105, 1000),
            Bar::new(20, 105, 120, 100, 115, 1200),
            Bar::new(30, 115, 130, 110, 120, 900),
        ]
    }

    #[test]
    fn as_of_never_returns_future() {
        let v = DataView::new(bars(), DataSourceId::new("test")).expect("valid fixture");
        assert_eq!(v.as_of(0).len(), 0);
        assert_eq!(v.as_of(10).len(), 1);
        assert_eq!(v.as_of(20).len(), 2);
        assert_eq!(v.as_of(999).len(), 3);
    }

    #[test]
    fn clean_data_passes() {
        let r = QualityGate::check(&bars());
        assert_eq!(r.verdict(), Verdict::Ok);
    }

    #[test]
    fn source_precision_changes_the_data_view_identity_even_when_raw_columns_match() {
        let exact_bar = |volume: &str| {
            Bar::new(10, 10, 12, 9, 10, 100)
                .with_exact_ohlcv(ExactOhlcv {
                    open: ExactDecimal::parse("0.0000000101").unwrap(),
                    high: ExactDecimal::parse("0.0000000129").unwrap(),
                    low: ExactDecimal::parse("0.0000000095").unwrap(),
                    close: ExactDecimal::parse("0.0000000101").unwrap(),
                    volume: ExactDecimal::parse(volume).unwrap(),
                })
                .unwrap()
        };
        let first = DataView::try_new(
            vec![exact_bar("0.0000001001")],
            DataSourceId::new("exact-source"),
        )
        .unwrap();
        let second = DataView::try_new(
            vec![exact_bar("0.0000001002")],
            DataSourceId::new("exact-source"),
        )
        .unwrap();
        assert_eq!(first.all()[0].volume, second.all()[0].volume);
        assert_ne!(first.metadata().data_hash, second.metadata().data_hash);
    }

    #[test]
    fn exact_positive_sub_nano_volume_is_not_misclassified_as_zero() {
        let bar = Bar::new(
            10,
            qx_core::SCALE,
            qx_core::SCALE,
            qx_core::SCALE,
            qx_core::SCALE,
            0,
        )
        .with_exact_ohlcv(ExactOhlcv {
            open: ExactDecimal::ONE,
            high: ExactDecimal::ONE,
            low: ExactDecimal::ONE,
            close: ExactDecimal::ONE,
            volume: ExactDecimal::parse("0.0000000001").unwrap(),
        })
        .unwrap();
        let view = DataView::try_new(vec![bar], DataSourceId::new("tiny-volume"))
            .expect("a positive exact source volume is not zero-volume data");
        assert_eq!(
            view.metadata().data_hash,
            bars_digest(view.source(), view.all())
        );
    }

    #[test]
    fn data_view_exposes_immutable_metadata_and_hash() {
        let view = DataView::new(bars(), DataSourceId::new("test")).expect("valid fixture");
        assert_eq!(view.metadata().schema_version, 1);
        assert_eq!(view.metadata().timezone, "UTC");
        assert_ne!(view.metadata().data_hash, 0);
        let mut metadata = view.metadata().clone();
        metadata.data_hash ^= 1;
        assert!(
            DataView::try_new_with_metadata(bars(), DataSourceId::new("test"), metadata).is_err()
        );
        let restored = DataView::from_json(&view.to_json().unwrap()).unwrap();
        assert_eq!(restored.metadata(), view.metadata());
        let mut forged: serde_json::Value = serde_json::from_str(&view.to_json().unwrap()).unwrap();
        forged["metadata"]["data_hash"] = serde_json::Value::from(1_u64);
        assert!(DataView::from_json(&forged.to_string()).is_err());
    }

    #[test]
    fn empty_inputs_are_rejected_by_quality_gate() {
        assert_eq!(QualityGate::check(&[]).verdict(), Verdict::Fail);
        assert!(DataView::try_new(Vec::new(), DataSourceId::new("test")).is_err());
    }

    #[test]
    fn checked_view_rejects_unsorted_input_without_reordering() {
        let mut input = bars();
        input.swap(0, 1);
        let result = DataView::try_new(input, DataSourceId::new("test"));
        assert!(result.is_err());
    }

    #[test]
    fn non_monotonic_is_fatal() {
        let mut b = bars();
        b[2].ts = 5;
        let r = QualityGate::check(&b);
        assert_eq!(r.verdict(), Verdict::Fail);
    }

    #[test]
    fn zero_volume_only_warns() {
        let mut b = bars();
        b[1].volume = 0;
        let r = QualityGate::check(&b);
        assert_eq!(r.verdict(), Verdict::Warn);
    }

    #[test]
    fn financial_view_obeys_publish_and_effective_time_pit() {
        let instrument = qx_core::InstrumentId::parse("T.V").unwrap();
        let source = DataSourceId::new("financial-v1");
        let mut first_values = BTreeMap::new();
        first_values.insert("revenue".into(), 100);
        let mut revised_values = BTreeMap::new();
        revised_values.insert("revenue".into(), 120);
        let view = FinancialView::try_new(
            vec![
                FinancialRecord {
                    instrument: instrument.clone(),
                    report_period: "2025Q4".into(),
                    publish_time: 20,
                    effective_time: 10,
                    revision: 1,
                    source: source.clone(),
                    values: first_values,
                },
                FinancialRecord {
                    instrument: instrument.clone(),
                    report_period: "2025Q4".into(),
                    publish_time: 30,
                    effective_time: 10,
                    revision: 2,
                    source: source.clone(),
                    values: revised_values,
                },
            ],
            source,
        )
        .unwrap();
        assert!(view.as_of(15).is_empty());
        assert_eq!(view.as_of(25)[&instrument]["revenue"], 100);
        assert_eq!(view.as_of(30)[&instrument]["revenue"], 120);
        let restored = FinancialView::from_json(&view.to_json().unwrap()).unwrap();
        assert_eq!(restored, view);
    }
}
