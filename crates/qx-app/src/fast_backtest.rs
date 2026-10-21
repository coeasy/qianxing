//! Manifest validation and bounded execution policy for offline batch backtests.

use std::path::{Path, PathBuf};

const MAX_FAST_BACKTEST_WORKERS: usize = 8;
const MAX_FAST_BACKTEST_JOBS: usize = 256;
const FAST_BACKTEST_MANIFEST_SCHEMA_VERSION: u64 = 1;

/// A validated job from a fast-backtest manifest.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct FastBacktestJob {
    pub index: usize,
    pub runtime: PathBuf,
    pub bars: PathBuf,
    pub market_spec: Option<PathBuf>,
    pub dataset_manifest: Option<PathBuf>,
}

/// Parse and resolve manifest paths relative to the manifest file.
pub fn parse_fast_backtest_manifest(
    manifest_path: &Path,
    payload: &str,
) -> Result<Vec<FastBacktestJob>, String> {
    let document: serde_json::Value = serde_json::from_str(payload)
        .map_err(|error| format!("快速回测 manifest JSON 无效: {error}"))?;
    let document = document
        .as_object()
        .ok_or_else(|| "快速回测 manifest 顶层必须是对象".to_string())?;
    if let Some(unknown) = document
        .keys()
        .find(|key| !matches!(key.as_str(), "schema_version" | "jobs"))
    {
        return Err(format!("快速回测 manifest 包含未知字段 {unknown}"));
    }
    let schema_version = document
        .get("schema_version")
        .and_then(serde_json::Value::as_u64);
    if schema_version != Some(FAST_BACKTEST_MANIFEST_SCHEMA_VERSION) {
        return Err(format!(
            "快速回测 manifest schema_version 必须为 {FAST_BACKTEST_MANIFEST_SCHEMA_VERSION}"
        ));
    }
    let jobs = document
        .get("jobs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "快速回测 manifest 必须包含 jobs 数组".to_string())?;
    if jobs.is_empty() || jobs.len() > MAX_FAST_BACKTEST_JOBS {
        return Err(format!(
            "快速回测 jobs 数量必须在 1..={MAX_FAST_BACKTEST_JOBS} 内"
        ));
    }
    let base = manifest_path.parent().unwrap_or_else(|| Path::new("."));
    let resolve = |value: &str| {
        let path = PathBuf::from(value);
        if path.is_absolute() {
            path
        } else {
            base.join(path)
        }
    };
    jobs.iter()
        .enumerate()
        .map(|(index, job)| {
            let object = job
                .as_object()
                .ok_or_else(|| format!("快速回测 jobs[{index}] 必须是对象"))?;
            if let Some(unknown) = object.keys().find(|key| {
                !matches!(
                    key.as_str(),
                    "runtime" | "bars" | "market_spec" | "dataset_manifest"
                )
            }) {
                return Err(format!("快速回测 jobs[{index}] 包含未知字段 {unknown}"));
            }
            let required_path = |name: &str| {
                object
                    .get(name)
                    .and_then(serde_json::Value::as_str)
                    .filter(|value| !value.trim().is_empty())
                    .map(resolve)
                    .ok_or_else(|| format!("快速回测 jobs[{index}] 缺少 {name}"))
            };
            let optional_path = |name: &str| -> Result<Option<PathBuf>, String> {
                match object.get(name) {
                    None => Ok(None),
                    Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
                        Ok(Some(resolve(value)))
                    }
                    Some(serde_json::Value::String(_)) => Err(format!(
                        "快速回测 jobs[{index}] {name} 若在场必须是非空路径"
                    )),
                    Some(_) => Err(format!("快速回测 jobs[{index}] {name} 必须是字符串路径")),
                }
            };
            let runtime = required_path("runtime")?;
            let bars = required_path("bars")?;
            let market_spec = optional_path("market_spec")?;
            let dataset_manifest = match object.get("dataset_manifest") {
                None => None,
                Some(serde_json::Value::String(value)) if !value.trim().is_empty() => {
                    Some(resolve(value))
                }
                Some(serde_json::Value::String(_)) => {
                    return Err(format!(
                        "快速回测 jobs[{index}] dataset_manifest 若在场必须是非空路径"
                    ));
                }
                Some(_) => {
                    return Err(format!(
                        "快速回测 jobs[{index}] dataset_manifest 必须是字符串路径"
                    ));
                }
            };
            Ok(FastBacktestJob {
                index,
                runtime,
                bars,
                market_spec,
                dataset_manifest,
            })
        })
        .collect()
}

/// Run validated jobs in bounded batches and return the completed count.
/// All jobs are joined before the first error is returned, so no scoped worker is abandoned.
pub fn run_fast_backtest_jobs<F>(
    jobs: &[FastBacktestJob],
    available_parallelism: usize,
    execute: F,
) -> Result<usize, String>
where
    F: Fn(&FastBacktestJob) -> Result<(), String> + Sync,
{
    if jobs.is_empty() {
        return Err("快速回测 jobs 数量必须在 1..=256 内".into());
    }
    let worker_limit = fast_backtest_parallelism(jobs.len(), available_parallelism);
    std::thread::scope(|scope| {
        let mut completed = 0;
        let mut first_error = None;
        for batch in jobs.chunks(worker_limit) {
            let execute = &execute;
            let handles = batch
                .iter()
                .map(|job| scope.spawn(move || execute(job).map_err(|error| (job.index, error))))
                .collect::<Vec<_>>();
            for handle in handles {
                match handle.join() {
                    Ok(Ok(())) => completed += 1,
                    Ok(Err((index, error))) => {
                        if first_error.is_none() {
                            first_error = Some(format!("jobs[{index}] {error}"));
                        }
                    }
                    Err(_) => {
                        if first_error.is_none() {
                            first_error = Some("快速回测任务线程 panic".to_string());
                        }
                    }
                }
            }
        }
        first_error.map_or(Ok(completed), Err)
    })
}

fn fast_backtest_parallelism(jobs: usize, available: usize) -> usize {
    jobs.min(available.max(1))
        .clamp(1, MAX_FAST_BACKTEST_WORKERS)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn manifest_paths_resolve_from_manifest_and_reject_invalid_optional_paths() {
        let path = Path::new("deploy/fast.json");
        let jobs = parse_fast_backtest_manifest(
            path,
            r#"{"schema_version":1,"jobs":[{"runtime":"runtime.toml","bars":"bars.json","market_spec":"spec.json","dataset_manifest":"dataset.json"}]}"#,
        )
        .unwrap();
        assert_eq!(jobs[0].runtime, PathBuf::from("deploy/runtime.toml"));
        assert_eq!(jobs[0].bars, PathBuf::from("deploy/bars.json"));
        assert_eq!(jobs[0].market_spec, Some(PathBuf::from("deploy/spec.json")));
        assert_eq!(
            jobs[0].dataset_manifest,
            Some(PathBuf::from("deploy/dataset.json"))
        );
        assert!(parse_fast_backtest_manifest(
            path,
            r#"{"schema_version":1,"jobs":[{"runtime":"r.toml","bars":"b.json","dataset_manifest":" "}]}"#,
        )
        .is_err());
        for malformed_market_spec in [r#""""#, "null", "42", "{}"] {
            let payload = format!(
                r#"{{"schema_version":1,"jobs":[{{"runtime":"r.toml","bars":"b.json","market_spec":{malformed_market_spec}}}]}}"#
            );
            assert!(
                parse_fast_backtest_manifest(path, &payload).is_err(),
                "market_spec must reject {malformed_market_spec}"
            );
        }
        let jobs = parse_fast_backtest_manifest(
            path,
            r#"{"schema_version":1,"jobs":[{"runtime":"r.toml","bars":"b.json"}]}"#,
        )
        .unwrap();
        assert_eq!(jobs[0].market_spec, None);
        for invalid_version in [
            "{}",
            r#"{"schema_version":2,"jobs":[]}"#,
            r#"{"schema_version":"1","jobs":[]}"#,
        ] {
            assert!(parse_fast_backtest_manifest(path, invalid_version).is_err());
        }
        assert!(parse_fast_backtest_manifest(
            path,
            r#"{"schema_version":1,"jobs":[],"typo":true}"#,
        )
        .is_err());
        assert!(parse_fast_backtest_manifest(
            path,
            r#"{"schema_version":1,"jobs":[{"runtime":"r.toml","bars":"b.json","bar":"bars.json"}]}"#,
        )
        .is_err());
    }

    #[test]
    fn bounded_batch_runner_completes_all_jobs_and_surfaces_first_error() {
        let jobs = (0..12)
            .map(|index| FastBacktestJob {
                index,
                runtime: PathBuf::new(),
                bars: PathBuf::new(),
                market_spec: None,
                dataset_manifest: None,
            })
            .collect::<Vec<_>>();
        let completed = AtomicUsize::new(0);
        let error = run_fast_backtest_jobs(&jobs, 64, |job| {
            completed.fetch_add(1, Ordering::SeqCst);
            if job.index == 3 || job.index == 8 {
                Err(format!("failure-{}", job.index))
            } else {
                Ok(())
            }
        })
        .unwrap_err();
        assert_eq!(completed.load(Ordering::SeqCst), jobs.len());
        assert_eq!(error, "jobs[3] failure-3");
    }

    #[test]
    fn worker_limit_is_bounded_and_never_zero() {
        assert_eq!(fast_backtest_parallelism(256, 64), 8);
        assert_eq!(fast_backtest_parallelism(3, 64), 3);
        assert_eq!(fast_backtest_parallelism(4, 0), 1);
    }
}
