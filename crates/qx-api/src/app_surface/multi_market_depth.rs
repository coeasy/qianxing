use super::*;

pub(crate) fn multi_market_depth(body: &str, paths: &AppPaths) -> ApiResponse {
    let mut spec = match qx_app::MultiMarketDepthBacktestSpec::from_json(body) {
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
    respond(
        qx_app::run_multi_market_depth_backtest(&spec, &context(&run_id))
            .and_then(|outcome| outcome.to_json()),
    )
}
