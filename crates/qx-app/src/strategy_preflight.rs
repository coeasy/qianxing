//! Shared preflight for file-backed Python strategies.
//!
//! Python remains responsible for loading user modules and hashing their source bundle. This
//! module owns the execution contract: declared parameters, search bounds, market requirements,
//! and Rust-validated Bar inputs. Entry points must not run a strategy after a complete manifest
//! fails this preflight.

use crate::{
    validate_dataset, AppError, AppErrorCategory, CallerCapability, DatasetSpec, RunContext,
};
use serde::{Deserialize, Serialize};
use serde_json::{Map, Value};

pub const STRATEGY_EXECUTION_PREFLIGHT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum StrategyPreflightPurpose {
    Run,
    Search,
    Diagnose,
}

#[derive(Clone, Debug, PartialEq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyExecutionPreflightSpecV1 {
    pub schema_version: u32,
    pub strategy_manifest: Value,
    pub execution_spec: Value,
    pub purpose: StrategyPreflightPurpose,
    #[serde(default)]
    pub target_strategy_id: Option<String>,
    #[serde(default)]
    pub split_plan: Option<Value>,
}

impl StrategyExecutionPreflightSpecV1 {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            AppError::new(
                AppErrorCategory::InvalidInput,
                format!("StrategyExecutionPreflightSpecV1 JSON 无效: {error}"),
            )
        })?;
        if spec.schema_version != STRATEGY_EXECUTION_PREFLIGHT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "StrategyExecutionPreflightSpecV1 schema_version={} 本版本只认 {}",
                spec.schema_version, STRATEGY_EXECUTION_PREFLIGHT_SCHEMA_VERSION
            )));
        }
        Ok(spec)
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyPreflightMarketV1 {
    pub label: String,
    pub instrument: String,
    pub rows: u64,
    pub content_fingerprint: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyExecutionPreflightResultV1 {
    pub schema_version: u32,
    pub strategy_id: String,
    pub strategy_version: String,
    pub completeness: String,
    pub data_level: Option<String>,
    pub warmup_bars: Option<u64>,
    pub checked_markets: Vec<StrategyPreflightMarketV1>,
}

/// Validate a normalized strategy manifest against an execution/search specification.
///
/// Partial manifests preserve legacy strategy behavior. Complete manifests fail closed and
/// validate every selected market dataset through the same `ValidateDataset` application use
/// case used by the CLI, API, and Python SDK.
pub fn validate_strategy_execution_preflight_v1(
    spec: &StrategyExecutionPreflightSpecV1,
    context: &RunContext,
) -> Result<StrategyExecutionPreflightResultV1, AppError> {
    let correlation_id = context.correlation_id().to_owned();
    let result = validate_strategy_execution_preflight_inner(spec, context)
        .map_err(|error| attach(error, &correlation_id));
    result
}

fn validate_strategy_execution_preflight_inner(
    spec: &StrategyExecutionPreflightSpecV1,
    context: &RunContext,
) -> Result<StrategyExecutionPreflightResultV1, AppError> {
    context.require(CallerCapability::Research, "策略执行预检")?;
    if spec.schema_version != STRATEGY_EXECUTION_PREFLIGHT_SCHEMA_VERSION {
        return Err(invalid(
            "StrategyExecutionPreflightSpecV1 schema_version 不受支持",
        ));
    }
    let manifest = spec
        .strategy_manifest
        .as_object()
        .ok_or_else(|| invalid("strategy_manifest 必须是对象"))?;
    for key in manifest.keys() {
        if !matches!(
            key.as_str(),
            "schema_version"
                | "kind"
                | "strategy_id"
                | "strategy_version"
                | "entrypoint"
                | "parameters"
                | "instrument_requirements"
                | "data_level"
                | "warmup_bars"
                | "dependencies"
                | "completeness"
                | "source_sha256"
                | "bundle_sha256"
        ) {
            return Err(invalid(format!(
                "strategy_manifest has unknown field {key:?}"
            )));
        }
    }
    if manifest.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid("strategy_manifest.schema_version 必须为 1"));
    }
    let strategy_id = required_nonempty_string(manifest, "strategy_id")?;
    let strategy_version = required_nonempty_string(manifest, "strategy_version")?;
    if manifest.get("entrypoint").and_then(Value::as_str) != Some("create_strategy(spec)") {
        return Err(invalid("strategy_manifest.entrypoint 不受支持"));
    }
    let dependencies = manifest
        .get("dependencies")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("strategy_manifest.dependencies 必须是数组"))?;
    let mut dependency_names = std::collections::BTreeSet::new();
    for dependency in dependencies {
        let dependency = dependency
            .as_str()
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| invalid("strategy_manifest.dependencies 只能包含非空路径"))?;
        if !dependency_names.insert(dependency) {
            return Err(invalid("strategy_manifest.dependencies 不得重复"));
        }
    }
    let completeness = required_nonempty_string(manifest, "completeness")?;
    if !matches!(completeness.as_str(), "complete" | "partial") {
        return Err(invalid(
            "strategy_manifest.completeness 仅支持 complete 或 partial",
        ));
    }
    let data_level = manifest
        .get("data_level")
        .and_then(Value::as_str)
        .map(str::to_owned);
    let warmup_bars = manifest.get("warmup_bars").and_then(Value::as_u64);
    if completeness == "partial" {
        return Ok(StrategyExecutionPreflightResultV1 {
            schema_version: STRATEGY_EXECUTION_PREFLIGHT_SCHEMA_VERSION,
            strategy_id,
            strategy_version,
            completeness,
            data_level,
            warmup_bars,
            checked_markets: Vec::new(),
        });
    }

    if manifest.get("kind").and_then(Value::as_str) != Some("qianxing-python-strategy-manifest") {
        return Err(invalid("strategy_manifest.kind 不受支持"));
    }
    if matches!(data_level.as_deref(), Some("tick" | "orderbook")) {
        return validate_depth_strategy_preflight(
            spec,
            manifest,
            strategy_id,
            strategy_version,
            completeness,
            data_level,
            warmup_bars,
            context,
        );
    }
    if data_level.as_deref() != Some("bar") {
        return Err(invalid(format!(
            "strategy requires data_level={:?}, but supported levels are bar, tick, and orderbook",
            data_level
        )));
    }
    let warmup_bars =
        warmup_bars.ok_or_else(|| invalid("complete manifest warmup_bars 必须是非负整数"))?;
    let minimum_rows = warmup_bars
        .checked_add(1)
        .ok_or_else(|| invalid("warmup_bars 超出支持范围"))?;

    let definitions = manifest
        .get("parameters")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("complete manifest parameters 必须是对象"))?;
    validate_parameter_definitions(definitions)?;
    validate_parameter_values(
        definitions,
        &spec.execution_spec,
        spec.target_strategy_id.as_deref(),
    )?;
    let check_search_space = match spec.purpose {
        StrategyPreflightPurpose::Run | StrategyPreflightPurpose::Diagnose => false,
        StrategyPreflightPurpose::Search => true,
    };
    if check_search_space {
        validate_search_space(
            definitions,
            &spec.execution_spec,
            spec.target_strategy_id.as_deref(),
        )?;
    }

    let requirements = manifest
        .get("instrument_requirements")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("complete manifest instrument_requirements 必须是对象"))?;
    for key in requirements.keys() {
        if !matches!(
            key.as_str(),
            "asset_classes" | "products" | "venues" | "requires_explicit_spec"
        ) {
            return Err(invalid(format!(
                "instrument_requirements has unknown field {key:?}"
            )));
        }
    }
    let asset_classes = requirement_values(requirements, "asset_classes")?;
    let products = requirement_values(requirements, "products")?;
    let venues = requirement_values(requirements, "venues")?;
    let requires_explicit_spec = requirements
        .get("requires_explicit_spec")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid("instrument_requirements.requires_explicit_spec 必须是布尔值"))?;

    let targets = execution_targets(&spec.execution_spec, spec.target_strategy_id.as_deref())?;
    let mut checked_markets = Vec::with_capacity(targets.len());
    for (index, (label, market)) in targets.iter().enumerate() {
        let instrument = market
            .get("instrument")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| invalid(format!("{label}.instrument 必须是非空字符串")))?;
        let market_spec = market.get("market_spec");
        let has_explicit_spec = market_spec
            .and_then(Value::as_object)
            .is_some_and(|value| !value.is_empty());
        if requires_explicit_spec && !has_explicit_spec {
            return Err(invalid(format!(
                "{label} ({instrument}) requires an explicit market_spec"
            )));
        }
        let (actual_product, actual_venue) = market_spec_identity(market_spec, instrument);
        if !products.is_empty() {
            let product = actual_product.as_deref().ok_or_else(|| {
                invalid(format!(
                    "{label} ({instrument}) cannot prove required product; provide market_spec"
                ))
            })?;
            if !products.contains(product) {
                let requirements: Vec<_> = products.iter().cloned().collect();
                return Err(invalid(format!(
                    "{label} ({instrument}) product {product:?} is outside manifest requirements {requirements:?}"
                )));
            }
        }
        if !venues.is_empty() {
            let venue = actual_venue.as_deref().ok_or_else(|| {
                invalid(format!(
                    "{label} ({instrument}) cannot prove required venue; provide venue in market_spec"
                ))
            })?;
            if !venues.contains(venue) {
                let requirements: Vec<_> = venues.iter().cloned().collect();
                return Err(invalid(format!(
                    "{label} ({instrument}) venue {venue:?} is outside manifest requirements {requirements:?}"
                )));
            }
        }
        if !asset_classes.is_empty() {
            let asset_class = market
                .get("asset_class")
                .and_then(Value::as_str)
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    invalid(format!(
                        "{label} ({instrument}) must declare asset_class to check manifest requirements"
                    ))
                })?
                .trim()
                .to_ascii_lowercase();
            if !asset_classes.contains(&asset_class) {
                let requirements: Vec<_> = asset_classes.iter().cloned().collect();
                return Err(invalid(format!(
                    "{label} ({instrument}) asset_class {asset_class:?} is outside manifest requirements {requirements:?}"
                )));
            }
        }
        let bars_path = market
            .get("bars_path")
            .and_then(Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| invalid(format!("{label} ({instrument}) requires bars_path")))?;
        let verdict = validate_dataset(
            &DatasetSpec::new(format!("strategy-preflight-{}", index + 1), bars_path),
            context,
        )?;
        if verdict.instrument != instrument {
            return Err(invalid(format!(
                "{label} ({instrument}) Bar dataset instrument does not match the market (found {})",
                verdict.instrument
            )));
        }
        if !verdict.usable || verdict.rows < minimum_rows {
            return Err(AppError::new(
                AppErrorCategory::FidelityInsufficient,
                format!(
                    "{label} ({instrument}) needs at least {minimum_rows} validated Bars for warmup_bars={warmup_bars} and one trading decision; found {}",
                    verdict.rows
                ),
            ));
        }
        checked_markets.push(StrategyPreflightMarketV1 {
            label: label.clone(),
            instrument: instrument.to_owned(),
            rows: verdict.rows,
            content_fingerprint: verdict.content_fingerprint,
        });
    }

    if let Some(split_plan) = spec.split_plan.as_ref() {
        let train_bars = split_plan
            .get("train_bars")
            .and_then(Value::as_u64)
            .ok_or_else(|| invalid("split_plan.train_bars 必须是非负整数"))?;
        if train_bars < minimum_rows {
            return Err(invalid(format!(
                "split_plan.train_bars must be at least {minimum_rows} to include warmup_bars={warmup_bars} and one training decision"
            )));
        }
    }

    Ok(StrategyExecutionPreflightResultV1 {
        schema_version: STRATEGY_EXECUTION_PREFLIGHT_SCHEMA_VERSION,
        strategy_id,
        strategy_version,
        completeness,
        data_level,
        warmup_bars: Some(warmup_bars),
        checked_markets,
    })
}

/// Validate the strategy contract for a single L1/L2 run. The depth engine owns
/// parsing and validating the market-data frame when the actual run starts; this
/// preflight validates the file-backed strategy's declared level, tier, config,
/// and any instrument requirements that can be proven from the explicit spec.
fn validate_depth_strategy_preflight(
    spec: &StrategyExecutionPreflightSpecV1,
    manifest: &Map<String, Value>,
    strategy_id: String,
    strategy_version: String,
    completeness: String,
    data_level: Option<String>,
    warmup_bars: Option<u64>,
    _context: &RunContext,
) -> Result<StrategyExecutionPreflightResultV1, AppError> {
    let execution = spec
        .execution_spec
        .as_object()
        .ok_or_else(|| invalid("depth strategy execution_spec 必须是对象"))?;
    let depth_spec = execution
        .get("base")
        .and_then(Value::as_object)
        .unwrap_or(execution);
    if execution.get("schema_version").and_then(Value::as_u64) != Some(1)
        || depth_spec.get("schema_version").and_then(Value::as_u64) != Some(1)
    {
        return Err(invalid(
            "depth strategy execution_spec.schema_version 必须是 1",
        ));
    }
    if depth_spec.get("depth_path").is_none() {
        return Err(invalid(format!(
            "strategy requires data_level={:?}, but this command accepts Bar data only",
            data_level
        )));
    }
    if depth_spec
        .get("depth_path")
        .and_then(Value::as_str)
        .is_none_or(|path| path.trim().is_empty())
    {
        return Err(invalid(
            "depth strategy execution_spec.depth_path 必须是非空字符串",
        ));
    }
    let expected_tier = match data_level.as_deref() {
        Some("tick") => "l1",
        Some("orderbook") => "l2",
        _ => {
            return Err(invalid(
                "depth strategy data_level 仅支持 tick 或 orderbook",
            ));
        }
    };
    if depth_spec.get("tier").and_then(Value::as_str) != Some(expected_tier) {
        return Err(invalid(format!(
            "strategy data_level={:?} requires depth tier={expected_tier}",
            data_level
        )));
    }
    let warmup_bars =
        warmup_bars.ok_or_else(|| invalid("complete manifest warmup_bars 必须是非负整数"))?;
    if warmup_bars != 0 {
        return Err(invalid(
            "history_requirements.warmup_bars 只适用于 Bar 策略；Tick/OrderBook 策略请在钩子中维护事件预热状态",
        ));
    }

    let definitions = manifest
        .get("parameters")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("complete manifest parameters 必须是对象"))?;
    validate_parameter_definitions(definitions)?;
    validate_parameter_values(
        definitions,
        &spec.execution_spec,
        spec.target_strategy_id.as_deref(),
    )?;
    if spec.purpose == StrategyPreflightPurpose::Search {
        validate_search_space(
            definitions,
            &spec.execution_spec,
            spec.target_strategy_id.as_deref(),
        )?;
    }

    let requirements = manifest
        .get("instrument_requirements")
        .and_then(Value::as_object)
        .ok_or_else(|| invalid("complete manifest instrument_requirements 必须是对象"))?;
    for key in requirements.keys() {
        if !matches!(
            key.as_str(),
            "asset_classes" | "products" | "venues" | "requires_explicit_spec"
        ) {
            return Err(invalid(format!(
                "instrument_requirements has unknown field {key:?}"
            )));
        }
    }
    let asset_classes = requirement_values(requirements, "asset_classes")?;
    let products = requirement_values(requirements, "products")?;
    let venues = requirement_values(requirements, "venues")?;
    let requires_explicit_spec = requirements
        .get("requires_explicit_spec")
        .and_then(Value::as_bool)
        .ok_or_else(|| invalid("instrument_requirements.requires_explicit_spec 必须是布尔值"))?;
    let market_spec = depth_spec.get("market_spec");
    let has_explicit_spec = market_spec
        .and_then(Value::as_object)
        .is_some_and(|value| !value.is_empty());
    if requires_explicit_spec && !has_explicit_spec {
        return Err(invalid("depth strategy requires an explicit market_spec"));
    }
    if !asset_classes.is_empty() {
        return Err(invalid(
            "DepthBacktestSpec does not declare asset_class; remove asset_classes from this strategy manifest or add an explicit asset-class field to the depth spec",
        ));
    }
    let (actual_product, actual_venue) = market_spec_identity(market_spec, "");
    if !products.is_empty() {
        let product = actual_product.as_deref().ok_or_else(|| {
            invalid("depth strategy cannot prove required product; provide market_spec")
        })?;
        if !products.contains(product) {
            return Err(invalid(format!(
                "depth strategy product {product:?} is outside manifest requirements {:?}",
                products.iter().cloned().collect::<Vec<_>>()
            )));
        }
    }
    if !venues.is_empty() {
        let venue = actual_venue.as_deref().ok_or_else(|| {
            invalid("depth strategy cannot prove required venue; provide market_spec")
        })?;
        if !venues.contains(venue) {
            return Err(invalid(format!(
                "depth strategy venue {venue:?} is outside manifest requirements {:?}",
                venues.iter().cloned().collect::<Vec<_>>()
            )));
        }
    }

    Ok(StrategyExecutionPreflightResultV1 {
        schema_version: STRATEGY_EXECUTION_PREFLIGHT_SCHEMA_VERSION,
        strategy_id,
        strategy_version,
        completeness,
        data_level,
        warmup_bars: Some(warmup_bars),
        // Depth file parsing and frame identity are revalidated by the Rust
        // depth engine during execution, rather than by Bar ValidateDataset.
        checked_markets: Vec::new(),
    })
}

fn validate_parameter_definitions(definitions: &Map<String, Value>) -> Result<(), AppError> {
    for (name, definition) in definitions {
        if name.trim().is_empty() || name.trim() != name {
            return Err(invalid("parameter names must be non-empty and trimmed"));
        }
        let definition = definition
            .as_object()
            .ok_or_else(|| invalid(format!("parameter {name:?} definition must be an object")))?;
        for key in definition.keys() {
            if !matches!(
                key.as_str(),
                "type" | "required" | "default" | "minimum" | "maximum" | "choices"
            ) {
                return Err(invalid(format!(
                    "parameter {name:?} has unknown field {key:?}"
                )));
            }
        }
        let parameter_type = definition
            .get("type")
            .and_then(Value::as_str)
            .ok_or_else(|| invalid(format!("parameter {name:?}.type 缺失或无效")))?;
        if !matches!(parameter_type, "integer" | "number" | "string" | "boolean") {
            return Err(invalid(format!("parameter {name:?}.type 不受支持")));
        }
        if definition
            .get("required")
            .is_some_and(|value| !value.is_boolean())
        {
            return Err(invalid(format!("parameter {name:?}.required 必须是布尔值")));
        }
        for bound in ["minimum", "maximum"] {
            if let Some(value) = definition.get(bound) {
                if !matches!(parameter_type, "integer" | "number") || !value.is_number() {
                    return Err(invalid(format!(
                        "parameter {name:?}.{bound} 必须是数值参数边界"
                    )));
                }
                if parameter_type == "integer" && number_i128(value).is_none() {
                    return Err(invalid(format!(
                        "integer parameter {name:?}.{bound} 必须是整数"
                    )));
                }
            }
        }
        if let (Some(minimum), Some(maximum)) =
            (definition.get("minimum"), definition.get("maximum"))
        {
            if number_cmp(minimum, maximum).is_some_and(|ordering| ordering.is_gt()) {
                return Err(invalid(format!("parameter {name:?}.minimum 大于 maximum")));
            }
        }
        if let Some(choices) = definition.get("choices") {
            let choices = choices
                .as_array()
                .filter(|values| !values.is_empty())
                .ok_or_else(|| invalid(format!("parameter {name:?}.choices 必须是非空数组")))?;
            for choice in choices {
                validate_parameter_value(name, definition, choice, false)?;
            }
        }
        if let Some(default) = definition.get("default") {
            validate_parameter_value(name, definition, default, false)?;
        }
        let required = definition.get("required").and_then(Value::as_bool) == Some(true);
        if required
            && !definition.contains_key("default")
            && !["minimum", "maximum", "choices"]
                .iter()
                .any(|key| definition.contains_key(*key))
        {
            return Err(invalid(format!(
                "required parameter {name:?} needs a default, bound, or choices"
            )));
        }
    }
    Ok(())
}

fn validate_parameter_values(
    definitions: &Map<String, Value>,
    spec: &Value,
    target_strategy_id: Option<&str>,
) -> Result<(), AppError> {
    let candidates = parameter_candidates(spec, target_strategy_id);
    let empty = Map::new();
    let candidates = if candidates.is_empty() {
        vec![&empty]
    } else {
        candidates
    };
    for candidate in candidates {
        for (name, definition) in definitions {
            let definition = definition.as_object().expect("validated definition");
            if definition.get("required").and_then(Value::as_bool) == Some(true)
                && !definition.contains_key("default")
                && !candidate.contains_key(name)
            {
                return Err(invalid(format!(
                    "required strategy parameter {name:?} is missing"
                )));
            }
        }
        for (name, value) in candidate {
            if let Some(definition) = definitions.get(name).and_then(Value::as_object) {
                validate_parameter_value(name, definition, value, true)?;
            }
        }
    }
    Ok(())
}

fn validate_parameter_value(
    name: &str,
    definition: &Map<String, Value>,
    value: &Value,
    check_choices: bool,
) -> Result<(), AppError> {
    let parameter_type = definition
        .get("type")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let valid_type = match parameter_type {
        "integer" => number_i128(value).is_some(),
        "number" => value.as_f64().is_some_and(f64::is_finite),
        "string" => value.is_string(),
        "boolean" => value.is_boolean(),
        _ => false,
    };
    if !valid_type {
        return Err(invalid(format!(
            "parameter {name:?} must have type {parameter_type}"
        )));
    }
    for (key, label) in [("minimum", "below minimum"), ("maximum", "above maximum")] {
        let Some(bound) = definition.get(key) else {
            continue;
        };
        let compared = number_cmp(value, bound);
        let violates = match (key, compared) {
            ("minimum", Some(ordering)) => ordering.is_lt(),
            ("maximum", Some(ordering)) => ordering.is_gt(),
            _ => false,
        };
        if violates {
            return Err(invalid(format!("parameter {name:?} is {label} {bound}")));
        }
    }
    if check_choices {
        if let Some(choices) = definition.get("choices").and_then(Value::as_array) {
            if !choices.iter().any(|choice| json_value_equal(value, choice)) {
                return Err(invalid(format!(
                    "parameter {name:?} is not one of its declared choices"
                )));
            }
        }
    }
    Ok(())
}

fn validate_search_space(
    definitions: &Map<String, Value>,
    spec: &Value,
    target_strategy_id: Option<&str>,
) -> Result<(), AppError> {
    let mut dimensions: Vec<(String, Value, Vec<Value>)> = Vec::new();
    for key in ["callback_parameter_space", "parameter_space"] {
        let Some(value) = spec.get(key) else { continue };
        let entries = value
            .as_array()
            .ok_or_else(|| invalid(format!("{key} 必须是数组")))?;
        for entry in entries {
            let object = entry
                .as_object()
                .ok_or_else(|| invalid(format!("{key} 条目必须是对象")))?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid(format!("{key} 条目缺少 name")))?;
            let values = object
                .get("values")
                .and_then(Value::as_array)
                .filter(|items| !items.is_empty())
                .ok_or_else(|| invalid(format!("{key} 参数 {name:?} 需要非空 values")))?;
            dimensions.push((name.to_owned(), entry.clone(), values.clone()));
        }
    }
    if let Some(search) = spec.get("search").and_then(Value::as_object) {
        let continuous = search
            .get("continuous_parameter_space")
            .map_or(Ok(&[][..]), |value| {
                value
                    .as_array()
                    .map(Vec::as_slice)
                    .ok_or_else(|| invalid("search.continuous_parameter_space 必须是数组"))
            })?;
        for entry in continuous {
            let object = entry
                .as_object()
                .ok_or_else(|| invalid("continuous search 维度必须是对象"))?;
            let name = object
                .get("name")
                .and_then(Value::as_str)
                .ok_or_else(|| invalid("continuous search 维度缺少 name"))?;
            let minimum = object
                .get("min")
                .ok_or_else(|| invalid(format!("continuous parameter {name:?} 缺少 min")))?;
            let maximum = object
                .get("max")
                .ok_or_else(|| invalid(format!("continuous parameter {name:?} 缺少 max")))?;
            if !minimum.is_number()
                || !maximum.is_number()
                || number_cmp(minimum, maximum).is_some_and(|ordering| ordering.is_gt())
            {
                return Err(invalid(format!("continuous parameter {name:?} 的范围无效")));
            }
            if let Some(step) = object.get("step") {
                if !step
                    .as_f64()
                    .is_some_and(|value| value.is_finite() && value > 0.0)
                {
                    return Err(invalid(format!(
                        "continuous parameter {name:?}.step 必须为正有限数"
                    )));
                }
            }
            dimensions.push((
                name.to_owned(),
                entry.clone(),
                vec![minimum.clone(), maximum.clone()],
            ));
        }
    }

    let mut tuned_names = std::collections::BTreeSet::new();
    for (name, dimension, values) in dimensions {
        let definition = definitions
            .get(&name)
            .and_then(Value::as_object)
            .ok_or_else(|| {
                invalid(format!(
                    "search parameter {name:?} 未在 STRATEGY_MANIFEST 声明"
                ))
            })?;
        tuned_names.insert(name.clone());
        for value in &values {
            validate_parameter_value(&name, definition, value, true)?;
        }
        if dimension.get("step").is_some()
            && definition.get("type").and_then(Value::as_str) == Some("integer")
            && number_i128(&dimension["step"]).is_none()
        {
            return Err(invalid(format!(
                "continuous integer parameter {name:?}.step 必须是整数"
            )));
        }
    }

    let fixed_names: std::collections::BTreeSet<String> =
        parameter_candidates(spec, target_strategy_id)
            .into_iter()
            .flat_map(|candidate| candidate.keys().cloned())
            .collect();
    let missing: Vec<String> = definitions
        .iter()
        .filter_map(|(name, definition)| {
            let definition = definition.as_object()?;
            (definition.get("required").and_then(Value::as_bool) == Some(true)
                && !definition.contains_key("default")
                && !tuned_names.contains(name)
                && !fixed_names.contains(name))
            .then(|| name.clone())
        })
        .collect();
    if !missing.is_empty() {
        return Err(invalid(format!("search space 未提供必需参数: {missing:?}")));
    }
    Ok(())
}

fn parameter_candidates<'a>(
    spec: &'a Value,
    target_strategy_id: Option<&str>,
) -> Vec<&'a Map<String, Value>> {
    let mut candidates = Vec::new();
    let mut scopes = vec![spec];
    if let Some(base) = spec.get("base").and_then(Value::as_object) {
        // A map borrowed from `spec` remains valid for the lifetime of this call.
        let _ = base;
        scopes.push(&spec["base"]);
    }
    for scope in scopes {
        if let Some(parameters) = scope.get("parameters").and_then(Value::as_object) {
            candidates.push(parameters);
        }
        if let Some(strategy) = scope.get("strategy").and_then(Value::as_object) {
            candidates.push(strategy);
        }
        if let Some(markets) = scope.get("markets").and_then(Value::as_array) {
            for market in markets {
                let Some(slots) = market.get("strategies").and_then(Value::as_array) else {
                    continue;
                };
                for slot in slots {
                    let Some(strategy) = slot.get("strategy").and_then(Value::as_object) else {
                        continue;
                    };
                    if target_strategy_id.is_none_or(|target| {
                        strategy.get("strategy_id").and_then(Value::as_str) == Some(target)
                    }) {
                        candidates.push(strategy);
                    }
                }
            }
        }
    }
    candidates
}

fn execution_targets<'a>(
    spec: &'a Value,
    target_strategy_id: Option<&str>,
) -> Result<Vec<(String, &'a Value)>, AppError> {
    if let Some(base) = spec.get("base") {
        return execution_targets(base, target_strategy_id);
    }
    if let Some(markets) = spec.get("markets").and_then(Value::as_array) {
        let mut selected = Vec::new();
        let mut matching_slots = 0usize;
        for (market_index, market) in markets.iter().enumerate() {
            let slots = market
                .get("strategies")
                .and_then(Value::as_array)
                .ok_or_else(|| invalid(format!("markets[{market_index}].strategies 必须是数组")))?;
            let mut selected_market = false;
            for (slot_index, slot) in slots.iter().enumerate() {
                let strategy =
                    slot.get("strategy")
                        .and_then(Value::as_object)
                        .ok_or_else(|| {
                            invalid(format!(
                            "markets[{market_index}].strategies[{slot_index}].strategy 必须是对象"
                        ))
                        })?;
                let id = strategy.get("strategy_id").and_then(Value::as_str).filter(|value| !value.trim().is_empty()).ok_or_else(|| invalid(format!("markets[{market_index}].strategies[{slot_index}].strategy.strategy_id 缺失")))?;
                if target_strategy_id.is_none_or(|target| id == target) {
                    matching_slots += 1;
                    selected_market = true;
                }
            }
            if selected_market {
                selected.push((format!("markets[{market_index}]"), market));
            }
        }
        if matching_slots == 0 {
            return Err(invalid(if let Some(target) = target_strategy_id {
                format!("策略 ID {target:?} 未匹配任何多市场策略槽")
            } else {
                "多市场规格没有策略槽".to_owned()
            }));
        }
        return Ok(selected);
    }
    if spec.get("bars_path").and_then(Value::as_str).is_some() {
        return Ok(vec![(
            spec.get("instrument")
                .and_then(Value::as_str)
                .unwrap_or("backtest")
                .to_owned(),
            spec,
        )]);
    }
    Err(invalid(
        "complete strategy preflight 需要 BacktestSpec 或多市场规格",
    ))
}

fn market_spec_identity(
    market_spec: Option<&Value>,
    instrument: &str,
) -> (Option<String>, Option<String>) {
    let object = market_spec.and_then(Value::as_object);
    let mut product = object
        .and_then(|value| value.get("product"))
        .and_then(Value::as_str)
        .map(str::to_owned);
    if product.is_none() {
        if let Some(market_type) = object
            .and_then(|value| value.get("market_type"))
            .and_then(Value::as_str)
        {
            product = Some(match market_type.to_ascii_lowercase().as_str() {
                "swap" | "perpetual" => "perpetual".into(),
                "future" | "futures" => "future".into(),
                "options" => "option".into(),
                other => other.to_owned(),
            });
        }
    }
    let venue = object
        .and_then(|value| value.get("exchange_id").or_else(|| value.get("venue")))
        .and_then(Value::as_str)
        .or_else(|| {
            object
                .and_then(|value| value.get("instrument"))
                .and_then(Value::as_object)
                .and_then(|value| value.get("venue"))
                .and_then(Value::as_str)
        })
        .or_else(|| instrument.rsplit_once('.').map(|(_, venue)| venue));
    (
        product.map(|value| value.trim().to_ascii_lowercase()),
        venue.map(|value| value.trim().to_ascii_lowercase()),
    )
}

fn requirement_values(
    requirements: &Map<String, Value>,
    key: &str,
) -> Result<std::collections::BTreeSet<String>, AppError> {
    let Some(values) = requirements.get(key) else {
        return Ok(Default::default());
    };
    let values = values
        .as_array()
        .ok_or_else(|| invalid(format!("instrument_requirements.{key} 必须是数组")))?;
    values
        .iter()
        .map(|value| {
            value
                .as_str()
                .filter(|value| !value.trim().is_empty())
                .map(|value| value.trim().to_ascii_lowercase())
                .ok_or_else(|| invalid(format!("instrument_requirements.{key} 只能包含非空字符串")))
        })
        .collect()
}

fn required_nonempty_string(object: &Map<String, Value>, key: &str) -> Result<String, AppError> {
    object
        .get(key)
        .and_then(Value::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(str::to_owned)
        .ok_or_else(|| invalid(format!("strategy_manifest.{key} 必须是非空字符串")))
}

fn number_i128(value: &Value) -> Option<i128> {
    value.as_number()?.to_string().parse().ok()
}

fn number_cmp(left: &Value, right: &Value) -> Option<std::cmp::Ordering> {
    if let (Some(left), Some(right)) = (number_i128(left), number_i128(right)) {
        return Some(left.cmp(&right));
    }
    left.as_f64()?.partial_cmp(&right.as_f64()?)
}

fn json_value_equal(left: &Value, right: &Value) -> bool {
    match (left, right) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        _ => left == right,
    }
}

fn invalid(message: impl Into<String>) -> AppError {
    AppError::new(AppErrorCategory::InvalidInput, message)
}

fn attach(error: AppError, correlation_id: &str) -> AppError {
    if error.correlation_id().is_some() {
        error
    } else {
        error.with_correlation_id(correlation_id)
    }
}
