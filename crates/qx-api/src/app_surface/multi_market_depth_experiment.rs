use super::*;

pub(crate) fn multi_market_depth_experiment(body: &str, paths: &AppPaths) -> ApiResponse {
    let mut spec = match RunMultiMarketDepthExperimentSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for market in &mut spec.base.markets {
        if let Err(error) = resolve_input_path(&mut market.depth_path, &paths.data_root) {
            return respond(Err(error));
        }
    }
    let experiment_id = spec.experiment_id.clone();
    spec.base.output_dir = paths
        .artifact_root
        .join(&experiment_id)
        .to_string_lossy()
        .into_owned();
    respond(
        run_multi_market_depth_experiment(&spec, &context(&experiment_id))
            .and_then(|result| result.to_json()),
    )
}
