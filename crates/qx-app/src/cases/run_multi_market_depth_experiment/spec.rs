use super::*;
use crate::{
    AppError, CallerCapability, ExperimentParameterSpace, RunContext, RunHandle,
    StrategyArtifactIdentity,
};
use qx_strategy::Strategy;

impl RunMultiMarketDepthExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload).map_err(|error| {
            invalid(format!(
                "RunMultiMarketDepthExperimentSpec JSON 无效: {error}"
            ))
        })?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        let mut canonical = self.clone();
        canonical.strategy_kinds.sort_by_key(|kind| kind.name());
        serde_json::to_string(&canonical)
            .map_err(|error| invariant(format!("多市场盘口实验规格序列化失败: {error}")))
    }

    pub fn validate(&self) -> Result<usize, AppError> {
        if self.schema_version != RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "RunMultiMarketDepthExperimentSpec schema_version={} 本版本只认 {}",
                self.schema_version, RUN_MULTI_MARKET_DEPTH_EXPERIMENT_SCHEMA_VERSION
            )));
        }
        validate_experiment_id(&self.experiment_id)?;
        if self.experiment_id.len() + 5 > 96 {
            return Err(invalid(
                "experiment_id 最多 91 字符，以便生成不越界的候选 run_id",
            ));
        }
        self.base.validate()?;
        let targets = self
            .base
            .markets
            .iter()
            .filter(|market| market.strategy.strategy_id == self.target_strategy_id)
            .collect::<Vec<_>>();
        if self.target_strategy_id.trim().is_empty() || targets.len() != 1 {
            return Err(invalid(
                "target_strategy_id 必须非空且唯一匹配一个多市场盘口策略槽",
            ));
        }
        let base_strategy = &targets[0].strategy;
        let mut kinds = if self.strategy_kinds.is_empty() {
            vec![base_strategy.kind]
        } else {
            self.strategy_kinds.clone()
        };
        kinds.sort_by_key(|kind| kind.name());
        let mut seen = BTreeSet::new();
        if kinds.iter().any(|kind| !seen.insert(kind.name())) {
            return Err(invalid("strategy_kinds 不得重复"));
        }
        if kinds.iter().any(|kind| kind.needs_reference_leg()) {
            return Err(invalid("多市场盘口单槽实验不能选择双腿策略"));
        }
        if self.parameter_space.is_empty()
            && self.strategy_kinds.is_empty()
            && self.callback_parameter_space.is_empty()
        {
            return Err(invalid(
                "多市场盘口实验至少需要参数维度、strategy_kinds 或 callback_parameter_space",
            ));
        }
        let callback_count = validate_callback_parameter_grid(&self.callback_parameter_space)?;
        if callback_count > 0
            && (!self.parameter_space.is_empty() || !self.strategy_kinds.is_empty())
        {
            return Err(invalid(
                "callback_parameter_space 只能与自定义策略工厂使用，不能和内置参数/策略网格混用",
            ));
        }
        let mut total = 0_usize;
        for kind in kinds {
            let mut candidate = base_strategy.clone();
            candidate.kind = kind;
            let normalized = normalized_strategy(&candidate, &self.parameter_space);
            let count = if self.parameter_space.is_empty() {
                1
            } else {
                validate_grid(&normalized, &self.parameter_space)?
            };
            total = total
                .checked_add(count)
                .filter(|count| *count <= MAX_MULTI_MARKET_DEPTH_EXPERIMENT_CANDIDATES)
                .ok_or_else(|| invalid("多市场盘口实验最多接受 256 个候选组合"))?;
        }
        if callback_count > 0 {
            total = total
                .checked_mul(callback_count)
                .filter(|count| *count <= MAX_MULTI_MARKET_DEPTH_EXPERIMENT_CANDIDATES)
                .ok_or_else(|| invalid("多市场盘口实验最多接受 256 个候选组合"))?;
        }
        Ok(total)
    }

    pub fn run_with_strategy_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunMultiMarketDepthExperimentResult, AppError>
    where
        F: FnMut(
            &MultiMarketDepthBacktestSpec,
            &BTreeMap<String, i128>,
        ) -> Result<Box<dyn Strategy>, String>,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言多市场盘口参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.strategy_kinds.is_empty() || !self.parameter_space.is_empty() {
            return Err(attach(
                invalid("自定义策略工厂只能使用 callback_parameter_space，不能混用内置策略参数或 strategy_kinds"),
                &correlation_id,
            ));
        }
        validate_identity(self, identity).map_err(|error| attach(error, &correlation_id))?;
        guard_panics(&correlation_id, || {
            run_inner(self, context, None, Some(identity), Some(factory))
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| attach(invariant("同步多市场盘口实验未产生报告"), &correlation_id))
    }

    pub fn start_with_strategy_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunMultiMarketDepthExperimentResult>, AppError>
    where
        F: FnMut(
                &MultiMarketDepthBacktestSpec,
                &BTreeMap<String, i128>,
            ) -> Result<Box<dyn Strategy>, String>
            + Send
            + 'static,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言多市场盘口参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.strategy_kinds.is_empty() || !self.parameter_space.is_empty() {
            return Err(attach(
                invalid("自定义策略工厂只能使用 callback_parameter_space，不能混用内置策略参数或 strategy_kinds"),
                &correlation_id,
            ));
        }
        validate_identity(&self, &identity).map_err(|error| attach(error, &correlation_id))?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner(
                    &self,
                    &context,
                    Some(&cancellation),
                    Some(&identity),
                    Some(&mut factory),
                )
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }
}

fn validate_callback_parameter_grid(
    dimensions: &[ExperimentParameterSpace],
) -> Result<usize, AppError> {
    let mut names = BTreeSet::new();
    let mut total = 1_usize;
    for dimension in dimensions {
        let mut chars = dimension.name.chars();
        let valid_name = chars
            .next()
            .is_some_and(|first| first.is_ascii_alphabetic() || first == '_')
            && chars.all(|ch| ch.is_ascii_alphanumeric() || ch == '_')
            && dimension.name.len() <= 64;
        if !valid_name || !names.insert(dimension.name.as_str()) {
            return Err(invalid(
                "callback 参数名必须唯一且符合 [A-Za-z_][A-Za-z0-9_]{0,63}",
            ));
        }
        if dimension.values.is_empty()
            || dimension.values.iter().collect::<BTreeSet<_>>().len() != dimension.values.len()
        {
            return Err(invalid(format!(
                "callback 参数维度 {} 为空或含重复值",
                dimension.name
            )));
        }
        total = total
            .checked_mul(dimension.values.len())
            .filter(|count| *count <= MAX_MULTI_MARKET_DEPTH_EXPERIMENT_CANDIDATES)
            .ok_or_else(|| invalid("callback 参数网格最多接受 256 个候选组合"))?;
    }
    Ok(if dimensions.is_empty() { 0 } else { total })
}
