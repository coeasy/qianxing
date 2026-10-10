use super::*;

pub(super) fn start(body: &str, paths: &AppPaths, registry: &AppRunRegistry) -> ApiResponse {
    let mut spec = match RunDepthExperimentSpec::from_json(body) {
        Ok(spec) => spec,
        Err(error) => return respond(Err(error)),
    };
    if let Err(error) = resolve_input_path(&mut spec.base.depth_path, &paths.data_root) {
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
    match registry.attach(&run_id, AppRun::DepthExperiment(handle)) {
        Ok(()) => registry.status(&run_id),
        Err(error) => respond(Err(error)),
    }
}
