//! # qx-guanxing — 观星
//!
//! 数据平面：Bar/Tick、point-in-time 可见性、机器可读质量门。
//!
//! 前视偏差必须在**数据层封禁**，而不是靠策略自觉。
//! 每个 `DataView` 都提供 `as_of(ts)`，策略只能访问该时刻及之前的数据。

use qx_core::{InstrumentId, Price, Quantity};
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
        }
    }
}

/// L1 最优报价。没有深度时不得声称具备队列位置或完整盘口能力。
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct QuoteTick {
    pub ts: u64,
    pub bid: Price,
    pub bid_qty: Quantity,
    pub ask: Price,
    pub ask_qty: Quantity,
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
            source_seq,
        }
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

/// 内存版 DataCatalog：为本地回测/纸面联调提供确定性的主源目录。
#[derive(Default)]
pub struct DataCatalog {
    bars: BTreeMap<InstrumentId, Vec<Bar>>,
    quotes: BTreeMap<InstrumentId, Vec<QuoteTick>>,
    raw: Vec<RawRecord>,
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

#[derive(Clone, PartialEq, Eq, Debug, Default)]
pub struct ParameterGrid {
    pub dimensions: BTreeMap<String, Vec<i128>>,
}

impl ParameterGrid {
    pub fn add(mut self, name: impl Into<String>, values: Vec<i128>) -> Self {
        self.dimensions.insert(name.into(), values);
        self
    }

    pub fn expand(&self) -> Vec<ParameterSet> {
        let mut out = vec![ParameterSet::default()];
        for (name, values) in &self.dimensions {
            let mut next = Vec::new();
            for base in &out {
                for value in values {
                    let mut p = base.0.clone();
                    p.insert(name.clone(), *value);
                    next.push(ParameterSet(p));
                }
            }
            out = next;
        }
        out
    }
}

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

pub struct VectorScanner;

impl VectorScanner {
    pub fn scan(
        grid: &ParameterGrid,
        strategy_version: &str,
        universe_version: &str,
        feature_version: &str,
        data_fingerprint: &str,
    ) -> Vec<CandidateConfig> {
        grid.expand()
            .into_iter()
            .map(|parameters| CandidateConfig {
                strategy_version: strategy_version.into(),
                universe_version: universe_version.into(),
                feature_version: feature_version.into(),
                parameters,
                data_fingerprint: data_fingerprint.into(),
                intended_exposure: BTreeMap::new(),
                constraints: BTreeMap::new(),
                execution_model: "vector-only@v1".into(),
                risk_model: "unbound@v1".into(),
            })
            .collect()
    }
}

impl DataCatalog {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn append_raw(&mut self, record: RawRecord) {
        self.raw.push(record);
    }

    pub fn put_bars(
        &mut self,
        instrument: InstrumentId,
        bars: Vec<Bar>,
    ) -> Result<(), QualityReport> {
        let report = QualityGate::check(&bars);
        if matches!(report.verdict(), Verdict::Fail | Verdict::Quarantine) {
            return Err(report);
        }
        self.bars.insert(instrument, bars);
        Ok(())
    }

    pub fn put_quotes(
        &mut self,
        instrument: InstrumentId,
        quotes: Vec<QuoteTick>,
    ) -> Result<(), QualityReport> {
        let report = QualityGate::check_quotes(&quotes);
        if matches!(report.verdict(), Verdict::Fail | Verdict::Quarantine) {
            return Err(report);
        }
        self.quotes.insert(instrument, quotes);
        Ok(())
    }

    pub fn bars_as_of(&self, instrument: &InstrumentId, ts: u64) -> &[Bar] {
        self.bars
            .get(instrument)
            .map(|v| &v[..v.partition_point(|b| b.ts <= ts)])
            .unwrap_or(&[])
    }

    pub fn quotes_as_of(&self, instrument: &InstrumentId, ts: u64) -> &[QuoteTick] {
        self.quotes
            .get(instrument)
            .map(|v| &v[..v.partition_point(|q| q.ts <= ts)])
            .unwrap_or(&[])
    }

    pub fn raw_records(&self) -> &[RawRecord] {
        &self.raw
    }
}

/// 数据视图：唯一被策略允许访问的行情入口。
#[derive(Clone, Serialize, Deserialize)]
pub struct DataView {
    bars: Vec<Bar>,
    source: DataSourceId,
    metadata: DataViewMetadata,
}

impl DataView {
    /// 生产入口：拒绝未排序、重复或其他质量问题，不隐式修正原始事实。
    pub fn try_new(bars: Vec<Bar>, source: DataSourceId) -> Result<Self, QualityReport> {
        let metadata = DataViewMetadata::default_for(&source, &bars);
        Self::try_new_with_metadata(bars, source, metadata)
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

    /// 兼容旧调用方的便捷构造。外部生产数据应使用 `try_new`，避免静默排序掩盖血缘问题。
    pub fn new(bars: Vec<Bar>, source: DataSourceId) -> Self {
        Self::try_new(bars, source).expect("DataView::new requires quality-checked monotonic bars")
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
    CrossedBook { at: usize },
    QuoteNonMonotonic { at: usize },
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
                    | QualityIssue::CrossedBook { .. }
                    | QualityIssue::QuoteNonMonotonic { .. }
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
            if b.open < 0 || b.high < 0 || b.low < 0 || b.close < 0 || b.volume < 0 {
                issues.push(QualityIssue::NegativePrice { at: i });
            }
            if b.high < b.low {
                issues.push(QualityIssue::HighLessThanLow { at: i });
            }
            if b.close > b.high || b.close < b.low {
                issues.push(QualityIssue::CloseOutOfRange { at: i });
            }
            if b.volume == 0 {
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

    pub fn check_quotes(quotes: &[QuoteTick]) -> QualityReport {
        let mut issues = Vec::new();
        if quotes.is_empty() {
            issues.push(QualityIssue::EmptyInput);
        }
        for (i, q) in quotes.iter().enumerate() {
            if q.is_crossed() {
                issues.push(QualityIssue::CrossedBook { at: i });
            }
            if q.bid.raw() < 0 || q.ask.raw() < 0 || q.bid_qty.raw() < 0 || q.ask_qty.raw() < 0 {
                issues.push(QualityIssue::NegativePrice { at: i });
            }
            if i > 0 && q.ts <= quotes[i - 1].ts {
                issues.push(QualityIssue::QuoteNonMonotonic { at: i });
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
    }
    hash.finish()
}

/// 为定点数值提供的展示辅助（避免策略层直接依赖浮点）。
pub trait NumericExt {
    fn to_display(&self) -> f64;
}

impl NumericExt for i128 {
    fn to_display(&self) -> f64 {
        (*self as f64) / 1e9
    }
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
        let v = DataView::new(bars(), DataSourceId::new("test"));
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
    fn data_view_exposes_immutable_metadata_and_hash() {
        let view = DataView::new(bars(), DataSourceId::new("test"));
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
        assert_eq!(QualityGate::check_quotes(&[]).verdict(), Verdict::Fail);
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
    fn catalog_is_point_in_time() {
        let instrument = qx_core::InstrumentId::parse("T.V").unwrap();
        let mut c = DataCatalog::new();
        c.put_quotes(
            instrument.clone(),
            vec![
                QuoteTick::new(
                    10,
                    Price::from_i64(99),
                    Quantity::from_i64(1),
                    Price::from_i64(101),
                    Quantity::from_i64(2),
                    1,
                ),
                QuoteTick::new(
                    20,
                    Price::from_i64(100),
                    Quantity::from_i64(1),
                    Price::from_i64(102),
                    Quantity::from_i64(2),
                    2,
                ),
            ],
        )
        .unwrap();
        assert_eq!(c.quotes_as_of(&instrument, 10).len(), 1);
        assert_eq!(c.quotes_as_of(&instrument, 19).len(), 1);
        assert_eq!(c.quotes_as_of(&instrument, 20).len(), 2);
    }

    #[test]
    fn crossed_quote_is_fatal() {
        let q = QuoteTick::new(
            1,
            Price::from_i64(101),
            Quantity::from_i64(1),
            Price::from_i64(100),
            Quantity::from_i64(1),
            1,
        );
        assert_eq!(QualityGate::check_quotes(&[q]).verdict(), Verdict::Fail);
    }

    #[test]
    fn vector_scan_is_deterministic_and_keeps_provenance() {
        let grid = ParameterGrid::default()
            .add("fast", vec![5, 10])
            .add("slow", vec![20, 30]);
        let candidates = VectorScanner::scan(&grid, "s-v1", "u-v1", "f-v1", "data-hash");
        assert_eq!(candidates.len(), 4);
        assert_eq!(candidates[0].parameters.0.get("fast"), Some(&5));
        assert_eq!(candidates[0].data_fingerprint, "data-hash");
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
