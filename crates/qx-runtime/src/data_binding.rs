use crate::StrategyContext;
use qx_core::{Fnv1a, RunManifest};
use qx_data::{DatasetManifest, DatasetRef, DatasetResolver};
use qx_factor::FactorExecutionPlan;
use serde::{Deserialize, Serialize};

/// Resolved dataset identity used by research, strategy and runtime audit paths.
///
/// The reference is the immutable caller-facing identity. The resolved manifest
/// is retained so runtime validation can prove which registry entry satisfied
/// that identity without re-querying an external provider.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RuntimeDatasetBinding {
    pub reference: DatasetRef,
    pub manifest: DatasetManifest,
}

impl RuntimeDatasetBinding {
    pub fn resolve<R: DatasetResolver + ?Sized>(
        resolver: &R,
        reference: DatasetRef,
    ) -> Result<Self, String> {
        reference.validate()?;
        let manifest = resolver.resolve(&reference)?;
        let binding = Self {
            reference,
            manifest,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.reference.validate()?;
        self.manifest.validate()?;
        if self.manifest.dataset_id != self.reference.dataset_id
            || self.manifest.version != self.reference.version
            || self.manifest.fingerprint != self.reference.fingerprint
        {
            return Err("resolved dataset manifest does not match DatasetRef identity".into());
        }
        Ok(())
    }

    pub fn validate_for_run_manifest(&self, run: &RunManifest) -> Result<(), String> {
        self.validate()?;
        run.validate()?;
        if run.data_fingerprint != self.reference.fingerprint {
            return Err(format!(
                "RunManifest data_fingerprint 不匹配: run={} dataset={}",
                run.data_fingerprint, self.reference.fingerprint
            ));
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<u64, String> {
        self.validate()?;
        let mut hash = Fnv1a::new();
        hash.write_text("qx-runtime-dataset-binding-v1");
        hash.write_text(&self.reference.dataset_id);
        hash.write_text(&self.reference.version);
        hash.write_text(&self.reference.fingerprint);
        hash.write_text(&self.manifest.source);
        hash.write_u64(self.manifest.schema_version as u64);
        hash.write_u64(self.manifest.start_timestamp);
        hash.write_u64(self.manifest.end_timestamp);
        Ok(hash.finish())
    }
}

/// Immutable bridge from a resolved dataset to a compiled factor plan.
///
/// This is the V2 hand-off into StrategyContext: Strategy still receives its
/// existing stable contract, while runtime verifies that the research snapshot,
/// compiled factor graph and RunManifest all refer to the same dataset lineage.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct RuntimeResearchBinding {
    pub dataset: RuntimeDatasetBinding,
    pub factor_plan: FactorExecutionPlan,
}

impl RuntimeResearchBinding {
    pub fn new(
        dataset: RuntimeDatasetBinding,
        factor_plan: FactorExecutionPlan,
    ) -> Result<Self, String> {
        let binding = Self {
            dataset,
            factor_plan,
        };
        binding.validate()?;
        Ok(binding)
    }

    pub fn validate(&self) -> Result<(), String> {
        self.dataset.validate()?;
        self.factor_plan
            .validate()
            .map_err(|error| format!("factor execution plan 非法: {error:?}"))?;
        if self.factor_plan.input_fingerprint != self.dataset.reference.fingerprint {
            return Err(format!(
                "factor plan fingerprint 不匹配: plan={} dataset={}",
                self.factor_plan.input_fingerprint, self.dataset.reference.fingerprint
            ));
        }
        if self.factor_plan.as_of < self.dataset.manifest.start_timestamp
            || self.factor_plan.as_of > self.dataset.manifest.end_timestamp
        {
            return Err(format!(
                "factor plan as_of={} 超出 dataset range {}..={}",
                self.factor_plan.as_of,
                self.dataset.manifest.start_timestamp,
                self.dataset.manifest.end_timestamp
            ));
        }
        Ok(())
    }

    pub fn validate_for_strategy_context(&self, context: &StrategyContext) -> Result<(), String> {
        self.validate()?;
        context.validate(context.as_of, false)?;
        if context.data_fingerprint != self.dataset.reference.fingerprint
            || context.research.candidate.config.data_fingerprint
                != self.dataset.reference.fingerprint
        {
            return Err("StrategyContext 与 DatasetRef fingerprint 不一致".into());
        }
        if context.as_of != self.factor_plan.as_of {
            return Err(format!(
                "StrategyContext as_of={} 与 factor plan as_of={} 不一致",
                context.as_of, self.factor_plan.as_of
            ));
        }
        let mut factor_keys = context.research.candidate.factor_keys.clone();
        factor_keys.sort();
        if factor_keys != self.factor_plan.requested_outputs {
            return Err("StrategyContext factor keys 与 ExecutionPlan outputs 不一致".into());
        }
        Ok(())
    }

    pub fn validate_for_run_manifest(&self, run: &RunManifest) -> Result<(), String> {
        self.validate()?;
        self.dataset.validate_for_run_manifest(run)
    }

    pub fn digest(&self) -> Result<u64, String> {
        self.validate()?;
        let plan_digest = self
            .factor_plan
            .digest()
            .map_err(|error| format!("factor execution plan digest 失败: {error:?}"))?;
        let mut hash = Fnv1a::new();
        hash.write_text("qx-runtime-research-binding-v1");
        hash.write_u64(self.dataset.digest()?);
        hash.write_u64(plan_digest);
        Ok(hash.finish())
    }
}

impl StrategyContext {
    /// Verify that this strategy decision context is backed by one resolved,
    /// immutable dataset and the exact factor plan compiled for that dataset.
    pub fn validate_research_binding(
        &self,
        binding: &RuntimeResearchBinding,
    ) -> Result<(), String> {
        binding.validate_for_strategy_context(self)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use qx_core::InstrumentId;
    use qx_data::DatasetRegistry;
    use qx_factor::{
        CandidateRequest, FactorCatalog, FactorReport, FeatureArtifact, FeatureDefinition,
        StrategyResearchSnapshot,
    };
    use std::collections::BTreeMap;

    fn build_fixture() -> (RuntimeResearchBinding, StrategyContext, RunManifest) {
        let mut registry = DatasetRegistry::default();
        registry
            .register(DatasetManifest {
                dataset_id: "bars.daily".into(),
                version: "v1".into(),
                source: "fixture".into(),
                fingerprint: "bars-abc".into(),
                schema_version: 1,
                start_timestamp: 1,
                end_timestamp: 20,
            })
            .unwrap();
        let dataset = RuntimeDatasetBinding::resolve(
            &registry,
            DatasetRef::new("bars.daily", "v1", "bars-abc").unwrap(),
        )
        .unwrap();

        let mut catalog = FactorCatalog::default();
        catalog
            .register_definition(FeatureDefinition {
                name: "momentum".into(),
                version: "v1".into(),
                formula: "close / close[-1] - 1".into(),
                input_fields: vec!["close".into()],
                dependencies: Vec::new(),
                point_in_time: true,
            })
            .unwrap();
        let plan = catalog
            .compile_execution_plan(&["momentum@v1".into()], "bars-abc", 10)
            .unwrap();
        let binding = RuntimeResearchBinding::new(dataset, plan).unwrap();

        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let artifact = FeatureArtifact {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "bars-abc".into(),
            as_of: 10,
            coverage_bps: 10_000,
            values: BTreeMap::from([(instrument.clone(), 25)]),
        };
        catalog.publish_artifact(artifact.clone()).unwrap();
        let report = FactorReport {
            feature_key: "momentum@v1".into(),
            input_fingerprint: "bars-abc".into(),
            observation_hash: 1,
            analysis_start: 1,
            analysis_end: 10,
            sample_count: 1,
            coverage_bps: 10_000,
            ic_bps: 100,
            rank_ic_bps: 100,
            turnover_bps: 50,
            transform: None,
            missing_policy: "reject".into(),
            decay_bps: 0,
            capacity_raw: 1,
            exposures: BTreeMap::new(),
        };
        catalog.publish_report(report.clone()).unwrap();
        let candidate = catalog
            .bind_candidate(CandidateRequest {
                strategy_version: "strategy-v1".into(),
                universe_version: "universe-v1".into(),
                parameters: Default::default(),
                data_fingerprint: "bars-abc".into(),
                factor_keys: vec!["momentum@v1".into()],
                cost_bps: 5,
                train_start: 1,
                train_end: 5,
                validation_start: 6,
                validation_end: 10,
                intended_exposure: BTreeMap::from([(instrument.clone(), 10)]),
                constraints: BTreeMap::new(),
                execution_model: "event-backtest@v1".into(),
                risk_model: "risk@v1".into(),
            })
            .unwrap();
        let context = StrategyContext {
            strategy_id: "strategy-main".into(),
            strategy_version: "strategy-v1".into(),
            data_fingerprint: "bars-abc".into(),
            as_of: 10,
            research: StrategyResearchSnapshot {
                schema_version: StrategyResearchSnapshot::SCHEMA_VERSION,
                candidate,
                artifacts: vec![artifact],
                reports: vec![report],
                as_of: 10,
            },
            account_id: "main".into(),
            venue_id: "paper".into(),
            positions: BTreeMap::new(),
            cash: BTreeMap::from([("USDT".into(), 1_000)]),
            available_margin_raw: Some(1_000),
            risk_state: "verified".into(),
        };
        let run = RunManifest {
            run_id: "run-1".into(),
            code_commit: "commit-1".into(),
            config_hash: "config-1".into(),
            data_fingerprint: "bars-abc".into(),
            input_components: BTreeMap::new(),
            clock_start: 1,
            clock_end: 10,
            global_seed: 7,
            determinism_mode: true,
            result_hash: "result-1".into(),
            strategy_version: "strategy-v1".into(),
            instrument_spec_version: "instrument-v1".into(),
            model_fingerprint: "model-v1".into(),
            input_event_hash: "input-1".into(),
            output_event_hash: "output-1".into(),
            runtime_version: "runtime-v1".into(),
        };
        (binding, context, run)
    }

    #[test]
    fn dataset_factor_strategy_and_run_manifest_share_one_lineage() {
        let (binding, context, run) = build_fixture();
        binding.validate().unwrap();
        context.validate_research_binding(&binding).unwrap();
        binding.validate_for_run_manifest(&run).unwrap();
        assert_ne!(binding.digest().unwrap(), 0);
    }

    #[test]
    fn lineage_mismatch_fails_closed() {
        let (binding, mut context, mut run) = build_fixture();
        context.data_fingerprint = "other-data".into();
        assert!(context.validate_research_binding(&binding).is_err());

        run.data_fingerprint = "other-data".into();
        assert!(binding.validate_for_run_manifest(&run).is_err());
    }

    #[test]
    fn factor_plan_must_match_dataset_time_and_fingerprint() {
        let (binding, _, _) = build_fixture();
        let mut invalid = binding.clone();
        invalid.factor_plan.as_of = 21;
        assert!(invalid.validate().is_err());
        invalid.factor_plan.as_of = 10;
        invalid.factor_plan.input_fingerprint = "other-data".into();
        assert!(invalid.validate().is_err());
    }
}
