//! Stable identity construction for single-strategy backtest artifacts.

use std::collections::BTreeMap;

use qx_core::{RunManifest, TradingInstrumentSpec};
use qx_xingban::{BacktestReport, RunManifestIdentity};
use serde_json::{json, Value};

/// Inputs already resolved by the CLI's unique runtime/data read paths.
///
/// Identity composition and hashing live in the application layer so CLI and
/// future non-CLI callers share the same collision-resistant run naming rule.
pub struct StrategyBacktestRunIdentityInput<'a> {
    pub strategy_id: &'a str,
    pub instrument: &'a str,
    pub config_fingerprint: &'a str,
    pub data_fingerprint: &'a str,
    pub input_kind: &'a str,
    pub dataset_id: &'a str,
    pub dataset_version: &'a str,
    pub input_fingerprint: &'a str,
    pub market_spec: Option<&'a TradingInstrumentSpec>,
    pub instrument_spec_version: Option<&'a str>,
    pub dataset_quality: Option<&'a Value>,
    pub model_descriptors: &'a [String],
    pub cost_source: &'a str,
    pub risk_rule_set_version: &'a str,
    pub result_hash: u64,
}

/// Inputs needed to build the durable manifest for an already completed bar run.
/// Runtime version is supplied by the facade because each entry point has its own
/// runtime contract, while manifest composition itself belongs to qx-app.
pub struct BacktestRunManifestInput<'a> {
    pub run_id: &'a str,
    pub code_commit: &'a str,
    pub config_hash: &'a str,
    pub strategy_version: &'a str,
    pub instrument_spec_version: &'a str,
    pub runtime_version: &'a str,
    pub data_fingerprint: &'a str,
    pub input_components: BTreeMap<String, String>,
}

/// Complete identity and manifest metadata for one strategy backtest.
/// Keeping both stages in one application call guarantees that the run ID
/// embedded in the manifest is derived from the same resolved facts.
pub struct StrategyBacktestRunManifestInput<'a> {
    pub identity: StrategyBacktestRunIdentityInput<'a>,
    pub code_commit: &'a str,
    pub strategy_version: &'a str,
    pub instrument_spec_version: &'a str,
    pub runtime_version: &'a str,
    pub input_components: BTreeMap<String, String>,
}

/// Derive a stable strategy run ID and construct its canonical manifest together.
pub fn build_strategy_backtest_run_manifest(
    report: &BacktestReport,
    input: StrategyBacktestRunManifestInput<'_>,
) -> Result<(String, RunManifest), String> {
    let config_hash = input.identity.config_fingerprint;
    let data_fingerprint = input.identity.data_fingerprint;
    let run_id = strategy_backtest_run_id(input.identity)
        .map_err(|error| format!("策略回测运行身份编码失败: {error}"))?;
    let manifest = build_backtest_run_manifest(
        report,
        BacktestRunManifestInput {
            run_id: &run_id,
            code_commit: input.code_commit,
            config_hash,
            strategy_version: input.strategy_version,
            instrument_spec_version: input.instrument_spec_version,
            runtime_version: input.runtime_version,
            data_fingerprint,
            input_components: input.input_components,
        },
    )?;
    Ok((run_id, manifest))
}

/// Assemble a stable RunManifest from a completed engine report.
pub fn build_backtest_run_manifest(
    report: &BacktestReport,
    input: BacktestRunManifestInput<'_>,
) -> Result<RunManifest, String> {
    report.run_manifest_with_input_components(
        RunManifestIdentity {
            run_id: input.run_id,
            code_commit: input.code_commit,
            config_hash: input.config_hash,
            strategy_version: input.strategy_version,
            instrument_spec_version: input.instrument_spec_version,
            runtime_version: input.runtime_version,
        },
        input.data_fingerprint,
        input.input_components,
    )
}

/// Build the immutable run ID from the consumed input and completed result.
/// Locator paths are omitted from dataset quality identity because portable
/// evidence verification may relocate the same immutable dataset.
pub fn strategy_backtest_run_id(
    input: StrategyBacktestRunIdentityInput<'_>,
) -> Result<String, serde_json::Error> {
    let dataset_quality = input.dataset_quality.map(|quality| {
        let mut identity = quality.clone();
        if let Some(fields) = identity.as_object_mut() {
            fields.remove("manifest_path");
        }
        identity
    });
    let cost_source = stable_cost_source_identity(input.cost_source);
    let artifact_identity = json!({
        "schema": "strategy-run-artifact-identity-v1",
        "data_fingerprint": input.data_fingerprint,
        "input": {
            "kind": input.input_kind,
            "dataset_id": input.dataset_id,
            "dataset_version": input.dataset_version,
            "fingerprint": input.input_fingerprint,
        },
        "market_spec": input.market_spec,
        "instrument_spec_version": input.instrument_spec_version,
        "dataset_quality": dataset_quality,
        "model_descriptors": input.model_descriptors,
        "cost_source": cost_source,
        "risk_rule_set_version": input.risk_rule_set_version,
        "result_hash": format!("{:016x}", input.result_hash),
    });
    let identity_hash = qx_strategy::sha256_hex(&serde_json::to_vec(&artifact_identity)?);
    Ok(format!(
        "strategy-backtest:{}:{}:{}:{}",
        input.strategy_id,
        input.instrument,
        input.config_fingerprint,
        &identity_hash[..24],
    ))
}

/// Keep the cost provenance class in run identity while omitting machine-local file locators.
/// Effective fee and latency parameters are already bound by `model_descriptors`; including an
/// absolute path here made a portable EvidenceBundle recompute to a different run ID after its
/// copied runtime dependencies moved under the bundle directory.
fn stable_cost_source_identity(source: &str) -> String {
    if source.starts_with("ashare-rules:") {
        if source.contains("+cost-rules-file:") {
            "ashare-rules+cost-rules-file".into()
        } else {
            "ashare-rules".into()
        }
    } else if source.starts_with("cost-rules-file:") {
        "cost-rules-file".into()
    } else {
        source.to_string()
    }
}

#[cfg(test)]
mod tests {
    use super::{
        stable_cost_source_identity, strategy_backtest_run_id, StrategyBacktestRunIdentityInput,
    };

    #[test]
    fn run_identity_ignores_cost_dependency_locator_but_keeps_source_classes() {
        let identity = |cost_source| {
            strategy_backtest_run_id(StrategyBacktestRunIdentityInput {
                strategy_id: "strategy-ashare",
                instrument: "000001.SZSE",
                config_fingerprint: "config-fingerprint",
                data_fingerprint: "barframe:data-fingerprint",
                input_kind: "barframe",
                dataset_id: "dataset",
                dataset_version: "v1",
                input_fingerprint: "input-fingerprint",
                market_spec: None,
                instrument_spec_version: None,
                dataset_quality: None,
                model_descriptors: &["fee=100;latency=2".into()],
                cost_source,
                risk_rule_set_version: "risk-v1",
                result_hash: 42,
            })
            .unwrap()
        };

        assert_eq!(
            identity("ashare-rules:D:\\runs\\first\\rules.json"),
            identity("ashare-rules:runtime\\rules.json")
        );
        assert_eq!(
            identity("ashare-rules:D:\\runs\\first\\rules.json+cost-rules-file:D:\\costs.json"),
            identity("ashare-rules:runtime\\rules.json+cost-rules-file:runtime\\costs.json")
        );
        assert_ne!(
            stable_cost_source_identity("ashare-rules:rules.json"),
            stable_cost_source_identity("cost-rules-file:rules.json")
        );
    }
}
