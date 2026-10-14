use super::Client;
use qx_app::{
    run_multi_market_depth_experiment, AppError, MultiMarketDepthBacktestSpec, RunHandle,
    RunMultiMarketDepthExperimentResult, RunMultiMarketDepthExperimentSpec,
    StrategyArtifactIdentity,
};
use qx_strategy::Strategy;

impl Client {
    pub fn run_multi_market_depth_experiment(
        &self,
        spec: &RunMultiMarketDepthExperimentSpec,
    ) -> Result<RunMultiMarketDepthExperimentResult, AppError> {
        run_multi_market_depth_experiment(spec, &self.context)
    }

    pub fn start_multi_market_depth_experiment(
        &self,
        spec: RunMultiMarketDepthExperimentSpec,
    ) -> RunHandle<RunMultiMarketDepthExperimentResult> {
        spec.start(self.context.clone())
    }

    pub fn run_multi_market_depth_experiment_with_strategy_factory<F>(
        &self,
        spec: &RunMultiMarketDepthExperimentSpec,
        identity: &StrategyArtifactIdentity,
        factory: &mut F,
    ) -> Result<RunMultiMarketDepthExperimentResult, AppError>
    where
        F: FnMut(
            &MultiMarketDepthBacktestSpec,
            &std::collections::BTreeMap<String, i128>,
        ) -> Result<Box<dyn Strategy>, String>,
    {
        spec.run_with_strategy_factory(identity, factory, &self.context)
    }

    pub fn start_multi_market_depth_experiment_with_strategy_factory<F>(
        &self,
        spec: RunMultiMarketDepthExperimentSpec,
        identity: StrategyArtifactIdentity,
        factory: F,
    ) -> Result<RunHandle<RunMultiMarketDepthExperimentResult>, AppError>
    where
        F: FnMut(
                &MultiMarketDepthBacktestSpec,
                &std::collections::BTreeMap<String, i128>,
            ) -> Result<Box<dyn Strategy>, String>
            + Send
            + 'static,
    {
        spec.start_with_strategy_factory(identity, factory, self.context.clone())
    }
}
