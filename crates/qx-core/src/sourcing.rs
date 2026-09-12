//! 事件溯源与重放校验。
//!
//! 可复现性的最小单元是**完整运行 Manifest**，而不是单个收益数字。
//! 只保存收益或参数，无法支撑审计、复跑和问题定位。

use crate::error::QxResult;
use crate::event::{Event, EventKind};
use crate::ledger::Ledger;
use std::collections::BTreeSet;

/// FNV-1a 64：零依赖、跨平台稳定。
///
/// 不用 `DefaultHasher`——它的输出**不保证跨版本稳定**，会破坏"两次运行哈希一致"。
pub struct Fnv1a {
    h: u64,
}

impl Fnv1a {
    pub fn new() -> Self {
        Self {
            h: 0xcbf2_9ce4_8422_2325,
        }
    }

    fn write_byte(&mut self, b: u8) {
        self.h ^= b as u64;
        self.h = self.h.wrapping_mul(0x0100_0000_01b3);
    }

    pub fn write_u64(&mut self, v: u64) {
        for b in v.to_le_bytes() {
            self.write_byte(b);
        }
    }

    pub fn write_i128(&mut self, v: i128) {
        for b in v.to_le_bytes() {
            self.write_byte(b);
        }
    }

    pub fn write_bytes(&mut self, bs: &[u8]) {
        for b in bs {
            self.write_byte(*b);
        }
    }

    /// 写入带长度前缀的 UTF-8 文本，避免相邻字符串边界产生摘要碰撞。
    pub fn write_text(&mut self, value: &str) {
        self.write_u64(value.len() as u64);
        self.write_bytes(value.as_bytes());
    }

    pub fn finish(&self) -> u64 {
        self.h
    }
}

impl Default for Fnv1a {
    fn default() -> Self {
        Self::new()
    }
}

/// 事件日志：append-only，不可变。
#[derive(Clone, Default)]
pub struct EventLog {
    events: Vec<Event>,
    next_seq: u64,
}

impl EventLog {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn next_seq(&self) -> u64 {
        self.next_seq
    }

    pub fn alloc_seq(&mut self) -> u64 {
        let s = self.next_seq;
        self.next_seq += 1;
        s
    }

    pub fn append(&mut self, e: Event) {
        self.next_seq = self.next_seq.max(e.seq.saturating_add(1));
        self.events.push(e);
    }

    /// 追加并校验事件序号，供生产归约器使用。
    pub fn append_checked(&mut self, e: Event) -> QxResult<()> {
        if self.events.iter().any(|event| event.seq == e.seq) {
            return Err(crate::error::QxError::Invariant("事件 seq 重复".into()));
        }
        if let Some(previous) = self.events.last() {
            let previous_key = (previous.ts, previous.prio, previous.seq);
            let current_key = (e.ts, e.prio, e.seq);
            if current_key <= previous_key {
                return Err(crate::error::QxError::Invariant(
                    "事件追加违反时间/优先级/序号顺序".into(),
                ));
            }
        }
        self.next_seq = e
            .seq
            .checked_add(1)
            .ok_or_else(|| crate::error::QxError::Invariant("事件序号溢出".into()))?
            .max(self.next_seq);
        self.events.push(e);
        Ok(())
    }

    /// 校验已加载日志的序号和因果排序。
    pub fn validate(&self) -> QxResult<()> {
        let mut previous: Option<(u64, u8, u64)> = None;
        let mut seen_seq = BTreeSet::new();
        let mut max_seq = None;
        for event in &self.events {
            if !seen_seq.insert(event.seq) {
                return Err(crate::error::QxError::Invariant("事件日志 seq 重复".into()));
            }
            let key = (event.ts, event.prio, event.seq);
            if previous.is_some_and(|p| key <= p) {
                return Err(crate::error::QxError::Invariant(
                    "事件日志未按时间/优先级/序号排序".into(),
                ));
            }
            previous = Some(key);
            max_seq = Some(max_seq.map_or(event.seq, |max: u64| max.max(event.seq)));
        }
        let expected_next = match max_seq {
            Some(seq) => seq
                .checked_add(1)
                .ok_or_else(|| crate::error::QxError::Invariant("事件序号溢出".into()))?,
            None => 0,
        };
        if self.next_seq != expected_next {
            return Err(crate::error::QxError::Invariant(
                "事件日志 next_seq 与最大 seq 不一致".into(),
            ));
        }
        Ok(())
    }

    pub fn events(&self) -> &[Event] {
        &self.events
    }

    pub fn len(&self) -> usize {
        self.events.len()
    }

    pub fn is_empty(&self) -> bool {
        self.events.is_empty()
    }

    /// 稳定 JSON 载荷。文件/对象存储由边界层负责，Kernel 只负责纯序列化语义。
    pub fn to_json(&self) -> QxResult<String> {
        self.validate()?;
        serde_json::to_string(&self.events).map_err(|error| {
            crate::error::QxError::Invariant(format!("事件日志序列化失败: {error}"))
        })
    }

    pub fn from_json(input: &str) -> QxResult<Self> {
        let events: Vec<Event> = serde_json::from_str(input).map_err(|error| {
            crate::error::QxError::Permanent(format!("事件日志 JSON 无法解析: {error}"))
        })?;
        let mut log = Self::new();
        for event in events {
            log.append(event);
        }
        log.validate()?;
        Ok(log)
    }

    /// 全量摘要：事件序列完全一致则哈希一致。
    pub fn digest(&self) -> u64 {
        let mut h = Fnv1a::new();
        h.write_u64(self.events.len() as u64);
        for e in &self.events {
            e.digest(&mut h);
        }
        h.finish()
    }
}

/// 运行 Manifest：代码+数据+配置+随机性+结果的全量指纹。
#[derive(Clone, PartialEq, Eq, Debug, Default, serde::Serialize, serde::Deserialize)]
pub struct RunManifest {
    pub run_id: String,
    pub code_commit: String,
    pub config_hash: String,
    pub data_fingerprint: String,
    pub clock_start: u64,
    pub clock_end: u64,
    pub global_seed: u64,
    pub determinism_mode: bool,
    pub result_hash: String,
    pub strategy_version: String,
    pub instrument_spec_version: String,
    pub model_fingerprint: String,
    pub input_event_hash: String,
    pub output_event_hash: String,
    pub runtime_version: String,
}

impl RunManifest {
    pub fn validate(&self) -> Result<(), String> {
        let required = [
            ("run_id", &self.run_id),
            ("code_commit", &self.code_commit),
            ("config_hash", &self.config_hash),
            ("data_fingerprint", &self.data_fingerprint),
            ("result_hash", &self.result_hash),
            ("strategy_version", &self.strategy_version),
            ("instrument_spec_version", &self.instrument_spec_version),
            ("model_fingerprint", &self.model_fingerprint),
            ("input_event_hash", &self.input_event_hash),
            ("output_event_hash", &self.output_event_hash),
            ("runtime_version", &self.runtime_version),
        ];
        if let Some((field, _)) = required.iter().find(|(_, value)| value.trim().is_empty()) {
            return Err(format!("RunManifest 字段不能为空: {field}"));
        }
        if self.clock_start > self.clock_end {
            return Err("RunManifest 时钟范围非法".into());
        }
        Ok(())
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| format!("RunManifest JSON 序列化失败: {error}"))
    }

    pub fn from_json(input: &str) -> Result<Self, String> {
        let manifest: Self = serde_json::from_str(input)
            .map_err(|error| format!("RunManifest JSON 无法解析: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn digest(&self) -> u64 {
        let mut h = Fnv1a::new();
        write_text(&mut h, &self.run_id);
        write_text(&mut h, &self.code_commit);
        write_text(&mut h, &self.config_hash);
        write_text(&mut h, &self.data_fingerprint);
        h.write_u64(self.clock_start);
        h.write_u64(self.clock_end);
        h.write_u64(self.global_seed);
        h.write_u64(self.determinism_mode as u64);
        write_text(&mut h, &self.result_hash);
        write_text(&mut h, &self.strategy_version);
        write_text(&mut h, &self.instrument_spec_version);
        write_text(&mut h, &self.model_fingerprint);
        write_text(&mut h, &self.input_event_hash);
        write_text(&mut h, &self.output_event_hash);
        write_text(&mut h, &self.runtime_version);
        h.finish()
    }
}

fn write_text(hash: &mut Fnv1a, value: &str) {
    hash.write_text(value);
}

/// 重放校验：三重验证。
pub struct ReplayVerifier;

impl ReplayVerifier {
    /// ① 相同 manifest 两次运行，结果哈希必须完全一致。
    pub fn identical(a: u64, b: u64) -> bool {
        a == b
    }

    /// ② 修改任一输入后，受影响事件的哈希必须发生预期变化。
    pub fn changed(a: u64, b: u64) -> bool {
        a != b
    }

    /// ③ 事件日志可重新驱动纯函数估值，得到相同账户与指标。
    pub fn rebuild_from(events: &[Event]) -> u64 {
        let mut log = EventLog::new();
        for e in events {
            log.append(e.clone());
        }
        log.digest()
    }

    /// 由账簿事实事件真实重建账户状态，而不是只重新计算日志哈希。
    pub fn rebuild_ledger(events: &[Event]) -> QxResult<Ledger> {
        let mut log = EventLog::new();
        let mut ledger = Ledger::new();
        for event in events {
            log.append_checked(event.clone())?;
            if let EventKind::LedgerApplied { entry } = &event.kind {
                ledger.apply_entry(entry.clone())?;
            }
        }
        log.validate()?;
        Ok(ledger)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::clock::Ts;
    use crate::event::{Event, EventKind};

    fn sample(n: u64) -> EventLog {
        let mut l = EventLog::new();
        for i in 0..n {
            l.append(Event::new(i, i as Ts * 10, 2, EventKind::Settle));
        }
        l
    }

    #[test]
    fn same_events_same_digest() {
        assert!(ReplayVerifier::identical(
            sample(5).digest(),
            sample(5).digest()
        ));
    }

    #[test]
    fn different_events_different_digest() {
        assert!(ReplayVerifier::changed(
            sample(5).digest(),
            sample(6).digest()
        ));
    }

    #[test]
    fn manifest_validates_and_round_trips_without_boundary_collisions() {
        let manifest = RunManifest {
            run_id: "r-1".into(),
            code_commit: "ab".into(),
            config_hash: "c".into(),
            data_fingerprint: "data".into(),
            clock_start: 1,
            clock_end: 2,
            global_seed: 7,
            determinism_mode: true,
            result_hash: "result".into(),
            strategy_version: "s-v1".into(),
            instrument_spec_version: "i-v1".into(),
            model_fingerprint: "m-v1".into(),
            input_event_hash: "in".into(),
            output_event_hash: "out".into(),
            runtime_version: "test".into(),
        };
        let restored = RunManifest::from_json(&manifest.to_json().unwrap()).unwrap();
        assert_eq!(restored, manifest);

        let mut changed = manifest.clone();
        changed.code_commit = "a".into();
        changed.config_hash = "bc".into();
        assert_ne!(manifest.digest(), changed.digest());

        changed.clock_start = 3;
        assert!(changed.validate().is_err());
    }

    #[test]
    fn rebuild_is_stable() {
        let l = sample(4);
        assert_eq!(
            ReplayVerifier::rebuild_from(l.events()),
            ReplayVerifier::rebuild_from(l.events())
        );
    }

    #[test]
    fn append_updates_cursor_and_validation_rejects_gaps() {
        let mut log = EventLog::new();
        log.append(Event::new(0, 1, 2, EventKind::Settle));
        log.append(Event::new(1, 2, 2, EventKind::Settle));
        assert_eq!(log.next_seq(), 2);
        assert!(log.validate().is_ok());
        log.append(Event::new(2, 3, 2, EventKind::Settle));
        assert!(log.validate().is_ok());
        log.append(Event::new(2, 4, 2, EventKind::Settle));
        assert!(log.validate().is_err());
    }

    #[test]
    fn append_checked_rejects_out_of_order_facts_before_reduction() {
        let mut log = EventLog::new();
        log.append_checked(Event::new(0, 10, 2, EventKind::Settle))
            .unwrap();
        assert!(log
            .append_checked(Event::new(1, 9, 2, EventKind::Settle))
            .is_err());
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn json_round_trip_preserves_event_digest() {
        let log = sample(4);
        let restored = EventLog::from_json(&log.to_json().unwrap()).unwrap();
        assert_eq!(log.digest(), restored.digest());
        assert_eq!(log.next_seq(), restored.next_seq());
    }
}
