//! Portable bundle source resolution, writing, and bounded artifact reads.

use super::super::{
    copy_to, file_sha256, BundleFile, BundleReference, ExperimentEvidenceBundleManifest,
};
use std::collections::{BTreeMap, HashMap};
use std::fs::File;
use std::io::BufReader;
use std::path::{Component, Path, PathBuf};

pub(super) fn resolve_source(
    reference: &str,
    relative_base: Option<&Path>,
) -> Result<PathBuf, String> {
    let candidate = PathBuf::from(reference);
    if candidate.is_absolute() {
        return candidate
            .canonicalize()
            .map_err(|error| format!("解析实验源文件失败 {}: {error}", candidate.display()));
    }
    let cwd = std::env::current_dir().map_err(|error| format!("读取当前目录失败: {error}"))?;
    let mut candidates = vec![cwd.join(&candidate)];
    if let Some(base) = relative_base {
        let path = base.join(&candidate);
        if !candidates.contains(&path) {
            candidates.push(path);
        }
    }
    let existing = candidates
        .into_iter()
        .filter_map(|path| path.canonicalize().ok())
        .filter(|path| path.is_file())
        .collect::<Vec<_>>();
    let first = existing
        .first()
        .cloned()
        .ok_or_else(|| format!("相对实验源文件不存在: {reference}（已尝试当前目录和报告目录）"))?;
    let first_digest = file_sha256(&first)?;
    for candidate in existing.iter().skip(1) {
        if file_sha256(candidate)? != first_digest {
            return Err(format!(
                "相对实验源文件解析存在歧义，候选内容不同: {reference}"
            ));
        }
    }
    Ok(first)
}
pub(super) struct BundleWriter<'a> {
    pub(super) root: &'a Path,
    pub(super) files: Vec<BundleFile>,
    pub(super) references: Vec<BundleReference>,
    reference_targets: BTreeMap<String, String>,
    source_targets: HashMap<PathBuf, String>,
    next_source: usize,
}

impl<'a> BundleWriter<'a> {
    pub(super) fn new(root: &'a Path) -> Self {
        Self {
            root,
            files: Vec::new(),
            references: Vec::new(),
            reference_targets: BTreeMap::new(),
            source_targets: HashMap::new(),
            next_source: 1,
        }
    }

    pub(super) fn add_file(&mut self, file: BundleFile) {
        self.files.push(file);
    }

    pub(super) fn cache_source(&mut self, source: &Path, relative: &str) {
        if let Ok(source) = source.canonicalize() {
            self.source_targets.insert(source, relative.to_string());
        }
    }

    pub(super) fn add_reference(&mut self, locator: &str, relative: &str) -> Result<(), String> {
        let digest = qx_strategy::sha256_hex(locator.as_bytes());
        match self.reference_targets.get(&digest) {
            Some(existing) if existing != relative => {
                Err("同一工件 locator 被映射到不同实验包文件".into())
            }
            Some(_) => Ok(()),
            None => {
                self.reference_targets
                    .insert(digest.clone(), relative.to_string());
                self.references.push(BundleReference {
                    locator_sha256: digest,
                    path: relative.to_string(),
                });
                Ok(())
            }
        }
    }

    pub(super) fn copy_reference(&mut self, locator: &str, base: &Path) -> Result<String, String> {
        if locator.trim().is_empty() {
            return Err("实验报告含空工件 locator".into());
        }
        let source = resolve_source(locator, Some(base))?;
        let relative = if let Some(relative) = self.source_targets.get(&source) {
            relative.clone()
        } else {
            let extension = source
                .extension()
                .and_then(|value| value.to_str())
                .filter(|extension| {
                    !extension.is_empty()
                        && extension.bytes().all(|byte| byte.is_ascii_alphanumeric())
                })
                .unwrap_or("bin");
            let relative = format!("artifacts/source-{:05}.{extension}", self.next_source);
            self.next_source += 1;
            let file = copy_to(&source, self.root, &relative)?;
            self.files.push(file);
            self.source_targets.insert(source.clone(), relative.clone());
            relative
        };
        self.add_reference(locator, &relative)?;
        self.add_reference(&source.to_string_lossy(), &relative)?;
        Ok(relative)
    }
}

pub(super) struct BundleArtifactReader {
    root: PathBuf,
    paths_by_locator: HashMap<String, String>,
    paths_by_digest: HashMap<String, String>,
    declared_paths: std::collections::HashSet<String>,
}

impl BundleArtifactReader {
    pub(super) fn new(
        root: &Path,
        bundle: &ExperimentEvidenceBundleManifest,
    ) -> Result<Self, String> {
        let root = root
            .canonicalize()
            .map_err(|error| format!("解析实验包根目录失败: {error}"))?;
        let mut paths_by_digest = HashMap::new();
        let mut declared_paths = std::collections::HashSet::new();
        for file in &bundle.files {
            declared_paths.insert(file.path.clone());
            paths_by_digest
                .entry(file.sha256.clone())
                .or_insert_with(|| file.path.clone());
        }
        let paths_by_locator = bundle
            .references
            .iter()
            .map(|reference| (reference.locator_sha256.clone(), reference.path.clone()))
            .collect();
        Ok(Self {
            root,
            paths_by_locator,
            paths_by_digest,
            declared_paths,
        })
    }

    fn resolve_locator(&self, reference: &str) -> Result<PathBuf, qx_app::AppError> {
        let locator_digest = qx_strategy::sha256_hex(reference.as_bytes());
        if let Some(relative) = self.paths_by_locator.get(&locator_digest) {
            return self.resolve_relative(relative);
        }
        let relative = Path::new(reference);
        if relative.is_absolute()
            || relative.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::InvalidInput,
                "实验包 ArtifactReader 拒绝未登记的绝对或越界 locator",
            ));
        }
        if !self.declared_paths.contains(reference) {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::StorageFailure,
                "实验包 ArtifactReader 拒绝未登记的 locator",
            ));
        }
        self.resolve_relative(reference)
    }

    fn resolve_relative(&self, reference: &str) -> Result<PathBuf, qx_app::AppError> {
        let path = Path::new(reference);
        if path.is_absolute()
            || path.components().any(|component| {
                matches!(
                    component,
                    Component::ParentDir | Component::RootDir | Component::Prefix(_)
                )
            })
        {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::InvalidInput,
                "实验包 ArtifactLocation 必须是包内相对路径",
            ));
        }
        let resolved =
            self.root.join(path).canonicalize().map_err(|error| {
                qx_app::AppError::from_io("解析实验包 ArtifactLocation", &error)
            })?;
        if !resolved.starts_with(&self.root) || !resolved.is_file() {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::InvalidInput,
                "实验包 ArtifactLocation 越界或不是普通文件",
            ));
        }
        Ok(resolved)
    }

    fn open_path(
        &self,
        relative: &str,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        let path = self.resolve_relative(relative)?;
        let file = File::open(&path)
            .map_err(|error| qx_app::AppError::from_io("打开实验包 Artifact", &error))?;
        let metadata = file
            .metadata()
            .map_err(|error| qx_app::AppError::from_io("读取实验包 Artifact 元数据", &error))?;
        if !metadata.is_file() {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::StorageFailure,
                "实验包 Artifact 必须是普通文件",
            ));
        }
        Ok(qx_app::artifact_store::ArtifactStream::new(
            metadata.len(),
            BufReader::new(file),
        ))
    }
}

impl qx_app::artifact_store::ArtifactReader for BundleArtifactReader {
    fn open(
        &self,
        reference: &str,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        let path = self.resolve_locator(reference)?;
        let relative = path
            .strip_prefix(&self.root)
            .map_err(|_| {
                qx_app::AppError::new(
                    qx_app::AppErrorCategory::InvalidInput,
                    "实验包 locator 越界",
                )
            })?
            .to_string_lossy()
            .replace('\\', "/");
        self.open_path(&relative)
    }

    fn open_by_id(
        &self,
        artifact_id: &qx_app::artifact_store::ArtifactId,
        legacy_reference: Option<&str>,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        if let Some(relative) = self.paths_by_digest.get(artifact_id.as_str()) {
            return self.open_path(relative);
        }
        let Some(reference) = legacy_reference else {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::StorageFailure,
                format!("实验包中不存在 ArtifactId {}", artifact_id.as_str()),
            ));
        };
        self.open(reference)
    }
}
