//! Canonical experiment identities, numeric normalization, and stable result hashing.

use super::*;
use qx_spec::{CanonicalDatasetIdentity, CanonicalRunClock, CanonicalStrategyIdentity};

pub(super) fn build_experiment_canonical_run_spec(
    spec: &RunExperimentSpec,
    identity: Option<&StrategyArtifactIdentity>,
    spec_json: &str,
    frame: &BarFrame,
    data_fingerprint: &str,
    context: &RunContext,
) -> Result<CanonicalRunSpec, AppError> {
    let start_ts_ms =
        frame.ts.first().copied().ok_or_else(|| {
            experiment_identity_invalid("CanonicalRunSpec 缺少 BarFrame 开始时间")
        })?;
    let end_ts_ms =
        frame.ts.last().copied().ok_or_else(|| {
            experiment_identity_invalid("CanonicalRunSpec 缺少 BarFrame 结束时间")
        })?;

    let portable_spec_json = quote_unsafe_integer_tokens(spec_json);
    let mut behavior: Value = serde_json::from_str(&portable_spec_json).map_err(|error| {
        experiment_identity_invalid(format!("RunExperimentSpec 规范化 JSON 无效: {error}"))
    })?;
    let fields = behavior
        .as_object_mut()
        .ok_or_else(|| experiment_identity_invalid("RunExperimentSpec 规范化结果不是 JSON 对象"))?;
    fields.remove("experiment_id");
    let base = fields
        .get_mut("base")
        .and_then(Value::as_object_mut)
        .ok_or_else(|| experiment_identity_invalid("实验 CanonicalRunSpec 缺少 base"))?;
    for field in ["run_id", "bars_path", "output_dir"] {
        base.remove(field);
    }
    if let Some(dimensions) = fields
        .get_mut("parameter_space")
        .and_then(Value::as_array_mut)
    {
        for dimension in dimensions {
            if let Some(values) = dimension.get_mut("values").and_then(Value::as_array_mut) {
                for value in values {
                    canonicalize_integer(value);
                }
            }
        }
    }
    if let Some(ranges) = fields
        .get_mut("search")
        .and_then(|search| search.get_mut("continuous_parameter_space"))
        .and_then(Value::as_array_mut)
    {
        for range in ranges {
            for field in ["min", "max", "step"] {
                if let Some(value) = range.get_mut(field) {
                    canonicalize_integer(value);
                }
            }
        }
    }
    normalize_experiment_identity_values(&mut behavior, "behavior")?;

    let mut kinds = if spec.strategy_kinds.is_empty() {
        vec![spec.base.strategy.kind]
    } else {
        spec.strategy_kinds.clone()
    };
    kinds.sort_by_key(|kind| kind.name());
    let mut strategies = Vec::with_capacity(kinds.len());
    for kind in kinds {
        let mut strategy = spec.base.strategy.clone();
        strategy.kind = kind;
        let mut parameters = serde_json::to_value(&strategy).map_err(|error| {
            experiment_identity_invalid(format!("实验策略参数转 JSON 失败: {error}"))
        })?;
        normalize_experiment_identity_values(&mut parameters, "strategies.parameters")?;
        let strategy_id = if spec.strategy_kinds.is_empty() {
            strategy.strategy_id.clone()
        } else {
            format!("{}#{}", strategy.strategy_id, kind.name())
        };
        strategies.push(CanonicalStrategyIdentity {
            strategy_id,
            strategy_version: identity
                .map(|identity| identity.strategy_version.clone())
                .unwrap_or_else(|| {
                    super::super::run_backtest::strategy_slot::strategy_version(&strategy)
                }),
            source_sha256: identity.map(|identity| identity.source_sha256.clone()),
            parameters,
        });
    }

    let mut market_rules = std::collections::BTreeMap::new();
    if let Some(market_spec) = &spec.base.market_spec {
        let mut market_rules_value = serde_json::to_value(market_spec)
            .map_err(|error| experiment_identity_invalid(format!("市场规则转换失败: {error}")))?;
        normalize_experiment_identity_values(&mut market_rules_value, "market_rules")?;
        let bytes = serde_json::to_vec(&market_rules_value)
            .map_err(|error| experiment_identity_invalid(format!("市场规则编码失败: {error}")))?;
        market_rules.insert(
            spec.base.instrument.clone(),
            qx_strategy::sha256_hex(&bytes),
        );
    }

    let runtime_version = format!("qx-app-{}", env!("CARGO_PKG_VERSION"));
    let execution_model = serde_json::json!({
        "runtime_version": runtime_version,
        "matching_kernel": crate::cases::kernels::BAR_MATCHING_KERNEL,
        "fill_model": match spec.base.fill_model.unwrap_or(crate::BacktestFillModel::NextBarOpen) {
            crate::BacktestFillModel::NextBarOpen => "next_bar_open",
            crate::BacktestFillModel::BestPrice => "best_price",
            crate::BacktestFillModel::OneTickSlippage => "one_tick_slippage",
        },
        "risk_rule_set_version": spec.base.risk_rules.as_ref().map(|rules| rules.version.as_str()).unwrap_or("runtime-risk-v1"),
        "account_id": spec.base.account_id.as_deref().unwrap_or("app"),
        "settlement_currency": spec.base.settlement_currency,
    });
    CanonicalRunSpec::new(
        "experiment",
        context.code_commit(),
        strategies,
        vec![CanonicalDatasetIdentity {
            role: "bars".into(),
            instrument: spec.base.instrument.clone(),
            content_fingerprint: data_fingerprint.into(),
            start_ts_ms,
            end_ts_ms,
        }],
        market_rules,
        CanonicalRunClock {
            start_ts_ms,
            end_ts_ms,
            timestamp_unit: "unix_ms".into(),
            timezone: "UTC".into(),
        },
        spec.base.seed,
        behavior,
        execution_model,
        std::collections::BTreeMap::new(),
    )
    .map_err(|error| experiment_identity_invalid(format!("实验 CanonicalRunSpec 无效: {error}")))
}

pub(crate) fn normalize_experiment_identity_values(
    value: &mut Value,
    path: &str,
) -> Result<(), AppError> {
    match value {
        Value::Null | Value::Bool(_) | Value::String(_) => Ok(()),
        Value::Number(number) if number.is_f64() => Err(experiment_identity_invalid(format!(
            "{path} 含浮点值；CanonicalRunSpec 精确数值必须使用十进制文本"
        ))),
        Value::Number(number) => {
            if number
                .as_i64()
                .is_some_and(|integer| integer.unsigned_abs() > 9_007_199_254_740_991)
                || number
                    .as_u64()
                    .is_some_and(|integer| integer > 9_007_199_254_740_991)
            {
                canonicalize_integer(value);
            }
            Ok(())
        }
        Value::Array(values) => {
            for (index, child) in values.iter_mut().enumerate() {
                normalize_experiment_identity_values(child, &format!("{path}[{index}]"))?;
            }
            Ok(())
        }
        Value::Object(fields) => {
            for (name, child) in fields.iter_mut() {
                if name.ends_with("_raw") || name == "seed" {
                    canonicalize_integer(child);
                }
                normalize_experiment_identity_values(child, &format!("{path}.{name}"))?;
            }
            Ok(())
        }
    }
}

fn canonicalize_integer(value: &mut Value) {
    if value.is_number() {
        *value = Value::String(value.to_string());
    }
}

pub(crate) fn quote_unsafe_integer_tokens(json: &str) -> String {
    let bytes = json.as_bytes();
    let mut output = String::with_capacity(json.len());
    let mut segment_start = 0;
    let mut index = 0;
    let mut in_string = false;
    let mut escaped = false;
    while index < bytes.len() {
        let byte = bytes[index];
        if in_string {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                in_string = false;
            }
            index += 1;
            continue;
        }
        if byte == b'"' {
            in_string = true;
            index += 1;
            continue;
        }
        if byte.is_ascii_digit() || byte == b'-' {
            let start = index;
            while index < bytes.len()
                && !matches!(
                    bytes[index],
                    b',' | b']' | b'}' | b':' | b' ' | b'\n' | b'\r' | b'\t'
                )
            {
                index += 1;
            }
            let token = &json[start..index];
            let is_unsafe_integer = !token.bytes().any(|byte| matches!(byte, b'.' | b'e' | b'E'))
                && token
                    .parse::<i128>()
                    .map(|number| number.unsigned_abs() > 9_007_199_254_740_991)
                    .unwrap_or_else(|_| {
                        token
                            .parse::<u128>()
                            .is_ok_and(|number| number > 9_007_199_254_740_991)
                    });
            if is_unsafe_integer {
                output.push_str(&json[segment_start..start]);
                output.push('"');
                output.push_str(token);
                output.push('"');
                segment_start = index;
            }
            continue;
        }
        index += 1;
    }
    output.push_str(&json[segment_start..]);
    output
}

pub(super) fn validate_experiment_run_identity(
    result: &RunExperimentResult,
) -> Result<(), AppError> {
    match (
        result.canonical_run_spec_json.as_deref(),
        result.canonical_run_spec_fingerprint.as_deref(),
    ) {
        (None, None) => Ok(()),
        (Some(payload), Some(expected_fingerprint)) => {
            let canonical = CanonicalRunSpec::from_json(payload)
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            let canonical_json = canonical
                .canonical_json()
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            let fingerprint = canonical
                .fingerprint()
                .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error))?;
            if canonical_json != payload
                || fingerprint != expected_fingerprint
                || canonical.run_type != "experiment"
                || canonical.datasets.len() != 1
                || result.input_data_fingerprint.as_deref()
                    != canonical
                        .datasets
                        .first()
                        .map(|dataset| dataset.content_fingerprint.as_str())
                || result
                    .candidates
                    .iter()
                    .filter_map(|candidate| candidate.outcome.as_ref())
                    .any(|outcome| canonical.datasets[0].instrument != outcome.instrument)
            {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "参数实验 CanonicalRunSpec 与报告数据身份或指纹不一致",
                ));
            }
            Ok(())
        }
        _ => Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "参数实验 CanonicalRunSpec JSON 与 fingerprint 必须同时存在或同时缺省",
        )),
    }
}

pub(super) fn validate_experiment_artifact_envelope(
    result: &RunExperimentResult,
) -> Result<(), AppError> {
    match (
        result.result_hash.as_deref(),
        result.run_manifest.as_deref(),
        result.run_record.as_deref(),
        result.artifact_locations.as_ref(),
    ) {
        (None, None, None, None) => Ok(()),
        (Some(result_hash), Some(manifest), Some(record), Some(locations)) => {
            let is_hash = |value: &str| {
                value.len() == 16
                    && value
                        .bytes()
                        .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
            };
            if !is_hash(result_hash)
                || manifest.trim().is_empty()
                || record.trim().is_empty()
                || result.canonical_run_spec_json.is_none()
                || result.canonical_run_spec_fingerprint.is_none()
                || result.input_data_fingerprint.is_none()
                || !matches!(locations.len(), 2 | 3)
                || (locations.len() == 3
                    && locations
                        .get("experiment_input")
                        .is_none_or(|path| path.trim().is_empty()))
                || locations
                    .get("experiment_report")
                    .is_none_or(|path| path.trim().is_empty())
                || locations.get("run_manifest").map(String::as_str) != Some(manifest)
            {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "参数实验标准产物指针或 ArtifactLocations 无效",
                ));
            }
            if experiment_result_hash(result)? != result_hash {
                return Err(AppError::new(
                    AppErrorCategory::StorageFailure,
                    "参数实验 result_hash 与报告事实不一致",
                ));
            }
            Ok(())
        }
        _ => Err(AppError::new(
            AppErrorCategory::StorageFailure,
            "参数实验 result_hash、RunManifest、RunRecord 和 ArtifactLocations 必须全部存在或全部缺省",
        )),
    }
}

pub(super) fn experiment_result_hash(result: &RunExperimentResult) -> Result<String, AppError> {
    let serialized = serde_json::to_string(result).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验事实摘要编码失败: {error}"),
        )
    })?;
    experiment_result_hash_from_json(&serialized).map_err(|error| {
        AppError::new(
            AppErrorCategory::InternalInvariant,
            format!("参数实验事实摘要规范化失败: {error}"),
        )
    })
}

pub(crate) fn experiment_result_hash_from_value(value: &Value) -> Result<String, String> {
    let payload = serde_json::to_string(value)
        .map_err(|error| format!("参数实验事实摘要编码失败: {error}"))?;
    experiment_result_hash_from_json(&payload)
}

pub(crate) fn experiment_result_hash_from_json(payload: &str) -> Result<String, String> {
    let mut parser = ExperimentJsonParser::new(payload);
    let value = parser.parse_value()?;
    parser.skip_whitespace();
    if parser.cursor != parser.bytes.len() {
        return Err("参数实验 JSON 存在尾随内容".into());
    }
    let mut canonical = String::new();
    value.write_canonical(&mut canonical);
    let mut digest = Fnv1a::new();
    digest.write_bytes(canonical.as_bytes());
    Ok(format!("{:016x}", digest.finish()))
}

#[derive(Debug)]
enum ExperimentJsonValue {
    Raw(String),
    Array(Vec<ExperimentJsonValue>),
    Object(BTreeMap<String, ExperimentJsonValue>),
}

impl ExperimentJsonValue {
    fn write_canonical(&self, output: &mut String) {
        match self {
            Self::Raw(value) => output.push_str(value),
            Self::Array(items) => {
                output.push('[');
                for (index, item) in items.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    item.write_canonical(output);
                }
                output.push(']');
            }
            Self::Object(fields) => {
                output.push('{');
                for (index, (name, value)) in fields.iter().enumerate() {
                    if index != 0 {
                        output.push(',');
                    }
                    write_json_string(name, output);
                    output.push(':');
                    value.write_canonical(output);
                }
                output.push('}');
            }
        }
    }
}

fn write_json_string(value: &str, output: &mut String) {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    output.push('"');
    for character in value.chars() {
        match character {
            '"' => output.push_str("\\\""),
            '\\' => output.push_str("\\\\"),
            '\u{0008}' => output.push_str("\\b"),
            '\u{000c}' => output.push_str("\\f"),
            '\n' => output.push_str("\\n"),
            '\r' => output.push_str("\\r"),
            '\t' => output.push_str("\\t"),
            character if (character as u32) <= 0x1f => {
                let code = character as usize;
                output.push_str("\\u00");
                output.push(HEX[(code >> 4) & 0x0f] as char);
                output.push(HEX[code & 0x0f] as char);
            }
            character => output.push(character),
        }
    }
    output.push('"');
}

struct ExperimentJsonParser<'a> {
    bytes: &'a [u8],
    cursor: usize,
}

impl<'a> ExperimentJsonParser<'a> {
    fn new(payload: &'a str) -> Self {
        Self {
            bytes: payload.as_bytes(),
            cursor: 0,
        }
    }

    fn skip_whitespace(&mut self) {
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(u8::is_ascii_whitespace)
        {
            self.cursor += 1;
        }
    }

    fn parse_value(&mut self) -> Result<ExperimentJsonValue, String> {
        self.skip_whitespace();
        match self.bytes.get(self.cursor).copied() {
            Some(b'{') => self.parse_object(),
            Some(b'[') => self.parse_array(),
            Some(b'"') => self.parse_raw_string(),
            Some(_) => self.parse_raw_scalar(),
            None => Err("参数实验 JSON 意外结束".into()),
        }
    }

    fn parse_object(&mut self) -> Result<ExperimentJsonValue, String> {
        self.cursor += 1;
        let mut fields = BTreeMap::new();
        self.skip_whitespace();
        if self.consume(b'}') {
            return Ok(ExperimentJsonValue::Object(fields));
        }
        loop {
            self.skip_whitespace();
            let key_start = self.cursor;
            let key_end = self.scan_string()?;
            let key: String = serde_json::from_slice(&self.bytes[key_start..key_end])
                .map_err(|error| format!("参数实验 JSON 对象键无效: {error}"))?;
            self.skip_whitespace();
            if !self.consume(b':') {
                return Err("参数实验 JSON 对象缺少冒号".into());
            }
            let value = self.parse_value()?;
            if !is_experiment_location_key(&key) {
                fields.insert(key, value);
            }
            self.skip_whitespace();
            if self.consume(b'}') {
                break;
            }
            if !self.consume(b',') {
                return Err("参数实验 JSON 对象缺少逗号".into());
            }
        }
        Ok(ExperimentJsonValue::Object(fields))
    }

    fn parse_array(&mut self) -> Result<ExperimentJsonValue, String> {
        self.cursor += 1;
        let mut values = Vec::new();
        self.skip_whitespace();
        if self.consume(b']') {
            return Ok(ExperimentJsonValue::Array(values));
        }
        loop {
            values.push(self.parse_value()?);
            self.skip_whitespace();
            if self.consume(b']') {
                break;
            }
            if !self.consume(b',') {
                return Err("参数实验 JSON 数组缺少逗号".into());
            }
        }
        Ok(ExperimentJsonValue::Array(values))
    }

    fn parse_raw_string(&mut self) -> Result<ExperimentJsonValue, String> {
        let start = self.cursor;
        let end = self.scan_string()?;
        let raw = std::str::from_utf8(&self.bytes[start..end])
            .map_err(|error| format!("参数实验 JSON 字符串不是 UTF-8: {error}"))?;
        Ok(ExperimentJsonValue::Raw(raw.to_owned()))
    }

    fn scan_string(&mut self) -> Result<usize, String> {
        if !self.consume(b'"') {
            return Err("参数实验 JSON 对象键必须是字符串".into());
        }
        let mut escaped = false;
        while let Some(byte) = self.bytes.get(self.cursor).copied() {
            self.cursor += 1;
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                return Ok(self.cursor);
            }
        }
        Err("参数实验 JSON 字符串没有结束".into())
    }

    fn parse_raw_scalar(&mut self) -> Result<ExperimentJsonValue, String> {
        let start = self.cursor;
        while self
            .bytes
            .get(self.cursor)
            .is_some_and(|byte| !byte.is_ascii_whitespace() && !matches!(*byte, b',' | b']' | b'}'))
        {
            self.cursor += 1;
        }
        if start == self.cursor {
            return Err("参数实验 JSON 标量为空".into());
        }
        let raw = std::str::from_utf8(&self.bytes[start..self.cursor])
            .map_err(|error| format!("参数实验 JSON 标量不是 UTF-8: {error}"))?;
        Ok(ExperimentJsonValue::Raw(raw.to_owned()))
    }

    fn consume(&mut self, expected: u8) -> bool {
        if self.bytes.get(self.cursor) == Some(&expected) {
            self.cursor += 1;
            true
        } else {
            false
        }
    }
}

fn is_experiment_location_key(name: &str) -> bool {
    matches!(
        name,
        "experiment_id"
            | "spec_fingerprint"
            | "artifact_path"
            | "artifacts"
            | "run_id"
            | "run_manifest"
            | "run_record"
            | "artifact_locations"
            | "result_hash"
            | "selected_run_manifest"
    ) || name.ends_with("_path")
        || name.ends_with("_file")
        || name.ends_with("_dir")
}

fn experiment_identity_invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

#[cfg(test)]
mod canonical_json_string_tests {
    use super::write_json_string;

    #[test]
    fn canonical_json_string_escaping_matches_serde_json() {
        let values = [
            "plain ASCII",
            "quote\" and slash\\",
            "line\nfeed\ttab\rreturn",
            "control\u{0000}\u{001f} boundary",
            "非 ASCII 字符",
        ];
        for value in values {
            let mut actual = String::new();
            write_json_string(value, &mut actual);
            assert_eq!(actual, serde_json::to_string(value).unwrap_or_default());
        }
    }
}
