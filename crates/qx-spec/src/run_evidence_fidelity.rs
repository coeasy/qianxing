//! Structured source classes for run-report fields.

use serde::{Deserialize, Serialize};

/// Machine-readable provenance classes for paths already present in the summary.
/// These labels describe evidence type; they do not certify accuracy or recompute values.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunEvidenceFidelity {
    /// Artifact root for every JSON Pointer: usually `summary`, or `run` for payload-local paths.
    pub source_artifact: String,
    pub observed: Vec<String>,
    pub configured: Vec<String>,
    pub estimated: Vec<String>,
    pub computed: Vec<String>,
    pub absent: Vec<String>,
}

pub(crate) fn validate(fidelity: Option<&RunEvidenceFidelity>) -> Result<(), String> {
    let Some(fidelity) = fidelity else {
        return Ok(());
    };
    if fidelity.source_artifact.trim().is_empty() {
        return Err("运行证据包 fidelity.source_artifact 不能为空".into());
    }
    if !matches!(fidelity.source_artifact.as_str(), "summary" | "run") {
        return Err(format!(
            "运行证据包 fidelity.source_artifact 不支持: {}",
            fidelity.source_artifact
        ));
    }
    let groups = [
        ("observed", &fidelity.observed),
        ("configured", &fidelity.configured),
        ("estimated", &fidelity.estimated),
        ("computed", &fidelity.computed),
        ("absent", &fidelity.absent),
    ];
    let mut seen = std::collections::BTreeSet::new();
    for (group, paths) in groups {
        if paths
            .iter()
            .any(|path| path.trim().is_empty() || !path.starts_with('/'))
        {
            return Err(format!(
                "运行证据包 fidelity.{group} 必须包含非空 JSON Pointer"
            ));
        }
        for path in paths {
            if !seen.insert(path.as_str()) {
                return Err(format!("运行证据包 fidelity 路径重复分类: {path}"));
            }
        }
    }
    if fidelity.observed.is_empty()
        || fidelity.configured.is_empty()
        || fidelity.estimated.is_empty()
        || fidelity.computed.is_empty()
    {
        return Err(
            "运行证据包 fidelity 必须分别登记 observed/configured/estimated/computed 路径".into(),
        );
    }
    Ok(())
}
