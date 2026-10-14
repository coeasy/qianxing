use super::{context, resolve_input_path, respond, ApiResponse, AppPaths};
use qx_app::{run_multi_account_backtest, run_multi_account_depth_backtest};
use qx_app::{MultiAccountBacktestSpec, MultiAccountDepthBacktestSpec};

pub(crate) fn multi_account(body: &str, paths: &AppPaths) -> ApiResponse {
    let mut spec = match MultiAccountBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for run in &mut spec.runs {
        if let Err(error) = resolve_input_path(&mut run.bars_path, &paths.data_root) {
            return respond(Err(error));
        }
        run.output_dir = paths
            .artifact_root
            .join(&run.run_id)
            .to_string_lossy()
            .into_owned();
    }
    let batch_id = spec.batch_id.clone();
    respond(
        run_multi_account_backtest(&spec, &context(&batch_id)).and_then(|result| result.to_json()),
    )
}

pub(crate) fn depth_batch(body: &str, paths: &AppPaths) -> ApiResponse {
    let mut spec = match MultiAccountDepthBacktestSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    for run in &mut spec.runs {
        if let Err(error) = resolve_input_path(&mut run.depth_path, &paths.data_root) {
            return respond(Err(error));
        }
        run.output_dir = paths
            .artifact_root
            .join(&run.run_id)
            .to_string_lossy()
            .into_owned();
    }
    let batch_id = spec.batch_id.clone();
    respond(
        run_multi_account_depth_backtest(&spec, &context(&batch_id))
            .and_then(|result| result.to_json()),
    )
}
