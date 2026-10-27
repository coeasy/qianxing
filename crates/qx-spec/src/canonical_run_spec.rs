//! Canonical, path-independent identity for a completed or planned run.
//!
//! This is the cross-run-type envelope. Market- and engine-specific behavior stays in the typed
//! `behavior` and `execution_model` objects supplied by the application adapter; run identity,
//! strategy source, dataset content, clock, seed, and market-rule fingerprints use one contract.

use crate::{FoundationDocument, FoundationKind};
use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::{BTreeMap, BTreeSet};

pub const CANONICAL_RUN_SPEC_SCHEMA_VERSION: u32 = 1;
const MAX_SAFE_JSON_INTEGER: u64 = 9_007_199_254_740_991;

/// A strategy implementation and its parameter object as used by one run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalStrategyIdentity {
    pub strategy_id: String,
    pub strategy_version: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_sha256: Option<String>,
    pub parameters: Value,
}

/// Content-addressed market data consumed by a run. Local file paths are intentionally absent.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalDatasetIdentity {
    pub role: String,
    pub instrument: String,
    pub content_fingerprint: String,
    pub start_ts_ms: u64,
    pub end_ts_ms: u64,
}

/// Clock and timestamp interpretation used by the run.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRunClock {
    pub start_ts_ms: u64,
    pub end_ts_ms: u64,
    pub timestamp_unit: String,
    pub timezone: String,
}

/// Stable inputs that affect a run, independent of host paths and output locations.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CanonicalRunSpec {
    pub schema_version: u32,
    pub run_type: String,
    /// Source revision of the engine and built-in strategy implementation.
    pub code_revision: String,
    pub strategies: Vec<CanonicalStrategyIdentity>,
    pub datasets: Vec<CanonicalDatasetIdentity>,
    pub market_rules: BTreeMap<String, String>,
    pub clock: CanonicalRunClock,
    /// Decimal text keeps seeds stable in clients whose JSON number range is narrower than u64.
    pub random_seed: String,
    /// RunType-specific settings, serialized as JSON but validated to exclude floats and unsafe
    /// integer tokens. Decimal money/quantity fields must use canonical decimal strings.
    pub behavior: Value,
    /// Effective engine/fill/fee model descriptors after application defaults are resolved.
    pub execution_model: Value,
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub extensions: BTreeMap<String, Value>,
}

impl CanonicalRunSpec {
    pub fn from_json(payload: &str) -> Result<Self, String> {
        let document: Self = serde_json::from_str(payload)
            .map_err(|error| format!("CanonicalRunSpec 解析失败: {error}"))?;
        document.validate()?;
        Ok(document)
    }

    pub fn new(
        run_type: impl Into<String>,
        code_revision: impl Into<String>,
        strategies: Vec<CanonicalStrategyIdentity>,
        datasets: Vec<CanonicalDatasetIdentity>,
        market_rules: BTreeMap<String, String>,
        clock: CanonicalRunClock,
        random_seed: u64,
        behavior: Value,
        execution_model: Value,
        extensions: BTreeMap<String, Value>,
    ) -> Result<Self, String> {
        let document = Self {
            schema_version: CANONICAL_RUN_SPEC_SCHEMA_VERSION,
            run_type: run_type.into(),
            code_revision: code_revision.into(),
            strategies,
            datasets,
            market_rules,
            clock,
            random_seed: random_seed.to_string(),
            behavior,
            execution_model,
            extensions,
        };
        document.validate()?;
        Ok(document)
    }

    /// Compact JSON with a stable struct-field order and sorted data/market maps.
    pub fn canonical_json(&self) -> Result<String, String> {
        self.validate_fields()?;
        let mut canonical = self.clone();
        canonical.datasets.sort_by(|left, right| {
            (&left.role, &left.instrument, &left.content_fingerprint).cmp(&(
                &right.role,
                &right.instrument,
                &right.content_fingerprint,
            ))
        });
        serde_json::to_string(&canonical)
            .map_err(|error| format!("CanonicalRunSpec 规范化失败: {error}"))
    }

    pub fn fingerprint(&self) -> Result<String, String> {
        let canonical = self.canonical_json()?;
        let mut hash = Fnv1a::new();
        hash.write_bytes(canonical.as_bytes());
        Ok(format!("{:016x}", hash.finish()))
    }

    fn validate_fields(&self) -> Result<(), String> {
        if self.schema_version != CANONICAL_RUN_SPEC_SCHEMA_VERSION {
            return Err(format!(
                "CanonicalRunSpec schema_version={} 不受支持（本构建只认 {}）",
                self.schema_version, CANONICAL_RUN_SPEC_SCHEMA_VERSION
            ));
        }
        if !valid_token(&self.run_type) {
            return Err("CanonicalRunSpec run_type 必须是非空小写标识符".into());
        }
        if self.code_revision.trim().is_empty() {
            return Err("CanonicalRunSpec code_revision 不能为空".into());
        }
        if self.strategies.is_empty() {
            return Err("CanonicalRunSpec 至少要有一个策略身份".into());
        }
        let mut strategy_ids = BTreeSet::new();
        for strategy in &self.strategies {
            if !valid_strategy_id(&strategy.strategy_id)
                || strategy.strategy_version.trim().is_empty()
                || !strategy_ids.insert(strategy.strategy_id.as_str())
            {
                return Err("CanonicalRunSpec 策略身份必须非空且 strategy_id 唯一".into());
            }
            if let Some(digest) = &strategy.source_sha256 {
                if !is_sha256_hex(digest) {
                    return Err(format!(
                        "CanonicalRunSpec 策略 {} 的 source_sha256 无效",
                        strategy.strategy_id
                    ));
                }
            }
            validate_json_value(&strategy.parameters, "strategies.parameters")?;
            validate_path_free(&strategy.parameters, "strategies.parameters")?;
            if !strategy.parameters.is_object() {
                return Err(format!(
                    "CanonicalRunSpec 策略 {} 的 parameters 必须是对象",
                    strategy.strategy_id
                ));
            }
        }
        if self.datasets.is_empty() {
            return Err("CanonicalRunSpec 至少要绑定一个数据集身份".into());
        }
        let mut dataset_keys = BTreeSet::new();
        for dataset in &self.datasets {
            if dataset.role.trim().is_empty()
                || dataset.instrument.trim().is_empty()
                || dataset.content_fingerprint.trim().is_empty()
                || dataset.start_ts_ms > MAX_SAFE_JSON_INTEGER
                || dataset.end_ts_ms > MAX_SAFE_JSON_INTEGER
                || dataset.start_ts_ms > dataset.end_ts_ms
                || !dataset_keys.insert((dataset.role.as_str(), dataset.instrument.as_str()))
            {
                return Err(
                    "CanonicalRunSpec 数据身份必须完整、时间范围合法且 role/instrument 唯一".into(),
                );
            }
        }
        if self.clock.start_ts_ms > self.clock.end_ts_ms
            || self.clock.start_ts_ms > MAX_SAFE_JSON_INTEGER
            || self.clock.end_ts_ms > MAX_SAFE_JSON_INTEGER
            || self.clock.timestamp_unit != "unix_ms"
            || self.clock.timezone != "UTC"
        {
            return Err("CanonicalRunSpec clock 目前要求合法的 unix_ms 时间范围和 UTC 时区".into());
        }
        if !is_canonical_unsigned_decimal(&self.random_seed) {
            return Err("CanonicalRunSpec random_seed 必须是无前导零的无符号十进制文本".into());
        }
        if self.random_seed.parse::<u64>().is_err() {
            return Err("CanonicalRunSpec random_seed 超出 u64 范围".into());
        }
        for (instrument, fingerprint) in &self.market_rules {
            if instrument.trim().is_empty() || fingerprint.trim().is_empty() {
                return Err("CanonicalRunSpec market_rules 不能包含空标的或空指纹".into());
            }
        }
        for (key, value) in &self.extensions {
            if key.trim().is_empty() {
                return Err("CanonicalRunSpec extensions 不能包含空键".into());
            }
            validate_json_value(value, "extensions")?;
            validate_path_free(value, "extensions")?;
        }
        if !self.behavior.is_object() || !self.execution_model.is_object() {
            return Err("CanonicalRunSpec behavior 与 execution_model 必须是对象".into());
        }
        validate_json_value(&self.behavior, "behavior")?;
        validate_path_free(&self.behavior, "behavior")?;
        validate_json_value(&self.execution_model, "execution_model")?;
        validate_path_free(&self.execution_model, "execution_model")?;
        Ok(())
    }
}

fn valid_token(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || b"_-".contains(&byte))
}

fn valid_strategy_id(value: &str) -> bool {
    !value.trim().is_empty() && value.len() <= 128 && !value.chars().any(char::is_control)
}

fn is_canonical_unsigned_decimal(value: &str) -> bool {
    value == "0"
        || (!value.is_empty()
            && !value.starts_with('0')
            && value.bytes().all(|byte| byte.is_ascii_digit()))
}

fn is_sha256_hex(value: &str) -> bool {
    value.len() == 64
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn validate_json_value(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) => {
            if number.is_f64() {
                return Err(format!(
                    "CanonicalRunSpec {path} 不接受浮点数；Decimal 必须使用规范文本"
                ));
            }
            if let Some(number) = number.as_i64() {
                if number.unsigned_abs() > MAX_SAFE_JSON_INTEGER {
                    return Err(format!(
                        "CanonicalRunSpec {path} 含超出跨语言安全范围的整数；请用十进制文本"
                    ));
                }
            } else if let Some(number) = number.as_u64() {
                if number > MAX_SAFE_JSON_INTEGER {
                    return Err(format!(
                        "CanonicalRunSpec {path} 含超出跨语言安全范围的整数；请用十进制文本"
                    ));
                }
            }
            Ok(())
        }
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                validate_json_value(child, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (name, child) in fields {
                validate_json_value(child, &format!("{path}.{name}"))?;
            }
            Ok(())
        }
    }
}

fn validate_path_free(value: &Value, path: &str) -> Result<(), String> {
    match value {
        Value::Array(values) => {
            for (index, child) in values.iter().enumerate() {
                validate_path_free(child, &format!("{path}[{index}]"))?;
            }
        }
        Value::Object(fields) => {
            for (name, child) in fields {
                let normalized = name.to_ascii_lowercase();
                let local_locator = matches!(
                    normalized.as_str(),
                    "path" | "file" | "directory" | "uri" | "run_id"
                ) || ["_path", "_file", "_dir", "_directory", "_uri"]
                    .iter()
                    .any(|suffix| normalized.ends_with(suffix));
                if local_locator {
                    return Err(format!(
                        "CanonicalRunSpec {path}.{name} 是位置或运行实例字段；请只保存内容身份"
                    ));
                }
                validate_path_free(child, &format!("{path}.{name}"))?;
            }
        }
        Value::Null | Value::Bool(_) | Value::Number(_) | Value::String(_) => {}
    }
    Ok(())
}

impl FoundationDocument for CanonicalRunSpec {
    const KIND: FoundationKind = FoundationKind::CanonicalRunSpec;
    const SCHEMA_VERSION: u32 = CANONICAL_RUN_SPEC_SCHEMA_VERSION;

    fn from_json(payload: &str) -> Result<Self, String> {
        Self::from_json(payload)
    }

    fn validate(&self) -> Result<(), String> {
        self.validate_fields()
    }

    fn identity(&self) -> String {
        format!("{} run", self.run_type)
    }

    fn fingerprint(&self) -> Result<String, String> {
        CanonicalRunSpec::fingerprint(self)
    }

    fn to_json(&self) -> Result<String, String> {
        self.canonical_json()
    }
}
