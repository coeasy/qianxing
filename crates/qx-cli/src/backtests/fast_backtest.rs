//! `fast-backtest` manifest 驱动的离线快回测入口。

use super::*;

/// 一条已解析的作业：序号、runtime 配置、bars 文件，以及可选的 market spec。
pub(crate) struct FastBacktestJob {
    pub(crate) index: usize,
    pub(crate) runtime: PathBuf,
    pub(crate) bars: PathBuf,
    pub(crate) market_spec: Option<PathBuf>,
}

/// manifest 的**解析段**：只做读文件与形状判定，不起线程、不落产物。
///
/// 拆出来是为了让样例模板能被用例读一次（V13 R1-A6）：执行段在 `run_strategy_backtest`
/// 里跑完整回测，用例付不起那个代价，于是"这份 manifest 有没有人读过"就会长期没人回答。
pub(crate) fn parse_fast_backtest_manifest(
    manifest_path: &Path,
) -> Result<Vec<FastBacktestJob>, String> {
    let payload = std::fs::read_to_string(manifest_path).map_err(|error| {
        format!(
            "读取快速回测 manifest 失败 {}: {error}",
            manifest_path.display()
        )
    })?;
    let document: serde_json::Value = serde_json::from_str(&payload)
        .map_err(|error| format!("快速回测 manifest JSON 无效: {error}"))?;
    let jobs = document
        .get("jobs")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| "快速回测 manifest 必须包含 jobs 数组".to_string())?;
    if jobs.is_empty() || jobs.len() > 256 {
        return Err("快速回测 jobs 数量必须在 1..=256 内".into());
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
    let mut parsed = Vec::with_capacity(jobs.len());
    for (index, job) in jobs.iter().enumerate() {
        let object = job
            .as_object()
            .ok_or_else(|| format!("快速回测 jobs[{index}] 必须是对象"))?;
        let runtime = object
            .get("runtime")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("快速回测 jobs[{index}] 缺少 runtime"))?;
        let bars = object
            .get("bars")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .ok_or_else(|| format!("快速回测 jobs[{index}] 缺少 bars"))?;
        let spec = object
            .get("market_spec")
            .and_then(serde_json::Value::as_str)
            .filter(|value| !value.trim().is_empty())
            .map(resolve);
        parsed.push(FastBacktestJob {
            index,
            runtime: resolve(runtime),
            bars: resolve(bars),
            market_spec: spec,
        });
    }
    Ok(parsed)
}

pub(crate) fn run_fast_backtest_manifest(manifest_path: &Path) -> Result<(), String> {
    let parsed = parse_fast_backtest_manifest(manifest_path)?;
    let jobs = parsed.len();
    let results = std::thread::scope(|scope| {
        let mut handles = Vec::with_capacity(parsed.len());
        for FastBacktestJob {
            index,
            runtime,
            bars,
            market_spec,
        } in parsed
        {
            handles.push(scope.spawn(move || {
                run_strategy_backtest(&runtime, &bars, market_spec.as_deref())
                    .map(|_| index)
                    .map_err(|error| format!("jobs[{index}] {error}"))
            }));
        }
        handles
            .into_iter()
            .map(|handle| {
                handle
                    .join()
                    .map_err(|_| "快速回测任务线程 panic".to_string())?
            })
            .collect::<Result<Vec<_>, String>>()
    })?;
    println!(
        "[Fast Backtest] manifest={} jobs={} completed={}",
        manifest_path.display(),
        jobs,
        results.len()
    );
    Ok(())
}
