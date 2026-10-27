use crate::{AppError, AppErrorCategory, CallerCapability, RunContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;

pub const EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION: u32 = 1;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ExecutionProfile {
    SandboxAcceptance,
    Production,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCapabilityPreflightSpecV1 {
    pub schema_version: u32,
    pub profile: ExecutionProfile,
    pub capability_id: String,
    pub venue: String,
    pub product: String,
    pub account_mode: String,
    pub account_model: String,
    pub margin_mode: String,
    pub position_mode: String,
    pub order_type: String,
    pub quantity_unit: String,
    pub side: String,
    pub market_rule_fingerprint: String,
    pub sandbox_endpoint: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionCapabilityPreflightDecisionV1 {
    pub schema_version: u32,
    pub allowed: bool,
    pub profile: ExecutionProfile,
    pub capability_id: String,
    pub scope_status: Option<String>,
    pub maturity_refs: Vec<String>,
    pub evidence_stages: Value,
    pub evidence: Vec<String>,
    pub limitations: Vec<String>,
    pub market_rule_fingerprint: String,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
}

impl ExecutionCapabilityPreflightSpecV1 {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            invalid(format!(
                "ExecutionCapabilityPreflightSpecV1 JSON 无效: {error}"
            ))
        })?;
        if spec.schema_version != EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "ExecutionCapabilityPreflightSpecV1 schema_version={} 本版本只认 {}",
                spec.schema_version, EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION
            )));
        }
        Ok(spec)
    }
}

/// Resolve the requested live order shape against the generated product catalog and its linked
/// maturity evidence. This is a pure decision: it never loads credentials or contacts a venue.
pub fn evaluate_execution_capability_preflight_v1(
    spec: &ExecutionCapabilityPreflightSpecV1,
    catalog: &Value,
    context: &RunContext,
) -> Result<ExecutionCapabilityPreflightDecisionV1, AppError> {
    evaluate_operation_capability_preflight_v1(spec, catalog, context, "submit")
}

/// Resolve a cancellation against the same exact venue/product/account/order scope as submit,
/// while requiring an independently declared `cancel` operation in that scope.
pub fn evaluate_cancel_execution_capability_preflight_v1(
    spec: &ExecutionCapabilityPreflightSpecV1,
    catalog: &Value,
    context: &RunContext,
) -> Result<ExecutionCapabilityPreflightDecisionV1, AppError> {
    evaluate_operation_capability_preflight_v1(spec, catalog, context, "cancel")
}

fn evaluate_operation_capability_preflight_v1(
    spec: &ExecutionCapabilityPreflightSpecV1,
    catalog: &Value,
    context: &RunContext,
    operation: &str,
) -> Result<ExecutionCapabilityPreflightDecisionV1, AppError> {
    let correlation_id = context.correlation_id().to_owned();
    let result = evaluate_inner(spec, catalog, context, operation);
    result.map_err(|error| attach(error, &correlation_id))
}

fn evaluate_inner(
    spec: &ExecutionCapabilityPreflightSpecV1,
    catalog: &Value,
    context: &RunContext,
    operation: &str,
) -> Result<ExecutionCapabilityPreflightDecisionV1, AppError> {
    context.require(CallerCapability::Live, "真实执行能力预检")?;
    validate_spec(spec)?;
    if !matches!(operation, "submit" | "cancel") {
        return Err(invalid("执行操作只支持 submit 或 cancel"));
    }
    if catalog.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(invalid("能力目录 schema_version 不受支持"));
    }
    let capabilities = catalog
        .get("capabilities")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("能力目录缺少 capabilities 数组"))?;
    let Some(capability) = capabilities
        .iter()
        .find(|entry| entry.get("id").and_then(Value::as_str) == Some(&spec.capability_id))
    else {
        return Ok(denied(spec, "capability_not_found", "稳定能力 ID 不存在"));
    };
    let scopes = capability
        .get("execution_scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("能力目录 execution_scopes 格式无效"))?;
    let Some(scope) = scopes.iter().find(|scope| scope_matches(scope, spec)) else {
        return Ok(denied(
            spec,
            "scope_not_declared",
            "交易所、产品、账户模式、账户体系、保证金模式、持仓模式或委托形状未被精确声明",
        ));
    };
    let scope_status = scope
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let operation_declared = scope
        .get("operations")
        .and_then(Value::as_array)
        .is_some_and(|operations| {
            operations.iter().any(|value| {
                value
                    .as_str()
                    .is_some_and(|value| value.eq_ignore_ascii_case(operation))
            })
        });
    if !operation_declared {
        return Ok(denied(
            spec,
            "operation_not_declared",
            &format!("该精确执行范围未声明 {operation} 操作"),
        )
        .with_scope(scope_status));
    }
    if !matches!(
        scope_status,
        "implemented-and-verified" | "implemented-unverified"
    ) {
        return Ok(denied(
            spec,
            "scope_status_not_executable",
            "该能力范围的产品状态不允许执行",
        )
        .with_scope(scope_status));
    }
    let maturity_refs = scope
        .get("maturity_refs")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("能力范围缺少 maturity_refs"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    if maturity_refs.is_empty() {
        return Err(invalid("能力范围 maturity_refs 不能为空"));
    }
    let maturity_records = capability
        .get("maturity_evidence")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("能力目录缺少 maturity_evidence"))?;
    let mut evidence_stages = serde_json::Map::new();
    let mut evidence = scope
        .get("evidence")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("能力范围缺少 evidence"))?
        .iter()
        .filter_map(Value::as_str)
        .map(str::to_owned)
        .collect::<Vec<_>>();
    let required_stage = match spec.profile {
        // Sandbox acceptance must be able to run before sandbox evidence exists. The sandbox
        // endpoint is mandatory and the run records the resulting vendor evidence separately.
        ExecutionProfile::SandboxAcceptance => "code_tested",
        ExecutionProfile::Production => "production_approved",
    };
    for maturity_ref in &maturity_refs {
        let Some(record) = maturity_records
            .iter()
            .find(|record| record.get("id").and_then(Value::as_str) == Some(maturity_ref))
        else {
            return Ok(denied(
                spec,
                "maturity_evidence_missing",
                &format!("能力目录缺少成熟度证据 {maturity_ref}"),
            )
            .with_scope(scope_status)
            .with_maturity_refs(maturity_refs));
        };
        let stages = record
            .get("stages")
            .and_then(Value::as_object)
            .ok_or_else(|| invalid("成熟度 stages 格式无效"))?;
        let implementation = stages
            .get("implementation")
            .is_some_and(|value| value == true);
        if !implementation {
            return Ok(denied(
                spec,
                "implementation_not_evidenced",
                &format!("成熟度证据 {maturity_ref} 未达到 implementation"),
            )
            .with_scope(scope_status)
            .with_maturity_refs(maturity_refs));
        }
        if !stages
            .get(required_stage)
            .is_some_and(|value| value == true)
        {
            return Ok(denied(
                spec,
                "evidence_floor_not_met",
                &format!("成熟度证据 {maturity_ref} 未达到 {required_stage}"),
            )
            .with_scope(scope_status)
            .with_maturity_refs(maturity_refs));
        }
        evidence_stages.insert(maturity_ref.clone(), Value::Object(stages.clone()));
        if let Some(paths) = record.get("evidence").and_then(Value::as_array) {
            evidence.extend(paths.iter().filter_map(Value::as_str).map(str::to_owned));
        }
    }
    evidence.sort();
    evidence.dedup();

    if spec.profile == ExecutionProfile::SandboxAcceptance && !spec.sandbox_endpoint {
        return Ok(denied(
            spec,
            "sandbox_endpoint_required",
            "sandbox_acceptance 只能连接经配置确认的 sandbox/testnet endpoint",
        )
        .with_scope(scope_status)
        .with_maturity_refs(maturity_refs));
    }
    if spec.profile == ExecutionProfile::Production && spec.sandbox_endpoint {
        return Ok(denied(
            spec,
            "production_endpoint_mismatch",
            "production profile 与 sandbox endpoint 不匹配",
        )
        .with_scope(scope_status)
        .with_maturity_refs(maturity_refs));
    }

    let limitations = capability
        .get("matrix")
        .and_then(|matrix| matrix.get("limitations"))
        .and_then(Value::as_array)
        .map(|values| {
            values
                .iter()
                .filter_map(Value::as_str)
                .map(str::to_owned)
                .collect()
        })
        .unwrap_or_default();
    Ok(ExecutionCapabilityPreflightDecisionV1 {
        schema_version: EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
        allowed: true,
        profile: spec.profile,
        capability_id: spec.capability_id.clone(),
        scope_status: Some(scope_status.to_owned()),
        maturity_refs,
        evidence_stages: Value::Object(evidence_stages),
        evidence,
        limitations,
        market_rule_fingerprint: spec.market_rule_fingerprint.clone(),
        reason_code: None,
        reason: None,
    })
}

fn scope_matches(scope: &Value, spec: &ExecutionCapabilityPreflightSpecV1) -> bool {
    eq(scope.get("venue"), &spec.venue)
        && eq(scope.get("product"), &spec.product)
        && eq(scope.get("account_mode"), &spec.account_mode)
        && contains(scope.get("account_models"), &spec.account_model)
        && contains(scope.get("margin_modes"), &spec.margin_mode)
        && contains(scope.get("position_modes"), &spec.position_mode)
        && scope
            .get("order_shapes")
            .and_then(Value::as_array)
            .is_some_and(|shapes| {
                shapes.iter().any(|shape| {
                    eq(shape.get("order_type"), &spec.order_type)
                        && eq(shape.get("quantity_unit"), &spec.quantity_unit)
                        && eq(shape.get("side"), &spec.side)
                })
            })
}

fn validate_spec(spec: &ExecutionCapabilityPreflightSpecV1) -> Result<(), AppError> {
    if spec.schema_version != EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION {
        return Err(invalid("ExecutionCapabilityPreflightSpecV1 版本不受支持"));
    }
    for (field, value) in [
        ("capability_id", &spec.capability_id),
        ("venue", &spec.venue),
        ("product", &spec.product),
        ("account_mode", &spec.account_mode),
        ("account_model", &spec.account_model),
        ("margin_mode", &spec.margin_mode),
        ("position_mode", &spec.position_mode),
        ("order_type", &spec.order_type),
        ("quantity_unit", &spec.quantity_unit),
        ("side", &spec.side),
    ] {
        if value.trim().is_empty() {
            return Err(invalid(format!("{field} 不能为空")));
        }
    }
    let fingerprint = spec.market_rule_fingerprint.as_str();
    if fingerprint.len() != 64 || !fingerprint.bytes().all(|byte| byte.is_ascii_hexdigit()) {
        return Err(invalid(
            "market_rule_fingerprint 必须是 64 位 SHA-256 十六进制值",
        ));
    }
    if !matches!(spec.order_type.as_str(), "market" | "limit")
        || !matches!(spec.quantity_unit.as_str(), "base" | "contracts" | "quote")
        || !matches!(spec.side.as_str(), "buy" | "sell")
    {
        return Err(invalid("订单类型、数量单位或方向不受支持"));
    }
    Ok(())
}

fn eq(value: Option<&Value>, expected: &str) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected.trim()))
}

fn contains(value: Option<&Value>, expected: &str) -> bool {
    value.and_then(Value::as_array).is_some_and(|values| {
        values.iter().any(|value| {
            value
                .as_str()
                .is_some_and(|value| value.eq_ignore_ascii_case(expected.trim()))
        })
    })
}

fn denied(
    spec: &ExecutionCapabilityPreflightSpecV1,
    code: &str,
    reason: &str,
) -> ExecutionCapabilityPreflightDecisionV1 {
    ExecutionCapabilityPreflightDecisionV1 {
        schema_version: EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
        allowed: false,
        profile: spec.profile,
        capability_id: spec.capability_id.clone(),
        scope_status: None,
        maturity_refs: Vec::new(),
        evidence_stages: Value::Object(serde_json::Map::new()),
        evidence: Vec::new(),
        limitations: Vec::new(),
        market_rule_fingerprint: spec.market_rule_fingerprint.clone(),
        reason_code: Some(code.to_owned()),
        reason: Some(reason.to_owned()),
    }
}

impl ExecutionCapabilityPreflightDecisionV1 {
    fn with_scope(mut self, status: &str) -> Self {
        self.scope_status = Some(status.to_owned());
        self
    }

    fn with_maturity_refs(mut self, refs: Vec<String>) -> Self {
        self.maturity_refs = refs;
        self
    }

    pub fn require_allowed(&self, context: &RunContext) -> Result<(), AppError> {
        context.require(CallerCapability::Live, "真实执行能力预检")?;
        if self.allowed {
            return Ok(());
        }
        let mut error = AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!(
                "执行能力预检拒绝 [{}]: {}",
                self.reason_code.as_deref().unwrap_or("unknown"),
                self.reason.as_deref().unwrap_or("未满足能力范围或证据门槛")
            ),
        )
        .with_correlation_id(context.correlation_id());
        if let Some(reason_code) = self.reason_code.as_deref() {
            error = error.with_reason_code(reason_code);
        }
        Err(error)
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(
        profile: ExecutionProfile,
        sandbox_endpoint: bool,
    ) -> ExecutionCapabilityPreflightSpecV1 {
        ExecutionCapabilityPreflightSpecV1 {
            schema_version: EXECUTION_CAPABILITY_PREFLIGHT_SCHEMA_VERSION,
            profile,
            capability_id: "qx.venue.binance_spot".into(),
            venue: "BINANCE".into(),
            product: "spot".into(),
            account_mode: "cash".into(),
            account_model: "spot".into(),
            margin_mode: "cash".into(),
            position_mode: "not_applicable".into(),
            order_type: "market".into(),
            quantity_unit: "base".into(),
            side: "buy".into(),
            market_rule_fingerprint: "a".repeat(64),
            sandbox_endpoint,
        }
    }

    fn catalog() -> Value {
        json!({
            "schema_version": 1,
            "capabilities": [{
                "id": "qx.venue.binance_spot",
                "matrix": {"limitations": ["sandbox result is not verified"]},
                "execution_scopes": [{
                    "venue": "BINANCE",
                    "product": "spot",
                    "account_mode": "cash",
                    "account_models": ["spot"],
                    "margin_modes": ["cash"],
                    "position_modes": ["not_applicable"],
                    "operations": ["submit", "cancel"],
                    "order_shapes": [{
                        "order_type": "market",
                        "quantity_unit": "base",
                        "side": "buy"
                    }],
                    "maturity_refs": ["binance_direct"],
                    "status": "implemented-unverified",
                    "evidence": ["crates/qx-adapter/src/binance.rs"]
                }],
                "maturity_evidence": [{
                    "id": "binance_direct",
                    "stages": {
                        "implementation": true,
                        "code_tested": true,
                        "sandbox_tested": false,
                        "production_approved": false
                    },
                    "evidence": ["deploy/testnet-config.json"]
                }]
            }]
        })
    }

    fn live_context() -> RunContext {
        RunContext::new(CallerCapability::Live, "preflight-test")
    }

    #[test]
    fn sandbox_acceptance_allows_only_declared_code_tested_scope() {
        let decision = evaluate_execution_capability_preflight_v1(
            &spec(ExecutionProfile::SandboxAcceptance, true),
            &catalog(),
            &live_context(),
        )
        .expect("well-formed preflight");

        assert!(decision.allowed);
        assert_eq!(decision.maturity_refs, ["binance_direct"]);
        assert_eq!(
            decision.evidence_stages["binance_direct"]["sandbox_tested"],
            false
        );
    }

    #[test]
    fn sandbox_acceptance_rejects_mainnet_endpoint() {
        let decision = evaluate_execution_capability_preflight_v1(
            &spec(ExecutionProfile::SandboxAcceptance, false),
            &catalog(),
            &live_context(),
        )
        .expect("well-formed preflight");

        assert_eq!(
            decision.reason_code.as_deref(),
            Some("sandbox_endpoint_required")
        );
    }

    #[test]
    fn production_remains_blocked_until_approval_evidence_exists() {
        let decision = evaluate_execution_capability_preflight_v1(
            &spec(ExecutionProfile::Production, false),
            &catalog(),
            &live_context(),
        )
        .expect("well-formed preflight");

        assert_eq!(
            decision.reason_code.as_deref(),
            Some("evidence_floor_not_met")
        );
    }

    #[test]
    fn undeclared_side_and_order_shape_fail_closed() {
        let mut request = spec(ExecutionProfile::SandboxAcceptance, true);
        request.side = "sell".into();
        let decision =
            evaluate_execution_capability_preflight_v1(&request, &catalog(), &live_context())
                .expect("well-formed preflight");

        assert_eq!(decision.reason_code.as_deref(), Some("scope_not_declared"));

        let error = decision
            .require_allowed(&live_context())
            .expect_err("未声明 scope 必须拒绝执行");
        assert_eq!(error.reason_code(), Some("scope_not_declared"));
    }

    #[test]
    fn cancel_requires_its_own_operation_declaration() {
        let mut catalog = catalog();
        catalog["capabilities"][0]["execution_scopes"][0]["operations"] = json!(["submit"]);
        let request = spec(ExecutionProfile::SandboxAcceptance, true);

        let submit =
            evaluate_execution_capability_preflight_v1(&request, &catalog, &live_context())
                .expect("submit scope is declared");
        let cancel =
            evaluate_cancel_execution_capability_preflight_v1(&request, &catalog, &live_context())
                .expect("well-formed cancellation preflight");

        assert!(submit.allowed);
        assert_eq!(
            cancel.reason_code.as_deref(),
            Some("operation_not_declared")
        );

        catalog["capabilities"][0]["execution_scopes"][0]["operations"] =
            json!(["submit", "cancel"]);
        let cancel =
            evaluate_cancel_execution_capability_preflight_v1(&request, &catalog, &live_context())
                .expect("cancel scope is explicitly declared");
        assert!(cancel.allowed);
    }

    #[test]
    fn non_live_context_cannot_evaluate_real_execution() {
        let context = RunContext::new(CallerCapability::Research, "preflight-test");
        let error = evaluate_execution_capability_preflight_v1(
            &spec(ExecutionProfile::SandboxAcceptance, true),
            &catalog(),
            &context,
        )
        .expect_err("research callers cannot preflight live execution");

        assert_eq!(error.category(), AppErrorCategory::PermissionDenied);
    }
}
