//! Start routes for the shared Rust app use cases.

use super::*;

pub(super) fn start_run_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match BacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = spec.validate() {
        return respond(Err(error));
    }
    if let Err(error) = resolve_input_path(&mut spec.bars_path, &paths.data_root) {
        return respond(Err(error));
    }
    let run_id = spec.run_id.clone();
    spec.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::Bar(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_depth_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match DepthBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = spec.validate() {
        return respond(Err(error));
    }
    if let Err(error) = resolve_input_path(&mut spec.depth_path, &paths.data_root) {
        return respond(Err(error));
    }
    let run_id = spec.run_id.clone();
    spec.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::Depth(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_experiment(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match RunExperimentSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = spec.validate() {
        return respond(Err(error));
    }
    if let Err(error) = resolve_input_path(&mut spec.base.bars_path, &paths.data_root) {
        return respond(Err(error));
    }
    let run_id = spec.experiment_id.clone();
    spec.base.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::Experiment(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_multi_leg_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match MultiLegBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = spec.validate() {
        return respond(Err(error));
    }
    for input in [&mut spec.primary_bars_path, &mut spec.reference_bars_path] {
        if let Err(error) = resolve_input_path(input, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let run_id = spec.run_id.clone();
    spec.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::MultiLeg(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_multi_account_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match MultiAccountBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for run in &mut spec.runs {
        if let Err(error) = resolve_input_path(&mut run.bars_path, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let batch_id = spec.batch_id.clone();
    for run in &mut spec.runs {
        run.output_dir = paths
            .artifact_root
            .join(&run.run_id)
            .to_string_lossy()
            .into_owned();
    }
    if let Err(error) = registry.reserve(batch_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&batch_id));
    match registry.attach(&batch_id, AppRun::MultiAccount(handle)) {
        Ok(()) => registry.status(&batch_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_multi_account_depth_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match MultiAccountDepthBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for run in &mut spec.runs {
        if let Err(error) = resolve_input_path(&mut run.depth_path, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let batch_id = spec.batch_id.clone();
    for run in &mut spec.runs {
        run.output_dir = paths
            .artifact_root
            .join(&run.run_id)
            .to_string_lossy()
            .into_owned();
    }
    if let Err(error) = registry.reserve(batch_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&batch_id));
    match registry.attach(&batch_id, AppRun::MultiAccountDepth(handle)) {
        Ok(()) => registry.status(&batch_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_multi_market_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match MultiMarketBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for market in &mut spec.markets {
        if let Err(error) = resolve_input_path(&mut market.bars_path, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let run_id = spec.run_id.clone();
    spec.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::MultiMarket(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_multi_market_depth_backtest(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match MultiMarketDepthBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for market in &mut spec.markets {
        if let Err(error) = resolve_input_path(&mut market.depth_path, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let run_id = spec.run_id.clone();
    spec.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::MultiMarketDepth(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_multi_leg_experiment(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match RunMultiLegExperimentSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for input in [
        &mut spec.base.primary_bars_path,
        &mut spec.base.reference_bars_path,
    ] {
        if let Err(error) = resolve_input_path(input, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let run_id = spec.experiment_id.clone();
    spec.base.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::MultiLegExperiment(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_multi_market_experiment(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match RunMultiMarketExperimentSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for market in &mut spec.base.markets {
        if let Err(error) = resolve_input_path(&mut market.bars_path, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let run_id = spec.experiment_id.clone();
    spec.base.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::MultiMarketExperiment(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}

pub(super) fn start_multi_market_depth_experiment(
    body: &str,
    paths: &AppPaths,
    registry: &AppRunRegistry,
) -> ApiResponse {
    let mut spec = match qx_app::RunMultiMarketDepthExperimentSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for market in &mut spec.base.markets {
        if let Err(error) = resolve_input_path(&mut market.depth_path, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let run_id = spec.experiment_id.clone();
    spec.base.output_dir = paths
        .artifact_root
        .join(&run_id)
        .to_string_lossy()
        .into_owned();
    if let Err(error) = registry.reserve(run_id.clone()) {
        return respond(Err(error));
    }
    let handle = spec.start(context(&run_id));
    match registry.attach(&run_id, AppRun::MultiMarketDepthExperiment(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}
