use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

pub(crate) fn list_backtest_summary_paths(runs_dir: &Path) -> Result<Vec<PathBuf>, String> {
    let mut summaries = Vec::new();
    if !runs_dir.is_dir() {
        return Ok(summaries);
    }
    for entry in std::fs::read_dir(runs_dir)
        .map_err(|error| format!("读取回测结果目录失败 {}: {error}", runs_dir.display()))?
    {
        let entry = entry.map_err(|error| format!("读取回测结果目录项失败: {error}"))?;
        let path = entry.path();
        if path
            .file_name()
            .and_then(|name| name.to_str())
            .is_some_and(|name| name.ends_with(".summary.json"))
        {
            summaries.push(path);
        }
    }
    summaries.sort_by(|left, right| {
        let left_modified = std::fs::metadata(left)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        let right_modified = std::fs::metadata(right)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(UNIX_EPOCH);
        left_modified
            .cmp(&right_modified)
            .then_with(|| left.cmp(right))
    });
    // Keep prior summaries readable, but when a schema-only upgrade reuses the same RunManifest,
    // list only the newest versioned summary instead of showing each schema generation as a run.
    let current_paths: std::collections::BTreeSet<PathBuf> = summaries
        .iter()
        .filter_map(|path| {
            let name = path.file_name()?.to_str()?;
            let stem = name.strip_suffix(".v7.summary.json")?;
            Some([
                path.with_file_name(format!("{stem}.v6.summary.json")),
                path.with_file_name(format!("{stem}.summary.json")),
            ])
        })
        .flatten()
        .collect();
    summaries.retain(|path| !current_paths.contains(path));
    Ok(summaries)
}
