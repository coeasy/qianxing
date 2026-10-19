//! Canonical output path comparison and backtest report sibling paths.

use std::path::{Path, PathBuf};

pub(crate) fn same_output_target(left: &std::path::Path, right: &std::path::Path) -> bool {
    canonical_output_target(left)
        .zip(canonical_output_target(right))
        .is_some_and(|(left, right)| left == right)
}

fn canonical_output_target(path: &std::path::Path) -> Option<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().ok()?.join(path)
    };
    let mut normalized = PathBuf::new();
    for component in absolute.components() {
        match component {
            std::path::Component::CurDir => {}
            std::path::Component::ParentDir => {
                normalized.pop();
            }
            other => normalized.push(other.as_os_str()),
        }
    }
    let mut ancestor = normalized.clone();
    let mut suffix = Vec::new();
    while !ancestor.exists() {
        suffix.push(ancestor.file_name()?.to_os_string());
        if !ancestor.pop() {
            return None;
        }
    }
    let mut target = ancestor.canonicalize().ok()?;
    for component in suffix.into_iter().rev() {
        target.push(component);
    }
    Some(target)
}

pub(crate) struct ReportArtifactPaths {
    pub(crate) equity_csv: PathBuf,
    pub(crate) fills_csv: PathBuf,
    pub(crate) html: PathBuf,
}

pub(crate) fn report_artifact_paths(summary_path: &Path) -> Result<ReportArtifactPaths, String> {
    let name = summary_path.file_name().and_then(|name| name.to_str());
    if name == Some("summary.json") {
        return Ok(ReportArtifactPaths {
            equity_csv: summary_path.with_file_name("equity.csv"),
            fills_csv: summary_path.with_file_name("fills.csv"),
            html: summary_path.with_file_name("report.html"),
        });
    }
    let stem = name
        .and_then(|name| name.strip_suffix(".summary.json"))
        .ok_or_else(|| {
            format!(
                "报告摘要文件名不是 *.summary.json: {}",
                summary_path.display()
            )
        })?;
    Ok(ReportArtifactPaths {
        equity_csv: summary_path.with_file_name(format!("{stem}.equity.csv")),
        fills_csv: summary_path.with_file_name(format!("{stem}.fills.csv")),
        html: summary_path.with_file_name(format!("{stem}.report.html")),
    })
}
