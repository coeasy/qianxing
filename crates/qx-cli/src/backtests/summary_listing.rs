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
    // list only the newest generation instead of showing each schema generation as a run.
    let mut newest_generation_by_stem = std::collections::BTreeMap::<String, u32>::new();
    for path in &summaries {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some((stem, generation)) = versioned_summary_stem(name) else {
            continue;
        };
        newest_generation_by_stem
            .entry(stem.to_owned())
            .and_modify(|current| *current = (*current).max(generation))
            .or_insert(generation);
    }
    summaries.retain(|path| {
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            return true;
        };
        if let Some((stem, generation)) = versioned_summary_stem(name) {
            return newest_generation_by_stem.get(stem) == Some(&generation);
        }
        name.strip_suffix(".summary.json")
            .is_none_or(|stem| !newest_generation_by_stem.contains_key(stem))
    });
    Ok(summaries)
}

fn versioned_summary_stem(name: &str) -> Option<(&str, u32)> {
    let stem_and_generation = name.strip_suffix(".summary.json")?;
    let (stem, generation) = stem_and_generation.rsplit_once(".v")?;
    if stem.is_empty()
        || generation.is_empty()
        || !generation.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    Some((stem, generation.parse().ok()?))
}
