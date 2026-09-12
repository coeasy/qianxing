use crate::{
    FactorCatalog, FactorError, FactorExecutionPlan, FactorIncrementalProvenance, FeatureArtifact,
    FeatureDefinition,
};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Append-only interval that an incremental kernel may recompute.
///
/// The lower bound is exclusive because the previous materialization already
/// includes `start_exclusive`; the upper bound is the current PIT cutoff.
#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FactorDirtyRange {
    pub start_exclusive: u64,
    pub end_inclusive: u64,
}

impl FactorDirtyRange {
    pub fn validate(self) -> Result<(), FactorError> {
        if self.start_exclusive >= self.end_inclusive {
            return Err(FactorError::Invalid(
                "factor dirty range 必须满足 start_exclusive < end_inclusive".into(),
            ));
        }
        Ok(())
    }
}

/// Input presented to a concrete factor kernel by the deterministic plan
/// executor. The materializer owns ordering, dependency resolution and cache
/// lineage; the evaluator owns the mathematical formula.
pub struct FactorEvaluationRequest<'a> {
    pub definition: &'a FeatureDefinition,
    pub input_fingerprint: &'a str,
    pub as_of: u64,
    pub dependencies: &'a BTreeMap<String, FeatureArtifact>,
    pub previous_artifact: Option<&'a FeatureArtifact>,
    pub dirty_range: Option<FactorDirtyRange>,
}

pub trait FactorEvaluator {
    fn evaluate(
        &self,
        request: FactorEvaluationRequest<'_>,
    ) -> Result<FeatureArtifact, FactorError>;
}

/// Deterministic cache keyed by `FactorPlanNode::cache_key_digest`.
///
/// A cache key is immutable: inserting a different artifact under an existing
/// digest is an invariant violation rather than last-write-wins replacement.
#[derive(Clone, Default, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FactorArtifactCache {
    entries: BTreeMap<u64, FeatureArtifact>,
}

impl FactorArtifactCache {
    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    pub fn get(&self, cache_key_digest: u64) -> Option<&FeatureArtifact> {
        self.entries.get(&cache_key_digest)
    }

    pub fn insert(
        &mut self,
        cache_key_digest: u64,
        artifact: FeatureArtifact,
    ) -> Result<(), FactorError> {
        artifact.validate()?;
        if cache_key_digest == 0 {
            return Err(FactorError::Invalid(
                "factor materialization cache key 不能为 0".into(),
            ));
        }
        if let Some(existing) = self.entries.get(&cache_key_digest) {
            if existing != &artifact {
                return Err(FactorError::Duplicate(format!(
                    "factor cache key collision: {cache_key_digest}"
                )));
            }
            return Ok(());
        }
        self.entries.insert(cache_key_digest, artifact);
        Ok(())
    }
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FactorMaterializationResult {
    pub plan_digest: u64,
    pub input_fingerprint: String,
    pub as_of: u64,
    pub artifacts: Vec<FeatureArtifact>,
    pub cache_hits: Vec<String>,
    pub computed_nodes: Vec<String>,
    pub incremental: Option<FactorIncrementalProvenance>,
}

impl FactorMaterializationResult {
    pub fn validate_for(&self, plan: &FactorExecutionPlan) -> Result<(), FactorError> {
        plan.validate()?;
        if self.plan_digest != plan.digest()?
            || self.input_fingerprint != plan.input_fingerprint
            || self.as_of != plan.as_of
            || self.artifacts.len() != plan.nodes.len()
        {
            return Err(FactorError::Invalid(
                "factor materialization result 与 execution plan 不一致".into(),
            ));
        }
        for (artifact, node) in self.artifacts.iter().zip(&plan.nodes) {
            artifact.validate()?;
            if artifact.feature_key != node.feature_key
                || artifact.input_fingerprint != plan.input_fingerprint
                || artifact.as_of != plan.as_of
            {
                return Err(FactorError::Invalid(format!(
                    "factor artifact 与 plan node 不一致: {}",
                    node.feature_key
                )));
            }
        }
        Ok(())
    }

    pub fn artifact_for(&self, feature_key: &str) -> Option<&FeatureArtifact> {
        self.artifacts
            .iter()
            .find(|artifact| artifact.feature_key == feature_key)
    }

    pub fn requested_artifacts<'a>(
        &'a self,
        plan: &'a FactorExecutionPlan,
    ) -> Result<Vec<&'a FeatureArtifact>, FactorError> {
        self.validate_for(plan)?;
        plan.requested_outputs
            .iter()
            .map(|key| {
                self.artifact_for(key)
                    .ok_or_else(|| FactorError::MissingArtifact(key.clone()))
            })
            .collect()
    }
}

pub struct FactorMaterializer<'a> {
    catalog: &'a FactorCatalog,
}

impl<'a> FactorMaterializer<'a> {
    pub fn new(catalog: &'a FactorCatalog) -> Self {
        Self { catalog }
    }

    pub fn materialize<E: FactorEvaluator + ?Sized>(
        &self,
        plan: &FactorExecutionPlan,
        evaluator: &E,
        cache: &mut FactorArtifactCache,
    ) -> Result<FactorMaterializationResult, FactorError> {
        self.materialize_with_previous(plan, None, None, evaluator, cache)
    }

    pub fn materialize_incremental<E: FactorEvaluator + ?Sized>(
        &self,
        plan: &FactorExecutionPlan,
        previous_plan: &FactorExecutionPlan,
        previous_result: &FactorMaterializationResult,
        evaluator: &E,
        cache: &mut FactorArtifactCache,
    ) -> Result<FactorMaterializationResult, FactorError> {
        previous_result.validate_for(previous_plan)?;
        let provenance = plan.incremental_against(previous_plan)?;
        self.materialize_with_previous(
            plan,
            Some(previous_result),
            Some(provenance),
            evaluator,
            cache,
        )
    }

    fn materialize_with_previous<E: FactorEvaluator + ?Sized>(
        &self,
        plan: &FactorExecutionPlan,
        previous_result: Option<&FactorMaterializationResult>,
        incremental: Option<FactorIncrementalProvenance>,
        evaluator: &E,
        cache: &mut FactorArtifactCache,
    ) -> Result<FactorMaterializationResult, FactorError> {
        plan.validate()?;
        let dirty_range = incremental
            .as_ref()
            .filter(|provenance| {
                !provenance.full_recompute && provenance.current_as_of > provenance.previous_as_of
            })
            .map(|provenance| FactorDirtyRange {
                start_exclusive: provenance.previous_as_of,
                end_inclusive: provenance.current_as_of,
            });
        if let Some(range) = dirty_range {
            range.validate()?;
        }

        let previous_by_key = previous_result
            .map(|result| {
                result
                    .artifacts
                    .iter()
                    .map(|artifact| (artifact.feature_key.as_str(), artifact))
                    .collect::<BTreeMap<_, _>>()
            })
            .unwrap_or_default();
        let incremental_allowed = incremental
            .as_ref()
            .is_some_and(|provenance| !provenance.full_recompute);

        let mut materialized = BTreeMap::<String, FeatureArtifact>::new();
        let mut artifacts = Vec::with_capacity(plan.nodes.len());
        let mut cache_hits = Vec::new();
        let mut computed_nodes = Vec::new();

        for node in &plan.nodes {
            if let Some(cached) = cache.get(node.cache_key_digest) {
                validate_artifact_for_plan(cached, &node.feature_key, plan)?;
                let cached = cached.clone();
                materialized.insert(node.feature_key.clone(), cached.clone());
                artifacts.push(cached);
                cache_hits.push(node.feature_key.clone());
                continue;
            }

            let definition = self
                .catalog
                .definitions
                .get(&node.feature_key)
                .ok_or_else(|| FactorError::MissingDefinition(node.feature_key.clone()))?;
            definition.validate()?;
            let dependencies = node
                .dependencies
                .iter()
                .map(|dependency| {
                    materialized
                        .get(dependency)
                        .cloned()
                        .map(|artifact| (dependency.clone(), artifact))
                        .ok_or_else(|| FactorError::MissingArtifact(dependency.clone()))
                })
                .collect::<Result<BTreeMap<_, _>, _>>()?;
            let previous_artifact = incremental_allowed
                .then(|| previous_by_key.get(node.feature_key.as_str()).copied())
                .flatten();
            let artifact = evaluator.evaluate(FactorEvaluationRequest {
                definition,
                input_fingerprint: &plan.input_fingerprint,
                as_of: plan.as_of,
                dependencies: &dependencies,
                previous_artifact,
                dirty_range,
            })?;
            validate_artifact_for_plan(&artifact, &node.feature_key, plan)?;
            cache.insert(node.cache_key_digest, artifact.clone())?;
            materialized.insert(node.feature_key.clone(), artifact.clone());
            artifacts.push(artifact);
            computed_nodes.push(node.feature_key.clone());
        }

        let result = FactorMaterializationResult {
            plan_digest: plan.digest()?,
            input_fingerprint: plan.input_fingerprint.clone(),
            as_of: plan.as_of,
            artifacts,
            cache_hits,
            computed_nodes,
            incremental,
        };
        result.validate_for(plan)?;
        Ok(result)
    }
}

fn validate_artifact_for_plan(
    artifact: &FeatureArtifact,
    feature_key: &str,
    plan: &FactorExecutionPlan,
) -> Result<(), FactorError> {
    artifact.validate()?;
    if artifact.feature_key != feature_key
        || artifact.input_fingerprint != plan.input_fingerprint
        || artifact.as_of != plan.as_of
    {
        return Err(FactorError::Invalid(format!(
            "factor evaluator 返回了错误血缘: expected={}@{} actual={}@{}",
            feature_key, plan.as_of, artifact.feature_key, artifact.as_of
        )));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compute_momentum;
    use qx_core::InstrumentId;
    use qx_guanxing::{Bar, DataSourceId, DataView};
    use std::cell::{Cell, RefCell};

    struct MomentumEvaluator {
        instrument: InstrumentId,
        view: DataView,
        lookback: usize,
        calls: Cell<usize>,
        incremental_requests: RefCell<Vec<(Option<u64>, Option<FactorDirtyRange>)>>,
    }

    impl FactorEvaluator for MomentumEvaluator {
        fn evaluate(
            &self,
            request: FactorEvaluationRequest<'_>,
        ) -> Result<FeatureArtifact, FactorError> {
            self.calls.set(self.calls.get() + 1);
            self.incremental_requests.borrow_mut().push((
                request.previous_artifact.map(|artifact| artifact.as_of),
                request.dirty_range,
            ));
            compute_momentum(
                request.definition,
                self.instrument.clone(),
                &self.view,
                request.as_of,
                self.lookback,
                request.input_fingerprint,
            )
        }
    }

    fn fixture() -> (FactorCatalog, MomentumEvaluator) {
        let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
        let view = DataView::try_new(
            vec![
                Bar::new(10, 100, 105, 95, 100, 10),
                Bar::new(20, 105, 115, 100, 110, 10),
                Bar::new(30, 115, 125, 110, 120, 10),
                Bar::new(40, 125, 135, 120, 130, 10),
            ],
            DataSourceId::new("fixture"),
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
        (
            catalog,
            MomentumEvaluator {
                instrument,
                view,
                lookback: 1,
                calls: Cell::new(0),
                incremental_requests: RefCell::new(Vec::new()),
            },
        )
    }

    #[test]
    fn materializer_executes_real_factor_kernel_and_reuses_cache() {
        let (catalog, evaluator) = fixture();
        let plan = catalog
            .compile_execution_plan(&["momentum@v1".into()], "bars-v1", 30)
            .unwrap();
        let materializer = FactorMaterializer::new(&catalog);
        let mut cache = FactorArtifactCache::default();
        let first = materializer
            .materialize(&plan, &evaluator, &mut cache)
            .unwrap();
        assert_eq!(first.computed_nodes, vec!["momentum@v1"]);
        assert!(first.cache_hits.is_empty());
        assert_eq!(evaluator.calls.get(), 1);
        assert_eq!(cache.len(), 1);

        let second = materializer
            .materialize(&plan, &evaluator, &mut cache)
            .unwrap();
        assert_eq!(second.cache_hits, vec!["momentum@v1"]);
        assert!(second.computed_nodes.is_empty());
        assert_eq!(evaluator.calls.get(), 1);
        assert_eq!(first.artifacts, second.artifacts);
    }

    #[test]
    fn incremental_materialization_passes_dirty_range_and_previous_artifact() {
        let (catalog, evaluator) = fixture();
        let previous_plan = catalog
            .compile_execution_plan(&["momentum@v1".into()], "bars-v1", 30)
            .unwrap();
        let current_plan = catalog
            .compile_execution_plan(&["momentum@v1".into()], "bars-v1", 40)
            .unwrap();
        let materializer = FactorMaterializer::new(&catalog);
        let previous = materializer
            .materialize(
                &previous_plan,
                &evaluator,
                &mut FactorArtifactCache::default(),
            )
            .unwrap();
        let current = materializer
            .materialize_incremental(
                &current_plan,
                &previous_plan,
                &previous,
                &evaluator,
                &mut FactorArtifactCache::default(),
            )
            .unwrap();

        let provenance = current.incremental.as_ref().unwrap();
        assert!(!provenance.full_recompute);
        assert_eq!(provenance.dirty_nodes, vec!["momentum@v1"]);
        assert_eq!(
            evaluator.incremental_requests.borrow().last().copied(),
            Some((
                Some(30),
                Some(FactorDirtyRange {
                    start_exclusive: 30,
                    end_inclusive: 40,
                })
            ))
        );
        assert_ne!(previous.artifacts, current.artifacts);
    }

    #[test]
    fn lineage_change_disables_incremental_previous_artifact() {
        let (catalog, evaluator) = fixture();
        let previous_plan = catalog
            .compile_execution_plan(&["momentum@v1".into()], "bars-v1", 30)
            .unwrap();
        let changed_plan = catalog
            .compile_execution_plan(&["momentum@v1".into()], "bars-v2", 40)
            .unwrap();
        let materializer = FactorMaterializer::new(&catalog);
        let previous = materializer
            .materialize(
                &previous_plan,
                &evaluator,
                &mut FactorArtifactCache::default(),
            )
            .unwrap();
        let changed = materializer
            .materialize_incremental(
                &changed_plan,
                &previous_plan,
                &previous,
                &evaluator,
                &mut FactorArtifactCache::default(),
            )
            .unwrap();

        assert!(changed.incremental.as_ref().unwrap().full_recompute);
        assert_eq!(
            evaluator.incremental_requests.borrow().last().copied(),
            Some((None, None))
        );
    }

    #[test]
    fn evaluator_cannot_return_wrong_lineage() {
        struct BadEvaluator;
        impl FactorEvaluator for BadEvaluator {
            fn evaluate(
                &self,
                request: FactorEvaluationRequest<'_>,
            ) -> Result<FeatureArtifact, FactorError> {
                Ok(FeatureArtifact {
                    feature_key: request.definition.key(),
                    input_fingerprint: "wrong".into(),
                    as_of: request.as_of,
                    coverage_bps: 10_000,
                    values: BTreeMap::from([(InstrumentId::parse("BTCUSDT.BINANCE").unwrap(), 1)]),
                })
            }
        }

        let (catalog, _) = fixture();
        let plan = catalog
            .compile_execution_plan(&["momentum@v1".into()], "bars-v1", 30)
            .unwrap();
        assert!(FactorMaterializer::new(&catalog)
            .materialize(&plan, &BadEvaluator, &mut FactorArtifactCache::default())
            .is_err());
    }
}
