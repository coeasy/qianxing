use crate::{FactorCatalog, FactorError, FeatureDefinition};
use qx_core::Fnv1a;
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const FACTOR_EXECUTION_PLAN_SCHEMA_VERSION: u32 = 1;

/// A single materialization step compiled from the immutable factor catalog.
///
/// `cache_key_digest` binds the factor definition, the input dataset fingerprint,
/// the PIT cutoff, and the cache keys of all dependencies. Workers can therefore
/// share one compiled plan without relying on process-local map iteration order.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FactorPlanNode {
    pub feature_key: String,
    pub definition_digest: u64,
    pub dependencies: Vec<String>,
    pub cache_key_digest: u64,
}

/// Deterministic, point-in-time execution plan for one or more requested factors.
///
/// Nodes are stored in dependency-before-dependent order. Shared dependencies
/// occur only once, so the plan is also the canonical common-subexpression
/// boundary for factor materialization workers.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FactorExecutionPlan {
    pub schema_version: u32,
    pub requested_outputs: Vec<String>,
    pub input_fingerprint: String,
    pub as_of: u64,
    pub nodes: Vec<FactorPlanNode>,
}

/// Auditable comparison between two plans.
///
/// `full_recompute=false` does not mean no work is required. An advancing PIT
/// cutoff with unchanged lineage marks the affected nodes dirty while allowing
/// a worker with an incremental kernel to update them from prior state.
#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct FactorIncrementalProvenance {
    pub previous_plan_digest: u64,
    pub current_plan_digest: u64,
    pub previous_as_of: u64,
    pub current_as_of: u64,
    pub dirty_nodes: Vec<String>,
    pub full_recompute: bool,
    pub reason: String,
}

impl FactorExecutionPlan {
    pub fn validate(&self) -> Result<(), FactorError> {
        if self.schema_version != FACTOR_EXECUTION_PLAN_SCHEMA_VERSION {
            return Err(FactorError::Invalid(format!(
                "factor execution plan schema_version 必须为 {FACTOR_EXECUTION_PLAN_SCHEMA_VERSION}"
            )));
        }
        if self.input_fingerprint.trim().is_empty() || self.as_of == 0 {
            return Err(FactorError::Invalid(
                "factor execution plan 缺少 input_fingerprint 或 as_of".into(),
            ));
        }
        if self.requested_outputs.is_empty() || self.nodes.is_empty() {
            return Err(FactorError::Invalid(
                "factor execution plan 必须包含 requested_outputs 和 nodes".into(),
            ));
        }
        if !is_strictly_sorted(&self.requested_outputs) {
            return Err(FactorError::Invalid(
                "factor execution plan requested_outputs 必须唯一且按字典序排列".into(),
            ));
        }

        let mut seen = BTreeSet::new();
        for node in &self.nodes {
            if node.feature_key.trim().is_empty() || !seen.insert(node.feature_key.clone()) {
                return Err(FactorError::Invalid(
                    "factor execution plan node key 为空或重复".into(),
                ));
            }
            if !is_strictly_sorted(&node.dependencies) {
                return Err(FactorError::Invalid(format!(
                    "factor execution plan {} dependencies 必须唯一且有序",
                    node.feature_key
                )));
            }
            if node
                .dependencies
                .iter()
                .any(|dependency| !seen.contains(dependency))
            {
                return Err(FactorError::Invalid(format!(
                    "factor execution plan {} 依赖必须先于当前节点",
                    node.feature_key
                )));
            }
        }
        for output in &self.requested_outputs {
            if !seen.contains(output) {
                return Err(FactorError::MissingDefinition(output.clone()));
            }
        }
        Ok(())
    }

    pub fn digest(&self) -> Result<u64, FactorError> {
        self.validate()?;
        Ok(self.digest_unchecked())
    }

    fn digest_unchecked(&self) -> u64 {
        let mut hash = Fnv1a::new();
        hash.write_text("qx-factor-execution-plan-v1");
        hash.write_u64(self.schema_version as u64);
        hash.write_text(&self.input_fingerprint);
        hash.write_u64(self.as_of);
        hash.write_u64(self.requested_outputs.len() as u64);
        for output in &self.requested_outputs {
            hash.write_text(output);
        }
        hash.write_u64(self.nodes.len() as u64);
        for node in &self.nodes {
            hash.write_text(&node.feature_key);
            hash.write_u64(node.definition_digest);
            hash.write_u64(node.dependencies.len() as u64);
            for dependency in &node.dependencies {
                hash.write_text(dependency);
            }
            hash.write_u64(node.cache_key_digest);
        }
        hash.finish()
    }

    /// Compare this plan with a previously materialized plan.
    ///
    /// A data fingerprint or graph/definition change is fail-closed to a full
    /// recompute because this contract does not guess whether two arbitrary
    /// datasets are append-only compatible. A later `as_of` with otherwise
    /// identical lineage is marked as incremental work instead.
    pub fn incremental_against(
        &self,
        previous: &Self,
    ) -> Result<FactorIncrementalProvenance, FactorError> {
        self.validate()?;
        previous.validate()?;
        if self.as_of < previous.as_of {
            return Err(FactorError::Invalid(format!(
                "factor execution plan 时间倒退: previous={} current={}",
                previous.as_of, self.as_of
            )));
        }

        let previous_by_key = previous
            .nodes
            .iter()
            .map(|node| (node.feature_key.as_str(), node))
            .collect::<BTreeMap<_, _>>();
        let current_keys = self
            .nodes
            .iter()
            .map(|node| node.feature_key.as_str())
            .collect::<BTreeSet<_>>();
        let previous_keys = previous_by_key.keys().copied().collect::<BTreeSet<_>>();

        let graph_changed = self.requested_outputs != previous.requested_outputs
            || current_keys != previous_keys
            || self.nodes.iter().any(|node| {
                previous_by_key
                    .get(node.feature_key.as_str())
                    .is_none_or(|old| {
                        old.definition_digest != node.definition_digest
                            || old.dependencies != node.dependencies
                    })
            });
        let fingerprint_changed = self.input_fingerprint != previous.input_fingerprint;
        let full_recompute = graph_changed || fingerprint_changed;

        let dirty_nodes = self
            .nodes
            .iter()
            .filter(|node| {
                previous_by_key
                    .get(node.feature_key.as_str())
                    .is_none_or(|old| old.cache_key_digest != node.cache_key_digest)
            })
            .map(|node| node.feature_key.clone())
            .collect::<Vec<_>>();

        let reason = if graph_changed {
            "factor graph or definition changed"
        } else if fingerprint_changed {
            "input fingerprint changed"
        } else if dirty_nodes.is_empty() {
            "execution plan unchanged"
        } else if self.as_of > previous.as_of {
            "PIT cutoff advanced with stable lineage"
        } else {
            "cache lineage changed"
        }
        .to_string();

        Ok(FactorIncrementalProvenance {
            previous_plan_digest: previous.digest_unchecked(),
            current_plan_digest: self.digest_unchecked(),
            previous_as_of: previous.as_of,
            current_as_of: self.as_of,
            dirty_nodes,
            full_recompute,
            reason,
        })
    }
}

impl FactorCatalog {
    /// Compile only the transitive closure needed by `requested_outputs` while
    /// preserving the catalog's existing deterministic topological order.
    pub fn compile_execution_plan(
        &self,
        requested_outputs: &[String],
        input_fingerprint: &str,
        as_of: u64,
    ) -> Result<FactorExecutionPlan, FactorError> {
        if requested_outputs.is_empty() {
            return Err(FactorError::Invalid(
                "factor execution plan 至少需要一个 requested output".into(),
            ));
        }
        if input_fingerprint.trim().is_empty() || as_of == 0 {
            return Err(FactorError::Invalid(
                "factor execution plan input_fingerprint/as_of 非法".into(),
            ));
        }

        let mut outputs = requested_outputs.to_vec();
        outputs.sort();
        if outputs.windows(2).any(|window| window[0] == window[1]) {
            return Err(FactorError::Duplicate(
                "factor execution plan requested output".into(),
            ));
        }

        // Reuse the existing DAG validation/order rather than constructing a
        // second graph implementation.
        let execution_order = self.execution_order()?;
        let mut required = BTreeSet::new();
        for output in &outputs {
            collect_required(self, output, &mut required)?;
        }

        let mut cache_keys = BTreeMap::<String, u64>::new();
        let mut nodes = Vec::with_capacity(required.len());
        for key in execution_order.into_iter().filter(|key| required.contains(key)) {
            let definition = self
                .definitions
                .get(&key)
                .ok_or_else(|| FactorError::MissingDefinition(key.clone()))?;
            definition.validate()?;
            let mut dependencies = definition.dependencies.clone();
            dependencies.sort();
            dependencies.dedup();
            let definition_digest = definition_digest(definition);
            let cache_key_digest = cache_key_digest(
                &key,
                input_fingerprint,
                as_of,
                definition_digest,
                &dependencies,
                &cache_keys,
            )?;
            cache_keys.insert(key.clone(), cache_key_digest);
            nodes.push(FactorPlanNode {
                feature_key: key,
                definition_digest,
                dependencies,
                cache_key_digest,
            });
        }

        let plan = FactorExecutionPlan {
            schema_version: FACTOR_EXECUTION_PLAN_SCHEMA_VERSION,
            requested_outputs: outputs,
            input_fingerprint: input_fingerprint.to_string(),
            as_of,
            nodes,
        };
        plan.validate()?;
        Ok(plan)
    }
}

fn collect_required(
    catalog: &FactorCatalog,
    key: &str,
    required: &mut BTreeSet<String>,
) -> Result<(), FactorError> {
    if required.contains(key) {
        return Ok(());
    }
    let definition = catalog
        .definitions
        .get(key)
        .ok_or_else(|| FactorError::MissingDefinition(key.to_string()))?;
    definition.validate()?;
    required.insert(key.to_string());
    for dependency in &definition.dependencies {
        collect_required(catalog, dependency, required)?;
    }
    Ok(())
}

fn definition_digest(definition: &FeatureDefinition) -> u64 {
    let mut input_fields = definition.input_fields.clone();
    input_fields.sort();
    input_fields.dedup();
    let mut dependencies = definition.dependencies.clone();
    dependencies.sort();
    dependencies.dedup();

    let mut hash = Fnv1a::new();
    hash.write_text("qx-factor-definition-v1");
    hash.write_text(&definition.key());
    hash.write_text(&definition.formula);
    hash.write_u64(u64::from(definition.point_in_time));
    hash.write_u64(input_fields.len() as u64);
    for field in input_fields {
        hash.write_text(&field);
    }
    hash.write_u64(dependencies.len() as u64);
    for dependency in dependencies {
        hash.write_text(&dependency);
    }
    hash.finish()
}

fn cache_key_digest(
    key: &str,
    input_fingerprint: &str,
    as_of: u64,
    definition_digest: u64,
    dependencies: &[String],
    dependency_cache_keys: &BTreeMap<String, u64>,
) -> Result<u64, FactorError> {
    let mut hash = Fnv1a::new();
    hash.write_text("qx-factor-cache-key-v1");
    hash.write_text(key);
    hash.write_text(input_fingerprint);
    hash.write_u64(as_of);
    hash.write_u64(definition_digest);
    hash.write_u64(dependencies.len() as u64);
    for dependency in dependencies {
        let dependency_cache_key = dependency_cache_keys
            .get(dependency)
            .ok_or_else(|| FactorError::MissingDefinition(dependency.clone()))?;
        hash.write_text(dependency);
        hash.write_u64(*dependency_cache_key);
    }
    Ok(hash.finish())
}

fn is_strictly_sorted(values: &[String]) -> bool {
    values
        .windows(2)
        .all(|window| window[0].as_str() < window[1].as_str())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition(name: &str, formula: &str, dependencies: &[&str]) -> FeatureDefinition {
        FeatureDefinition {
            name: name.into(),
            version: "v1".into(),
            formula: formula.into(),
            input_fields: vec!["close".into()],
            dependencies: dependencies.iter().map(|value| (*value).into()).collect(),
            point_in_time: true,
        }
    }

    fn shared_dependency_catalog() -> FactorCatalog {
        let mut catalog = FactorCatalog::default();
        catalog
            .register_definition(definition("base", "close", &[]))
            .unwrap();
        catalog
            .register_definition(definition("left", "base * 2", &["base@v1"]))
            .unwrap();
        catalog
            .register_definition(definition("right", "base * 3", &["base@v1"]))
            .unwrap();
        catalog
    }

    #[test]
    fn plan_reuses_shared_dependency_and_is_request_order_independent() {
        let catalog = shared_dependency_catalog();
        let first = catalog
            .compile_execution_plan(
                &["right@v1".into(), "left@v1".into()],
                "bars-sha256",
                100,
            )
            .unwrap();
        let second = catalog
            .compile_execution_plan(
                &["left@v1".into(), "right@v1".into()],
                "bars-sha256",
                100,
            )
            .unwrap();

        assert_eq!(first, second);
        assert_eq!(first.digest().unwrap(), second.digest().unwrap());
        assert_eq!(
            first
                .nodes
                .iter()
                .map(|node| node.feature_key.as_str())
                .collect::<Vec<_>>(),
            vec!["base@v1", "left@v1", "right@v1"]
        );
    }

    #[test]
    fn definition_data_and_time_are_bound_into_cache_lineage() {
        let catalog = shared_dependency_catalog();
        let original = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 100)
            .unwrap();
        let later = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 101)
            .unwrap();
        let new_data = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-b", 100)
            .unwrap();

        assert_ne!(original.digest().unwrap(), later.digest().unwrap());
        assert_ne!(original.nodes[0].cache_key_digest, later.nodes[0].cache_key_digest);
        assert_ne!(original.nodes[0].cache_key_digest, new_data.nodes[0].cache_key_digest);

        let mut changed_catalog = FactorCatalog::default();
        changed_catalog
            .register_definition(definition("base", "close + 1", &[]))
            .unwrap();
        changed_catalog
            .register_definition(definition("left", "base * 2", &["base@v1"]))
            .unwrap();
        let changed = changed_catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 100)
            .unwrap();
        assert_ne!(original.nodes[0].definition_digest, changed.nodes[0].definition_digest);
        assert_ne!(original.nodes[1].cache_key_digest, changed.nodes[1].cache_key_digest);
    }

    #[test]
    fn incremental_provenance_distinguishes_append_time_from_lineage_change() {
        let catalog = shared_dependency_catalog();
        let previous = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 100)
            .unwrap();
        let same = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 100)
            .unwrap();
        let later = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 101)
            .unwrap();
        let new_data = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-b", 101)
            .unwrap();

        let unchanged = same.incremental_against(&previous).unwrap();
        assert!(unchanged.dirty_nodes.is_empty());
        assert!(!unchanged.full_recompute);

        let incremental = later.incremental_against(&previous).unwrap();
        assert_eq!(incremental.dirty_nodes, vec!["base@v1", "left@v1"]);
        assert!(!incremental.full_recompute);
        assert!(incremental.reason.contains("PIT cutoff"));

        let full = new_data.incremental_against(&later).unwrap();
        assert_eq!(full.dirty_nodes, vec!["base@v1", "left@v1"]);
        assert!(full.full_recompute);
        assert!(full.reason.contains("fingerprint"));
    }

    #[test]
    fn plan_rejects_missing_duplicate_and_time_reversal() {
        let catalog = shared_dependency_catalog();
        assert!(matches!(
            catalog.compile_execution_plan(&["missing@v1".into()], "bars-a", 100),
            Err(FactorError::MissingDefinition(_))
        ));
        assert!(matches!(
            catalog.compile_execution_plan(
                &["left@v1".into(), "left@v1".into()],
                "bars-a",
                100
            ),
            Err(FactorError::Duplicate(_))
        ));
        assert!(catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 0)
            .is_err());

        let previous = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 100)
            .unwrap();
        let earlier = catalog
            .compile_execution_plan(&["left@v1".into()], "bars-a", 99)
            .unwrap();
        assert!(earlier.incremental_against(&previous).is_err());
    }
}
