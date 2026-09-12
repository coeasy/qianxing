//! DataStruct Facade：在 Kernel 之外提供稳定的列式数据边界。
//!
//! 当前实现不把 Arrow/Python 类型引入 Kernel 热路径，而是固定列名、顺序、PIT 截止和
//! raw 定点语义；边界层同时提供 Arrow C Data Interface 借用视图和 JSON/Python 转换，
//! 不能重新解释领域数据。

use qx_core::Fnv1a;
use qx_core::InstrumentId;
use qx_guanxing::{Bar, DataSourceId, DataView};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::ffi::c_char;
use std::marker::PhantomData;
use std::os::raw::c_void;
use std::sync::atomic::{AtomicU8, Ordering};

/// Arrow C Data Interface 的数组描述；数据所有权仍归 `BarFrame`。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ArrowArray {
    pub length: i64,
    pub null_count: i64,
    pub offset: i64,
    pub n_buffers: i64,
    pub n_children: i64,
    pub buffers: *const *const c_void,
    pub children: *mut *mut ArrowArray,
    pub dictionary: *mut ArrowArray,
    pub release: Option<unsafe extern "C" fn(*mut ArrowArray)>,
    pub private_data: *mut c_void,
}

/// Arrow C Data Interface 的 schema 描述。
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct ArrowSchema {
    pub format: *const c_char,
    pub name: *const c_char,
    pub metadata: *const c_char,
    pub flags: i64,
    pub n_children: i64,
    pub children: *mut *mut ArrowSchema,
    pub dictionary: *mut ArrowSchema,
    pub release: Option<unsafe extern "C" fn(*mut ArrowSchema)>,
    pub private_data: *mut c_void,
}

/// 借用型 Arrow 列视图：只复制指针和描述，不复制 `BarFrame` 的列数据。
///
/// `release` 保持为空，表示该对象不可独立释放；调用方必须保证视图生命周期不超过
/// 所属 `BarFrame`。需要跨边界转移所有权时，应由专用桥接层复制或接管数据。
pub struct ArrowColumnView<'a> {
    array: ArrowArray,
    schema: ArrowSchema,
    buffers: Box<[*const c_void; 2]>,
    _frame: PhantomData<&'a BarFrame>,
}

enum OwnedArrowStorage {
    U64(Vec<u64>),
    Decimal128(Vec<i128>),
}

#[allow(dead_code)]
struct OwnedArrowState {
    releases_remaining: AtomicU8,
    storage: OwnedArrowStorage,
    buffers: Box<[*const c_void; 2]>,
}

/// 拥有型 Arrow 列导出。调用 `into_ffi` 后所有权转移给 Arrow C Data Interface，
/// 调用方必须分别释放返回的 `ArrowArray` 和 `ArrowSchema`。
pub struct OwnedArrowColumn {
    array: ArrowArray,
    schema: ArrowSchema,
    state: Option<Box<OwnedArrowState>>,
}

impl<'a> ArrowColumnView<'a> {
    pub fn array(&self) -> &ArrowArray {
        &self.array
    }

    pub fn schema(&self) -> &ArrowSchema {
        &self.schema
    }

    pub fn data_ptr(&self) -> *const c_void {
        self.buffers[1]
    }

    /// 返回静态 schema 字段名；用于调试和不依赖 C 字符串生命周期的 Rust 调用方。
    pub fn name(&self) -> &'static str {
        match self.schema.name as usize {
            value if value == ARROW_NAME_TS.as_ptr() as usize => "ts",
            value if value == ARROW_NAME_OPEN.as_ptr() as usize => "open_raw",
            value if value == ARROW_NAME_HIGH.as_ptr() as usize => "high_raw",
            value if value == ARROW_NAME_LOW.as_ptr() as usize => "low_raw",
            value if value == ARROW_NAME_CLOSE.as_ptr() as usize => "close_raw",
            value if value == ARROW_NAME_VOLUME.as_ptr() as usize => "volume_raw",
            _ => "unknown",
        }
    }
}

impl OwnedArrowColumn {
    fn new(name: &'static [u8], format: &'static [u8], storage: OwnedArrowStorage) -> Self {
        let data_ptr = match &storage {
            OwnedArrowStorage::U64(values) => values.as_ptr().cast::<c_void>(),
            OwnedArrowStorage::Decimal128(values) => values.as_ptr().cast::<c_void>(),
        };
        let length = match &storage {
            OwnedArrowStorage::U64(values) => values.len(),
            OwnedArrowStorage::Decimal128(values) => values.len(),
        };
        let buffers = Box::new([std::ptr::null(), data_ptr]);
        let buffer_ptr = buffers.as_ptr();
        Self {
            array: ArrowArray {
                length: length as i64,
                null_count: 0,
                offset: 0,
                n_buffers: 2,
                n_children: 0,
                buffers: buffer_ptr,
                children: std::ptr::null_mut(),
                dictionary: std::ptr::null_mut(),
                release: None,
                private_data: std::ptr::null_mut(),
            },
            schema: ArrowSchema {
                format: format.as_ptr().cast(),
                name: name.as_ptr().cast(),
                metadata: std::ptr::null(),
                flags: 0,
                n_children: 0,
                children: std::ptr::null_mut(),
                dictionary: std::ptr::null_mut(),
                release: None,
                private_data: std::ptr::null_mut(),
            },
            state: Some(Box::new(OwnedArrowState {
                releases_remaining: AtomicU8::new(2),
                storage,
                buffers,
            })),
        }
    }

    pub fn array(&self) -> &ArrowArray {
        &self.array
    }

    pub fn schema(&self) -> &ArrowSchema {
        &self.schema
    }

    /// 将数组和 schema 的所有权转移给 C Data Interface 调用方。
    pub fn into_ffi(self) -> (ArrowArray, ArrowSchema) {
        let Self {
            mut array,
            mut schema,
            state,
        } = self;
        let state = state.expect("owned Arrow state must exist");
        let private_data = Box::into_raw(state).cast::<c_void>();
        array.release = Some(release_owned_array);
        array.private_data = private_data;
        schema.release = Some(release_owned_schema);
        schema.private_data = private_data;
        (array, schema)
    }
}

unsafe extern "C" fn release_owned_array(array: *mut ArrowArray) {
    if array.is_null() {
        return;
    }
    let private_data = (*array).private_data;
    (*array).release = None;
    (*array).private_data = std::ptr::null_mut();
    release_owned_state(private_data);
}

unsafe extern "C" fn release_owned_schema(schema: *mut ArrowSchema) {
    if schema.is_null() {
        return;
    }
    let private_data = (*schema).private_data;
    (*schema).release = None;
    (*schema).private_data = std::ptr::null_mut();
    release_owned_state(private_data);
}

unsafe fn release_owned_state(private_data: *mut c_void) {
    if private_data.is_null() {
        return;
    }
    let state = private_data.cast::<OwnedArrowState>();
    if (*state).releases_remaining.fetch_sub(1, Ordering::AcqRel) == 1 {
        drop(Box::from_raw(state));
    }
}

// Arrow C Data Interface format: `L` is uint64; `u` would mean UTF-8 string.
static ARROW_FORMAT_UINT64: &[u8] = b"L\0";
static ARROW_FORMAT_DECIMAL128: &[u8] = b"d:38,0\0";
static ARROW_NAME_TS: &[u8] = b"ts\0";
static ARROW_NAME_OPEN: &[u8] = b"open_raw\0";
static ARROW_NAME_HIGH: &[u8] = b"high_raw\0";
static ARROW_NAME_LOW: &[u8] = b"low_raw\0";
static ARROW_NAME_CLOSE: &[u8] = b"close_raw\0";
static ARROW_NAME_VOLUME: &[u8] = b"volume_raw\0";

fn arrow_column<'a, T>(
    _frame: &'a BarFrame,
    name: &'static [u8],
    format: &'static [u8],
    values: &'a [T],
) -> ArrowColumnView<'a> {
    let buffers = Box::new([std::ptr::null(), values.as_ptr().cast::<c_void>()]);
    let buffer_ptr = buffers.as_ptr();
    ArrowColumnView {
        array: ArrowArray {
            length: values.len() as i64,
            null_count: 0,
            offset: 0,
            n_buffers: 2,
            n_children: 0,
            buffers: buffer_ptr,
            children: std::ptr::null_mut(),
            dictionary: std::ptr::null_mut(),
            release: None,
            private_data: std::ptr::null_mut(),
        },
        schema: ArrowSchema {
            format: format.as_ptr().cast(),
            name: name.as_ptr().cast(),
            metadata: std::ptr::null(),
            flags: 0,
            n_children: 0,
            children: std::ptr::null_mut(),
            dictionary: std::ptr::null_mut(),
            release: None,
            private_data: std::ptr::null_mut(),
        },
        buffers,
        _frame: PhantomData,
    }
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub struct BarFrame {
    pub instrument: InstrumentId,
    pub source: DataSourceId,
    pub ts: Vec<u64>,
    pub open_raw: Vec<i128>,
    pub high_raw: Vec<i128>,
    pub low_raw: Vec<i128>,
    pub close_raw: Vec<i128>,
    pub volume_raw: Vec<i128>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum FrameError {
    Empty,
    ColumnLengthMismatch,
    NotMonotonic,
    InvalidInterval,
    NumericOverflow,
    ArrowDecimalOverflow,
    InvalidJson(String),
    InvalidInstrument(String),
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct TransformManifest {
    pub operation: String,
    pub algorithm_version: String,
    pub parameters: BTreeMap<String, String>,
    pub input_hash: u64,
    pub output_hash: u64,
}

impl TransformManifest {
    pub fn validate(&self) -> Result<(), FrameError> {
        if self.operation.trim().is_empty()
            || self.algorithm_version.trim().is_empty()
            || self.input_hash == 0
            || self.output_hash == 0
        {
            return Err(FrameError::InvalidJson(
                "TransformManifest 缺少操作字段或输入/输出哈希".into(),
            ));
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, FrameError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| FrameError::InvalidJson(error.to_string()))
    }

    pub fn from_json(input: &str) -> Result<Self, FrameError> {
        let manifest: Self = serde_json::from_str(input)
            .map_err(|error| FrameError::InvalidJson(error.to_string()))?;
        manifest.validate()?;
        Ok(manifest)
    }
}

#[derive(Deserialize)]
struct BarFrameWire {
    instrument: String,
    source: String,
    ts: Vec<u64>,
    open_raw: Vec<i128>,
    high_raw: Vec<i128>,
    low_raw: Vec<i128>,
    close_raw: Vec<i128>,
    volume_raw: Vec<i128>,
}

impl BarFrame {
    pub fn from_json(input: &str) -> Result<Self, FrameError> {
        let wire: BarFrameWire = serde_json::from_str(input)
            .map_err(|error| FrameError::InvalidJson(error.to_string()))?;
        let instrument = InstrumentId::parse(&wire.instrument)
            .ok_or_else(|| FrameError::InvalidInstrument(wire.instrument.clone()))?;
        let frame = Self {
            instrument,
            source: DataSourceId::new(wire.source),
            ts: wire.ts,
            open_raw: wire.open_raw,
            high_raw: wire.high_raw,
            low_raw: wire.low_raw,
            close_raw: wire.close_raw,
            volume_raw: wire.volume_raw,
        };
        frame.validate()?;
        Ok(frame)
    }

    pub fn from_view(
        instrument: InstrumentId,
        view: &DataView,
        as_of: u64,
    ) -> Result<Self, FrameError> {
        let bars = view.as_of(as_of);
        if bars.is_empty() {
            return Err(FrameError::Empty);
        }
        let frame = Self {
            instrument,
            source: view.source().clone(),
            ts: bars.iter().map(|bar| bar.ts).collect(),
            open_raw: bars.iter().map(|bar| bar.open).collect(),
            high_raw: bars.iter().map(|bar| bar.high).collect(),
            low_raw: bars.iter().map(|bar| bar.low).collect(),
            close_raw: bars.iter().map(|bar| bar.close).collect(),
            volume_raw: bars.iter().map(|bar| bar.volume).collect(),
        };
        frame.validate()?;
        Ok(frame)
    }

    pub fn validate(&self) -> Result<(), FrameError> {
        let n = self.ts.len();
        if n == 0 {
            return Err(FrameError::Empty);
        }
        if [
            self.open_raw.len(),
            self.high_raw.len(),
            self.low_raw.len(),
            self.close_raw.len(),
            self.volume_raw.len(),
        ]
        .iter()
        .any(|length| *length != n)
        {
            return Err(FrameError::ColumnLengthMismatch);
        }
        if self.ts.windows(2).any(|window| window[0] >= window[1]) {
            return Err(FrameError::NotMonotonic);
        }
        Ok(())
    }

    pub fn len(&self) -> usize {
        self.ts.len()
    }

    pub fn is_empty(&self) -> bool {
        self.ts.is_empty()
    }

    /// 导出六列 Arrow C Data Interface 借用视图，不复制底层列数据。
    /// i128 定点列以 Decimal128(38, 0) 暴露；超出 Arrow Decimal128 精度时拒绝导出，
    /// 不能静默截断为浮点或 i64。
    pub fn arrow_column_views(&self) -> Result<Vec<ArrowColumnView<'_>>, FrameError> {
        self.validate()?;
        let decimal_limit = (10_i128.pow(38) - 1) as u128;
        let decimal_columns = [
            &self.open_raw,
            &self.high_raw,
            &self.low_raw,
            &self.close_raw,
            &self.volume_raw,
        ];
        if decimal_columns
            .iter()
            .flat_map(|column| column.iter())
            .any(|value| value.unsigned_abs() > decimal_limit)
        {
            return Err(FrameError::ArrowDecimalOverflow);
        }
        Ok(vec![
            arrow_column(self, ARROW_NAME_TS, ARROW_FORMAT_UINT64, &self.ts),
            arrow_column(
                self,
                ARROW_NAME_OPEN,
                ARROW_FORMAT_DECIMAL128,
                &self.open_raw,
            ),
            arrow_column(
                self,
                ARROW_NAME_HIGH,
                ARROW_FORMAT_DECIMAL128,
                &self.high_raw,
            ),
            arrow_column(self, ARROW_NAME_LOW, ARROW_FORMAT_DECIMAL128, &self.low_raw),
            arrow_column(
                self,
                ARROW_NAME_CLOSE,
                ARROW_FORMAT_DECIMAL128,
                &self.close_raw,
            ),
            arrow_column(
                self,
                ARROW_NAME_VOLUME,
                ARROW_FORMAT_DECIMAL128,
                &self.volume_raw,
            ),
        ])
    }

    /// 生成可跨语言长期持有的拥有型 Arrow 列；该路径明确复制数据，随后可通过
    /// `OwnedArrowColumn::into_ffi` 将释放责任转移给 C Data Interface 调用方。
    pub fn owned_arrow_columns(&self) -> Result<Vec<OwnedArrowColumn>, FrameError> {
        self.arrow_column_views()?;
        Ok(vec![
            OwnedArrowColumn::new(
                ARROW_NAME_TS,
                ARROW_FORMAT_UINT64,
                OwnedArrowStorage::U64(self.ts.clone()),
            ),
            OwnedArrowColumn::new(
                ARROW_NAME_OPEN,
                ARROW_FORMAT_DECIMAL128,
                OwnedArrowStorage::Decimal128(self.open_raw.clone()),
            ),
            OwnedArrowColumn::new(
                ARROW_NAME_HIGH,
                ARROW_FORMAT_DECIMAL128,
                OwnedArrowStorage::Decimal128(self.high_raw.clone()),
            ),
            OwnedArrowColumn::new(
                ARROW_NAME_LOW,
                ARROW_FORMAT_DECIMAL128,
                OwnedArrowStorage::Decimal128(self.low_raw.clone()),
            ),
            OwnedArrowColumn::new(
                ARROW_NAME_CLOSE,
                ARROW_FORMAT_DECIMAL128,
                OwnedArrowStorage::Decimal128(self.close_raw.clone()),
            ),
            OwnedArrowColumn::new(
                ARROW_NAME_VOLUME,
                ARROW_FORMAT_DECIMAL128,
                OwnedArrowStorage::Decimal128(self.volume_raw.clone()),
            ),
        ])
    }

    pub fn close_at(&self, index: usize) -> Option<i128> {
        self.close_raw.get(index).copied()
    }

    pub fn digest(&self) -> u64 {
        let mut hash = Fnv1a::new();
        write_text(&mut hash, &self.instrument.to_string());
        write_text(&mut hash, &self.source.0);
        hash.write_u64(self.len() as u64);
        for index in 0..self.len() {
            hash.write_u64(self.ts[index]);
            hash.write_i128(self.open_raw[index]);
            hash.write_i128(self.high_raw[index]);
            hash.write_i128(self.low_raw[index]);
            hash.write_i128(self.close_raw[index]);
            hash.write_i128(self.volume_raw[index]);
        }
        hash.finish()
    }

    /// 返回新的不可变时间视图，边界均为闭区间。
    pub fn select_time(&self, start: u64, end: u64) -> Result<Self, FrameError> {
        if start > end {
            return Err(FrameError::InvalidInterval);
        }
        let indexes = self
            .ts
            .iter()
            .enumerate()
            .filter_map(|(index, ts)| (*ts >= start && *ts <= end).then_some(index))
            .collect::<Vec<_>>();
        if indexes.is_empty() {
            return Err(FrameError::Empty);
        }
        let frame = Self {
            instrument: self.instrument.clone(),
            source: self.source.clone(),
            ts: indexes.iter().map(|i| self.ts[*i]).collect(),
            open_raw: indexes.iter().map(|i| self.open_raw[*i]).collect(),
            high_raw: indexes.iter().map(|i| self.high_raw[*i]).collect(),
            low_raw: indexes.iter().map(|i| self.low_raw[*i]).collect(),
            close_raw: indexes.iter().map(|i| self.close_raw[*i]).collect(),
            volume_raw: indexes.iter().map(|i| self.volume_raw[*i]).collect(),
        };
        frame.validate()?;
        Ok(frame)
    }

    pub fn select_time_with_manifest(
        &self,
        start: u64,
        end: u64,
    ) -> Result<(Self, TransformManifest), FrameError> {
        let input_hash = self.digest();
        let frame = self.select_time(start, end)?;
        let mut parameters = BTreeMap::new();
        parameters.insert("start".into(), start.to_string());
        parameters.insert("end".into(), end.to_string());
        let manifest = TransformManifest {
            operation: "select_time".into(),
            algorithm_version: "qx-datastruct/select-time-v1".into(),
            parameters,
            input_hash,
            output_hash: frame.digest(),
        };
        manifest.validate()?;
        Ok((frame, manifest))
    }

    /// 按固定时间宽度重采样；窗口键为 `floor(ts / interval) * interval`。
    pub fn resample(&self, interval: u64) -> Result<Self, FrameError> {
        if interval == 0 {
            return Err(FrameError::InvalidInterval);
        }
        let mut rows = Vec::<Bar>::new();
        for index in 0..self.len() {
            let bucket = (self.ts[index] / interval)
                .checked_mul(interval)
                .ok_or(FrameError::NumericOverflow)?;
            match rows.last_mut() {
                Some(last) if last.ts == bucket => {
                    last.high = last.high.max(self.high_raw[index]);
                    last.low = last.low.min(self.low_raw[index]);
                    last.close = self.close_raw[index];
                    last.volume = last
                        .volume
                        .checked_add(self.volume_raw[index])
                        .ok_or(FrameError::NumericOverflow)?;
                }
                _ => rows.push(Bar::new(
                    bucket,
                    self.open_raw[index],
                    self.high_raw[index],
                    self.low_raw[index],
                    self.close_raw[index],
                    self.volume_raw[index],
                )),
            }
        }
        let frame = Self {
            instrument: self.instrument.clone(),
            source: self.source.clone(),
            ts: rows.iter().map(|bar| bar.ts).collect(),
            open_raw: rows.iter().map(|bar| bar.open).collect(),
            high_raw: rows.iter().map(|bar| bar.high).collect(),
            low_raw: rows.iter().map(|bar| bar.low).collect(),
            close_raw: rows.iter().map(|bar| bar.close).collect(),
            volume_raw: rows.iter().map(|bar| bar.volume).collect(),
        };
        frame.validate()?;
        Ok(frame)
    }

    pub fn resample_with_manifest(
        &self,
        interval: u64,
    ) -> Result<(Self, TransformManifest), FrameError> {
        let input_hash = self.digest();
        let frame = self.resample(interval)?;
        let mut parameters = BTreeMap::new();
        parameters.insert("interval".into(), interval.to_string());
        let manifest = TransformManifest {
            operation: "resample".into(),
            algorithm_version: "qx-datastruct/resample-floor-v1".into(),
            parameters,
            input_hash,
            output_hash: frame.digest(),
        };
        manifest.validate()?;
        Ok((frame, manifest))
    }

    /// 固定字段顺序的轻量 JSON，列值均为 raw integer，便于跨语言桥接和 golden test。
    pub fn to_json(&self) -> String {
        format!(
            "{{\"instrument\":\"{}\",\"source\":\"{}\",\"ts\":{},\"open_raw\":{},\"high_raw\":{},\"low_raw\":{},\"close_raw\":{},\"volume_raw\":{}}}",
            escape_json(&self.instrument.to_string()),
            escape_json(&self.source.0),
            json_array(&self.ts),
            json_array(&self.open_raw),
            json_array(&self.high_raw),
            json_array(&self.low_raw),
            json_array(&self.close_raw),
            json_array(&self.volume_raw),
        )
    }
}

fn write_text(hash: &mut Fnv1a, value: &str) {
    hash.write_u64(value.len() as u64);
    hash.write_bytes(value.as_bytes());
}

fn escape_json(value: &str) -> String {
    let mut escaped = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '"' => escaped.push_str("\\\""),
            '\\' => escaped.push_str("\\\\"),
            '\n' => escaped.push_str("\\n"),
            '\r' => escaped.push_str("\\r"),
            '\t' => escaped.push_str("\\t"),
            ch if ch.is_control() => escaped.push_str(&format!("\\u{:04x}", ch as u32)),
            ch => escaped.push(ch),
        }
    }
    escaped
}

impl From<&BarFrame> for Vec<Bar> {
    fn from(frame: &BarFrame) -> Self {
        (0..frame.len())
            .map(|index| {
                Bar::new(
                    frame.ts[index],
                    frame.open_raw[index],
                    frame.high_raw[index],
                    frame.low_raw[index],
                    frame.close_raw[index],
                    frame.volume_raw[index],
                )
            })
            .collect()
    }
}

fn json_array<T: std::fmt::Display>(values: &[T]) -> String {
    format!(
        "[{}]",
        values
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join(",")
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn frame_is_pit_bounded_and_round_trips_columns() {
        let instrument = InstrumentId::parse("T.SIM").unwrap();
        let view = DataView::try_new(
            vec![
                Bar::new(1, 10, 11, 9, 10, 1),
                Bar::new(2, 11, 12, 10, 11, 2),
                Bar::new(3, 12, 13, 11, 12, 3),
            ],
            DataSourceId::new("bars-v1"),
        )
        .unwrap();
        let frame = BarFrame::from_view(instrument, &view, 2).unwrap();
        assert_eq!(frame.len(), 2);
        assert_eq!(frame.close_at(1), Some(11));
        assert_eq!(Vec::<Bar>::from(&frame)[1].ts, 2);
        assert!(frame.to_json().contains("\"volume_raw\":[1,2]"));
        assert_eq!(BarFrame::from_json(&frame.to_json()).unwrap(), frame);
        assert_eq!(frame.digest(), 0xf9d5_8b91_e72d_ae58);
        let selected = frame.select_time(2, 2).unwrap();
        assert_eq!(selected.ts, vec![2]);
        let resampled = frame.resample(2).unwrap();
        assert_eq!(resampled.ts, vec![0, 2]);
        assert_eq!(resampled.volume_raw, vec![1, 2]);
        assert_eq!(frame.resample(0), Err(FrameError::InvalidInterval));
        let (selected, manifest) = frame.select_time_with_manifest(2, 3).unwrap();
        assert_eq!(manifest.operation, "select_time");
        assert_eq!(manifest.input_hash, frame.digest());
        assert_eq!(manifest.output_hash, selected.digest());
        assert_eq!(
            TransformManifest::from_json(&manifest.to_json().unwrap()).unwrap(),
            manifest
        );
        let (_, resample_manifest) = frame.resample_with_manifest(2).unwrap();
        assert_eq!(resample_manifest.parameters["interval"], "2");
    }

    #[test]
    fn arrow_views_are_zero_copy_and_reject_decimal_overflow() {
        let instrument = InstrumentId::parse("T.SIM").unwrap();
        let view = DataView::try_new(
            vec![
                Bar::new(1, 10, 11, 9, 10, 1),
                Bar::new(2, 11, 12, 10, 11, 2),
            ],
            DataSourceId::new("bars-v1"),
        )
        .unwrap();
        let frame = BarFrame::from_view(instrument, &view, 2).unwrap();
        let columns = frame.arrow_column_views().unwrap();
        assert_eq!(columns.len(), 6);
        assert_eq!(columns[0].name(), "ts");
        assert_eq!(columns[0].array().length, 2);
        assert_eq!(columns[0].data_ptr(), frame.ts.as_ptr().cast());
        assert_eq!(columns[1].name(), "open_raw");
        assert_eq!(columns[1].data_ptr(), frame.open_raw.as_ptr().cast());
        assert_eq!(
            columns[1].schema().format,
            ARROW_FORMAT_DECIMAL128.as_ptr().cast()
        );

        let mut extreme = frame.clone();
        extreme.open_raw[0] = i128::MAX;
        assert!(matches!(
            extreme.arrow_column_views(),
            Err(FrameError::ArrowDecimalOverflow)
        ));

        let mut owned = frame.owned_arrow_columns().unwrap();
        let owned_open = owned.remove(1);
        let (mut array, mut schema) = owned_open.into_ffi();
        assert!(array.release.is_some());
        assert!(schema.release.is_some());
        unsafe {
            (array.release.expect("array release callback"))(&mut array);
            (schema.release.expect("schema release callback"))(&mut schema);
        }
        assert!(array.release.is_none());
        assert!(schema.release.is_none());
    }
}
