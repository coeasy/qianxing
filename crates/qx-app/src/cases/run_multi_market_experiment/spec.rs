//! Multi-market experiment JSON contract and validation.

use super::*;

impl RunMultiMarketExperimentResult {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        serde_json::from_str(payload)
            .map_err(|error| AppError::new(AppErrorCategory::StorageFailure, error.to_string()))
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        serde_json::to_string(self)
            .map_err(|error| invariant(format!("多市场实验结果序列化失败: {error}")))
    }
}

impl RunMultiMarketExperimentSpec {
    pub fn from_json(payload: &str) -> Result<Self, AppError> {
        let spec: Self = serde_json::from_str(payload)
            .map_err(|error| invalid(format!("RunMultiMarketExperimentSpec JSON 无效: {error}")))?;
        spec.validate()?;
        Ok(spec)
    }

    pub fn to_json(&self) -> Result<String, AppError> {
        let mut canonical = self.clone();
        canonical.strategy_kinds.sort_by_key(|kind| kind.name());
        serde_json::to_string(&canonical)
            .map_err(|error| invariant(format!("RunMultiMarketExperimentSpec 序列化失败: {error}")))
    }

    pub fn validate(&self) -> Result<usize, AppError> {
        if self.schema_version != RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION {
            return Err(invalid(format!(
                "RunMultiMarketExperimentSpec schema_version={} 本版本只认 {}",
                self.schema_version, RUN_MULTI_MARKET_EXPERIMENT_SCHEMA_VERSION
            )));
        }
        validate_experiment_id(&self.experiment_id)?;
        self.base.validate()?;
        if let Some(plan) = &self.split_plan {
            plan.validate().map_err(invalid)?;
        }
        let targets = self
            .base
            .markets
            .iter()
            .flat_map(|market| &market.strategies)
            .filter(|slot| slot.strategy.strategy_id == self.target_strategy_id)
            .collect::<Vec<_>>();
        if targets.len() != 1 {
            return Err(invalid("target_strategy_id 必须唯一匹配一个多市场策略槽"));
        }
        if self.target_strategy_id.trim().is_empty() {
            return Err(invalid("target_strategy_id 不能为空"));
        }
        let base_strategy = &targets[0].strategy;
        let mut kinds = if self.strategy_kinds.is_empty() {
            vec![base_strategy.kind]
        } else {
            self.strategy_kinds.clone()
        };
        kinds.sort_by_key(|kind| kind.name());
        if kinds.iter().any(|kind| kind.needs_reference_leg()) {
            return Err(invalid(
                "多市场单槽实验不能选择需要 reference leg 的双腿策略",
            ));
        }
        if kinds.windows(2).any(|pair| pair[0] == pair[1]) {
            return Err(invalid("strategy_kinds 不得重复"));
        }
        if kinds.is_empty() {
            return Err(invalid("多市场实验没有策略候选"));
        }
        let callback_count = validate_callback_parameter_grid(&self.callback_parameter_space)?;
        if callback_count > 0
            && (!self.parameter_space.is_empty() || !self.strategy_kinds.is_empty())
        {
            return Err(invalid(
                "callback_parameter_space 只能与自定义策略工厂使用，不能和内置参数/策略网格混用",
            ));
        }
        if callback_count > 0
            && self
                .search
                .as_ref()
                .is_some_and(|search| !search.continuous_parameter_space.is_empty())
        {
            return Err(invalid(
                "callback_parameter_space 不能与 search.continuous_parameter_space 混用",
            ));
        }
        if self.parameter_space.is_empty()
            && self.strategy_kinds.is_empty()
            && callback_count == 0
            && self
                .search
                .as_ref()
                .is_none_or(|search| search.continuous_parameter_space.is_empty())
        {
            return Err(invalid("多市场实验至少需要参数维度或 strategy_kinds"));
        }
        if let Some(search) = &self.search {
            if self.split_plan.is_none() {
                return Err(invalid(
                    "多市场自适应搜索要求配置 split_plan，只能用训练窗提案和选参",
                ));
            }
            if !self.strategy_kinds.is_empty() {
                return Err(invalid(
                    "多市场自适应搜索当前针对单个策略槽的参数，不接受 strategy_kinds",
                ));
            }
            if search.budget == 0 || search.budget > MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES {
                return Err(invalid(format!(
                    "多市场自适应搜索预算必须在 1..={MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES}"
                )));
            }
            if search
                .max_training_drawdown_bps
                .is_some_and(|limit| limit > 10_000)
            {
                return Err(invalid("max_training_drawdown_bps 不得大于 10000"));
            }
            if search.early_pruning.is_some()
                || search.robustness_subwindows.is_some()
                || search.minimum_training_robustness_score_bps.is_some()
                || search.neighborhood_radius.is_some()
            {
                return Err(invalid(
                    "多市场自适应搜索暂不支持 early_pruning、robustness_subwindows、minimum_training_robustness_score_bps 或 neighborhood_radius",
                ));
            }
            let domain = if !search.continuous_parameter_space.is_empty() {
                if !self.parameter_space.is_empty() || callback_count > 0 {
                    return Err(invalid("continuous_parameter_space 不能与离散参数网格混用"));
                }
                if self.search_uses_callback_ranges() {
                    validate_callback_parameter_ranges(&search.continuous_parameter_space)?
                } else {
                    validate_continuous_grid(base_strategy, &search.continuous_parameter_space)?
                }
            } else if callback_count > 0 {
                callback_count as u128
            } else if !self.parameter_space.is_empty() {
                validate_grid(
                    &normalized_strategy(base_strategy, &self.parameter_space),
                    &self.parameter_space,
                )? as u128
            } else {
                return Err(invalid(
                    "多市场自适应搜索至少需要 parameter_space、callback_parameter_space 或 continuous_parameter_space",
                ));
            };
            if search.budget as u128 > domain {
                return Err(invalid(format!(
                    "多市场搜索预算 {} 超过参数值域大小 {domain}",
                    search.budget
                )));
            }
            return Ok(search.budget);
        }
        let mut total = 0_usize;
        for kind in kinds {
            let mut strategy = base_strategy.clone();
            strategy.kind = kind;
            let normalized = normalized_strategy(&strategy, &self.parameter_space);
            let count = if self.parameter_space.is_empty() {
                1
            } else {
                validate_grid(&normalized, &self.parameter_space)?
            };
            total = total
                .checked_add(count)
                .filter(|count| *count <= MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES)
                .ok_or_else(|| invalid("多市场实验最多接受 256 个候选组合"))?;
        }
        if callback_count > 0 {
            total = total
                .checked_mul(callback_count)
                .filter(|count| *count <= MAX_MULTI_MARKET_EXPERIMENT_CANDIDATES)
                .ok_or_else(|| invalid("多市场实验最多接受 256 个候选组合"))?;
        }
        Ok(total)
    }

    pub(super) fn search_uses_callback_ranges(&self) -> bool {
        self.search.as_ref().is_some_and(|search| {
            search
                .continuous_parameter_space
                .iter()
                .any(|range| !is_builtin_search_parameter(&range.name))
        })
    }

    fn supports_parameter_factory(&self) -> bool {
        !self.callback_parameter_space.is_empty()
            || self.search_uses_callback_ranges()
            || (self.search.is_some()
                && (!self.parameter_space.is_empty()
                    || self
                        .search
                        .as_ref()
                        .is_some_and(|search| !search.continuous_parameter_space.is_empty())))
    }

    pub fn start(self, context: RunContext) -> RunHandle<RunMultiMarketExperimentResult> {
        let run_id = context.correlation_id().to_string();
        RunHandle::spawn(run_id, move |cancellation| {
            context.require(CallerCapability::Research, "多市场参数回测实验")?;
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner_experiment(&self, &context, Some(&cancellation), None, None)
            })
            .map_err(|error| attach(error, &correlation_id))
        })
    }

    pub fn run_with_strategy_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunMultiMarketExperimentResult, AppError>
    where
        F: FnMut(&MultiMarketBacktestSpec) -> Result<Box<dyn Strategy>, String>,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言策略多市场参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.strategy_kinds.is_empty()
            || !self.callback_parameter_space.is_empty()
            || self.search_uses_callback_ranges()
        {
            return Err(invalid(
                "该自定义策略工厂不接受任意回调参数；请使用 run_with_parameter_factory 和 callback_parameter_space 或自定义连续范围",
            ));
        }
        validate_identity(self, identity)?;
        let mut adapted_factory =
            |candidate: &MultiMarketBacktestSpec, _parameters: &BTreeMap<String, i128>| {
                factory(candidate)
            };
        guard_panics(&correlation_id, || {
            run_inner_experiment(
                self,
                context,
                None,
                Some(identity),
                Some(&mut adapted_factory),
            )
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| attach(invariant("同步多市场策略实验未产生报告"), &correlation_id))
    }

    pub fn start_with_strategy_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunMultiMarketExperimentResult>, AppError>
    where
        F: FnMut(&MultiMarketBacktestSpec) -> Result<Box<dyn Strategy>, String> + Send + 'static,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言策略多市场参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.strategy_kinds.is_empty()
            || !self.callback_parameter_space.is_empty()
            || self.search_uses_callback_ranges()
        {
            return Err(invalid(
                "该自定义策略工厂不接受任意回调参数；请使用 start_with_parameter_factory 和 callback_parameter_space 或自定义连续范围",
            ));
        }
        validate_identity(&self, &identity).map_err(|error| attach(error, &correlation_id))?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            let mut adapted_factory =
                |candidate: &MultiMarketBacktestSpec, _parameters: &BTreeMap<String, i128>| {
                    factory(candidate)
                };
            guard_panics(&correlation_id, || {
                run_inner_experiment(
                    &self,
                    &context,
                    Some(&cancellation),
                    Some(&identity),
                    Some(&mut adapted_factory),
                )
            })
            .map_err(|error| attach(error, &correlation_id))
        }))
    }

    pub fn run_with_parameter_factory<F>(
        &self,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
        context: &RunContext,
    ) -> Result<RunMultiMarketExperimentResult, AppError>
    where
        F: FnMut(
            &MultiMarketBacktestSpec,
            &BTreeMap<String, i128>,
        ) -> Result<Box<dyn Strategy>, String>,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言策略多市场参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.supports_parameter_factory() || !self.strategy_kinds.is_empty() {
            return Err(attach(
                invalid(
                    "任意参数工厂必须只使用 callback_parameter_space 或 custom continuous ranges",
                ),
                &correlation_id,
            ));
        }
        validate_identity(self, identity).map_err(|error| attach(error, &correlation_id))?;
        guard_panics(&correlation_id, || {
            run_inner_experiment(self, context, None, Some(identity), Some(factory))
        })
        .map_err(|error| attach(error, &correlation_id))?
        .ok_or_else(|| attach(invariant("同步多市场策略实验未产生报告"), &correlation_id))
    }

    pub fn start_with_parameter_factory<F>(
        self,
        identity: StrategyArtifactIdentity,
        mut factory: F,
        context: RunContext,
    ) -> Result<RunHandle<RunMultiMarketExperimentResult>, AppError>
    where
        F: FnMut(
                &MultiMarketBacktestSpec,
                &BTreeMap<String, i128>,
            ) -> Result<Box<dyn Strategy>, String>
            + Send
            + 'static,
    {
        let correlation_id = context.correlation_id().to_string();
        context
            .require(CallerCapability::Research, "多语言策略多市场参数实验")
            .map_err(|error| attach(error, &correlation_id))?;
        self.validate()
            .map_err(|error| attach(error, &correlation_id))?;
        if !self.supports_parameter_factory() || !self.strategy_kinds.is_empty() {
            return Err(attach(
                invalid(
                    "任意参数工厂必须只使用 callback_parameter_space 或 custom continuous ranges",
                ),
                &correlation_id,
            ));
        }
        validate_identity(&self, &identity).map_err(|error| attach(error, &correlation_id))?;
        let run_id = context.correlation_id().to_string();
        Ok(RunHandle::spawn(run_id, move |cancellation| {
            let correlation_id = context.correlation_id().to_string();
            guard_panics(&correlation_id, || {
                run_inner_experiment(
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
