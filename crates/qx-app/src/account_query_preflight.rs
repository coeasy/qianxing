//! Fail-closed preflight for private account reads before authenticated connectors start.

use crate::{AppError, AppErrorCategory, CallerCapability, ExecutionProfile, RunContext};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::BTreeSet;

pub const ACCOUNT_QUERY_PREFLIGHT_SCHEMA_VERSION: u32 = 1;
const ACCOUNT_QUERY_OPERATIONS: [&str; 6] = [
    "balance.read",
    "orders.read",
    "positions.read",
    "cashflows.read",
    "funding.read",
    "account_settings.read",
];

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountQueryCapabilityPreflightSpecV1 {
    pub schema_version: u32,
    pub profile: ExecutionProfile,
    pub capability_id: String,
    pub venue: String,
    pub product: String,
    pub account_model: String,
    pub required_operations: Vec<String>,
    pub optional_operations: Vec<String>,
    pub sandbox_endpoint: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
pub struct AccountQueryCapabilityPreflightDecisionV1 {
    pub schema_version: u32,
    pub allowed: bool,
    pub profile: ExecutionProfile,
    pub capability_id: String,
    pub venue: String,
    pub product: String,
    pub account_model: String,
    /// Required operations plus optional operations declared for this exact venue scope.
    pub allowed_operations: Vec<String>,
    /// Optional operations deliberately not called because the scope does not declare them.
    pub omitted_optional_operations: Vec<String>,
    pub scope_status: Option<String>,
    pub maturity_refs: Vec<String>,
    pub evidence_stages: Value,
    pub evidence: Vec<String>,
    pub limitations: Vec<String>,
    pub reason_code: Option<String>,
    pub reason: Option<String>,
}

/// Resolve private account-read requirements against generated product and maturity evidence.
/// This decision is pure and must run before loading credentials or creating a connector.
pub fn evaluate_account_query_capability_preflight_v1(
    spec: &AccountQueryCapabilityPreflightSpecV1,
    catalog: &Value,
    context: &RunContext,
) -> Result<AccountQueryCapabilityPreflightDecisionV1, AppError> {
    let correlation_id = context.correlation_id().to_owned();
    evaluate_inner(spec, catalog, context).map_err(|error| {
        if error.correlation_id().is_some() {
            error
        } else {
            error.with_correlation_id(correlation_id)
        }
    })
}

fn evaluate_inner(
    spec: &AccountQueryCapabilityPreflightSpecV1,
    catalog: &Value,
    context: &RunContext,
) -> Result<AccountQueryCapabilityPreflightDecisionV1, AppError> {
    context.require(CallerCapability::Live, "账户私有查询能力预检")?;
    validate_spec(spec)?;
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
        .get("account_query_scopes")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("能力目录 account_query_scopes 格式无效"))?;
    let Some(scope) = scopes.iter().find(|scope| {
        eq(scope.get("venue"), &spec.venue)
            && eq(scope.get("product"), &spec.product)
            && contains(scope.get("account_models"), &spec.account_model)
    }) else {
        return Ok(denied(
            spec,
            "account_scope_not_declared",
            "交易所、产品或账户体系没有已声明的账户私有查询范围",
        ));
    };
    let scope_status = scope
        .get("status")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if !matches!(
        scope_status,
        "implemented-and-verified" | "implemented-unverified"
    ) {
        return Ok(denied(
            spec,
            "scope_status_not_executable",
            "该账户查询范围的产品状态不允许执行",
        )
        .with_scope(scope_status));
    }
    let maturity_refs = string_list(scope.get("maturity_refs"))
        .ok_or_else(|| invalid("账户查询范围缺少有效 maturity_refs"))?;
    if maturity_refs.is_empty() {
        return Err(invalid("账户查询范围 maturity_refs 不能为空"));
    }
    let declared_operations = string_list(scope.get("operations"))
        .ok_or_else(|| invalid("账户查询范围缺少有效 operations"))?
        .into_iter()
        .collect::<BTreeSet<_>>();
    let missing = spec
        .required_operations
        .iter()
        .filter(|operation| !declared_operations.contains(*operation))
        .cloned()
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Ok(denied(
            spec,
            "required_operation_not_declared",
            &format!("账户查询范围未声明必需操作: {}", missing.join(", ")),
        )
        .with_scope(scope_status)
        .with_maturity_refs(maturity_refs));
    }

    let maturity_records = capability
        .get("maturity_evidence")
        .and_then(Value::as_array)
        .ok_or_else(|| invalid("能力目录缺少 maturity_evidence"))?;
    let required_stage = match spec.profile {
        ExecutionProfile::SandboxAcceptance => "code_tested",
        ExecutionProfile::Production => "production_approved",
    };
    let mut evidence_stages = serde_json::Map::new();
    let mut evidence =
        string_list(scope.get("evidence")).ok_or_else(|| invalid("账户查询范围缺少 evidence"))?;
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
        if !stages
            .get("implementation")
            .is_some_and(|value| value == true)
        {
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
    evidence.sort();
    evidence.dedup();
    let optional_operations = spec
        .optional_operations
        .iter()
        .cloned()
        .collect::<BTreeSet<_>>();
    let mut allowed_operations = spec.required_operations.clone();
    allowed_operations.extend(
        optional_operations
            .intersection(&declared_operations)
            .cloned(),
    );
    allowed_operations.sort();
    let omitted_optional_operations = optional_operations
        .difference(&declared_operations)
        .cloned()
        .collect::<Vec<_>>();
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
    Ok(AccountQueryCapabilityPreflightDecisionV1 {
        schema_version: ACCOUNT_QUERY_PREFLIGHT_SCHEMA_VERSION,
        allowed: true,
        profile: spec.profile,
        capability_id: spec.capability_id.clone(),
        venue: spec.venue.clone(),
        product: spec.product.clone(),
        account_model: spec.account_model.clone(),
        allowed_operations,
        omitted_optional_operations,
        scope_status: Some(scope_status.to_owned()),
        maturity_refs,
        evidence_stages: Value::Object(evidence_stages),
        evidence,
        limitations,
        reason_code: None,
        reason: None,
    })
}

fn validate_spec(spec: &AccountQueryCapabilityPreflightSpecV1) -> Result<(), AppError> {
    if spec.schema_version != ACCOUNT_QUERY_PREFLIGHT_SCHEMA_VERSION {
        return Err(invalid("账户查询预检版本不受支持"));
    }
    for (field, value) in [
        ("capability_id", &spec.capability_id),
        ("venue", &spec.venue),
        ("product", &spec.product),
        ("account_model", &spec.account_model),
    ] {
        if value.trim().is_empty() {
            return Err(invalid(format!("{field} 不能为空")));
        }
    }
    if spec.venue != spec.venue.trim().to_ascii_uppercase()
        || spec.product != spec.product.trim().to_ascii_lowercase()
        || spec.account_model != spec.account_model.trim().to_ascii_lowercase()
    {
        return Err(invalid(
            "venue 必须大写，product 与 account_model 必须小写并去除首尾空白",
        ));
    }
    if spec.required_operations.is_empty() {
        return Err(invalid("required_operations 不能为空"));
    }
    let required = validate_operations(&spec.required_operations, "required_operations")?;
    let optional = validate_operations(&spec.optional_operations, "optional_operations")?;
    if !required.is_disjoint(&optional) {
        return Err(invalid("必需和可选账户查询操作不能重复"));
    }
    Ok(())
}

fn validate_operations(values: &[String], field: &str) -> Result<BTreeSet<String>, AppError> {
    let mut result = BTreeSet::new();
    for value in values {
        if !ACCOUNT_QUERY_OPERATIONS.contains(&value.as_str()) || !result.insert(value.clone()) {
            return Err(invalid(format!(
                "{field} 含有未知或重复的账户查询操作: {value}"
            )));
        }
    }
    Ok(result)
}

fn string_list(value: Option<&Value>) -> Option<Vec<String>> {
    value?
        .as_array()?
        .iter()
        .map(|item| item.as_str().map(str::to_owned))
        .collect()
}

fn contains(value: Option<&Value>, expected: &str) -> bool {
    string_list(value).is_some_and(|values| {
        values
            .iter()
            .any(|value| value.eq_ignore_ascii_case(expected))
    })
}

fn eq(value: Option<&Value>, expected: &str) -> bool {
    value
        .and_then(Value::as_str)
        .is_some_and(|value| value.eq_ignore_ascii_case(expected.trim()))
}

fn denied(
    spec: &AccountQueryCapabilityPreflightSpecV1,
    code: &str,
    reason: &str,
) -> AccountQueryCapabilityPreflightDecisionV1 {
    AccountQueryCapabilityPreflightDecisionV1 {
        schema_version: ACCOUNT_QUERY_PREFLIGHT_SCHEMA_VERSION,
        allowed: false,
        profile: spec.profile,
        capability_id: spec.capability_id.clone(),
        venue: spec.venue.clone(),
        product: spec.product.clone(),
        account_model: spec.account_model.clone(),
        allowed_operations: Vec::new(),
        omitted_optional_operations: spec.optional_operations.clone(),
        scope_status: None,
        maturity_refs: Vec::new(),
        evidence_stages: Value::Object(serde_json::Map::new()),
        evidence: Vec::new(),
        limitations: Vec::new(),
        reason_code: Some(code.to_owned()),
        reason: Some(reason.to_owned()),
    }
}

impl AccountQueryCapabilityPreflightDecisionV1 {
    fn with_scope(mut self, status: &str) -> Self {
        self.scope_status = Some(status.to_owned());
        self
    }

    fn with_maturity_refs(mut self, refs: Vec<String>) -> Self {
        self.maturity_refs = refs;
        self
    }

    pub fn require_allowed(&self, context: &RunContext) -> Result<(), AppError> {
        context.require(CallerCapability::Live, "账户私有查询能力预检")?;
        if self.allowed {
            return Ok(());
        }
        let mut error = AppError::new(
            AppErrorCategory::FidelityInsufficient,
            format!(
                "账户查询能力预检拒绝 [{}]: {}",
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn spec(
        profile: ExecutionProfile,
        sandbox_endpoint: bool,
    ) -> AccountQueryCapabilityPreflightSpecV1 {
        AccountQueryCapabilityPreflightSpecV1 {
            schema_version: ACCOUNT_QUERY_PREFLIGHT_SCHEMA_VERSION,
            profile,
            capability_id: "qx.venue.binance_spot".into(),
            venue: "BINANCE".into(),
            product: "spot".into(),
            account_model: "spot".into(),
            required_operations: vec!["balance.read".into(), "orders.read".into()],
            optional_operations: vec!["cashflows.read".into()],
            sandbox_endpoint,
        }
    }

    fn catalog() -> Value {
        json!({
            "schema_version": 1,
            "capabilities": [{
                "id": "qx.venue.binance_spot",
                "matrix": {"limitations": ["sandbox conformance pending"]},
                "account_query_scopes": [{
                    "venue": "BINANCE",
                    "product": "spot",
                    "account_models": ["spot"],
                    "operations": ["balance.read", "orders.read"],
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
        RunContext::new(CallerCapability::Live, "account-query-preflight-test")
    }

    #[test]
    fn sandbox_query_preflight_returns_only_declared_operations() {
        let decision = evaluate_account_query_capability_preflight_v1(
            &spec(ExecutionProfile::SandboxAcceptance, true),
            &catalog(),
            &live_context(),
        )
        .unwrap();
        assert!(decision.allowed);
        assert_eq!(decision.allowed_operations, ["balance.read", "orders.read"]);
        assert_eq!(decision.omitted_optional_operations, ["cashflows.read"]);
        assert_eq!(
            decision.evidence_stages["binance_direct"]["sandbox_tested"],
            false
        );
    }

    #[test]
    fn required_private_query_not_in_scope_is_denied() {
        let mut request = spec(ExecutionProfile::SandboxAcceptance, true);
        request.required_operations.push("positions.read".into());
        let decision =
            evaluate_account_query_capability_preflight_v1(&request, &catalog(), &live_context())
                .unwrap();
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("required_operation_not_declared")
        );
        let error = decision
            .require_allowed(&live_context())
            .expect_err("未声明的必需账户查询必须拒绝");
        assert_eq!(error.reason_code(), Some("required_operation_not_declared"));
    }

    #[test]
    fn production_query_requires_approval_evidence() {
        let decision = evaluate_account_query_capability_preflight_v1(
            &spec(ExecutionProfile::Production, false),
            &catalog(),
            &live_context(),
        )
        .unwrap();
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("evidence_floor_not_met")
        );
    }

    #[test]
    fn sandbox_query_rejects_a_mainnet_endpoint() {
        let decision = evaluate_account_query_capability_preflight_v1(
            &spec(ExecutionProfile::SandboxAcceptance, false),
            &catalog(),
            &live_context(),
        )
        .unwrap();
        assert_eq!(
            decision.reason_code.as_deref(),
            Some("sandbox_endpoint_required")
        );
    }

    #[test]
    fn research_context_cannot_preflight_private_account_access() {
        let context = RunContext::new(CallerCapability::Research, "account-query-preflight-test");
        let error = evaluate_account_query_capability_preflight_v1(
            &spec(ExecutionProfile::SandboxAcceptance, true),
            &catalog(),
            &context,
        )
        .expect_err("research cannot preflight live account access");
        assert_eq!(error.category(), AppErrorCategory::PermissionDenied);
    }
}
