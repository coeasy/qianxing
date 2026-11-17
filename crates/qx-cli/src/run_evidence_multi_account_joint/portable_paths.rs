use super::{read_run_bundle, JointEvidenceManifest};
use qx_app::{MultiAccountJointBacktestOutcome, MultiAccountJointBacktestSpec};
use qx_spec::EvidenceGeneration;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
pub(super) fn bind_portable_market_paths(
    spec: &mut MultiAccountJointBacktestSpec,
    account: &qx_app::JointBacktestAccount,
    evidence_dir: &str,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    let spec_account = spec
        .accounts
        .iter_mut()
        .find(|candidate| {
            candidate.exchange_id == account.exchange_id
                && candidate.account_id == account.account_id
        })
        .ok_or("便携规格缺少账户")?;
    for market in &mut spec_account.markets {
        let component = evidence
            .dataset
            .components
            .iter()
            .find(|component| component.role == format!("market:{}", market.instrument))
            .ok_or_else(|| format!("账户市场 {} 缺少 Evidence 输入组件", market.instrument))?;
        let relative = validate_portable_relative(&component.path)?;
        market.bars_path = format!("{evidence_dir}/{relative}");
        market.dataset_manifest_path = component
            .quality_manifest_path
            .as_deref()
            .map(|path| {
                validate_portable_relative(path)
                    .map(|relative| format!("{evidence_dir}/{relative}"))
            })
            .transpose()?;
    }
    Ok(())
}

pub(super) fn verify_portable_market_paths(
    package_root: &Path,
    evidence_dir: &str,
    account: &qx_app::JointBacktestAccount,
    evidence: &qx_spec::RunEvidencePayload,
) -> Result<(), String> {
    if account.markets.len() != evidence.dataset.components.len() {
        return Err("联合账户规格市场数与 Evidence 组件数不一致".into());
    }
    for market in &account.markets {
        let component = evidence
            .dataset
            .components
            .iter()
            .find(|component| component.role == format!("market:{}", market.instrument))
            .ok_or_else(|| format!("Evidence 缺少市场 {}", market.instrument))?;
        let expected_bars = format!(
            "{evidence_dir}/{}",
            validate_portable_relative(&component.path)?
        );
        if market.bars_path != expected_bars {
            return Err(format!(
                "市场 {} 的便携 BarFrame 路径不一致",
                market.instrument
            ));
        }
        let expected_quality = component
            .quality_manifest_path
            .as_deref()
            .map(|path| {
                validate_portable_relative(path)
                    .map(|relative| format!("{evidence_dir}/{relative}"))
            })
            .transpose()?;
        if market.dataset_manifest_path != expected_quality {
            return Err(format!(
                "市场 {} 的便携质量清单路径不一致",
                market.instrument
            ));
        }
        checked_package_file(package_root, &market.bars_path)?;
        if let Some(path) = &market.dataset_manifest_path {
            checked_package_file(package_root, path)?;
        }
    }
    Ok(())
}

pub(super) fn market_component(
    spec: &MultiAccountJointBacktestSpec,
    exchange_id: &str,
    account_id: &str,
    instrument: &str,
    package_root: &Path,
) -> Result<(String, Option<String>), String> {
    let account = spec
        .accounts
        .iter()
        .find(|account| account.exchange_id == exchange_id && account.account_id == account_id)
        .ok_or("联合便携规格缺少账户")?;
    let market = account
        .markets
        .iter()
        .find(|market| market.instrument == instrument)
        .ok_or("联合便携规格缺少市场")?;
    let bars = checked_package_file(package_root, &market.bars_path)?
        .canonicalize()
        .map_err(|error| format!("解析便携 BarFrame 失败: {error}"))?;
    let quality = market
        .dataset_manifest_path
        .as_deref()
        .map(|path| {
            checked_package_file(package_root, path)?
                .canonicalize()
                .map(|path| path.to_string_lossy().into_owned())
                .map_err(|error| format!("解析便携质量清单失败: {error}"))
        })
        .transpose()?;
    Ok((bars.to_string_lossy().into_owned(), quality))
}

pub(super) fn parent_artifact_sources(
    outcome: &MultiAccountJointBacktestOutcome,
) -> Result<Vec<(String, PathBuf, String)>, String> {
    let locations = outcome
        .artifact_locations
        .as_ref()
        .ok_or("联合账户结果缺少 artifact_locations")?;
    let expected = [
        (
            "joint_portfolio",
            &outcome.artifact_path,
            "parent/portfolio.json",
        ),
        ("joint_equity", &outcome.equity_path, "parent/equity.json"),
        ("joint_fills", &outcome.fills_path, "parent/fills.json"),
        (
            "run_manifest",
            outcome
                .run_manifest_path
                .as_ref()
                .ok_or("缺少父级 RunManifest")?,
            "parent/run-manifest.json",
        ),
    ];
    let mut files = Vec::with_capacity(5);
    for (name, source, relative) in expected {
        if locations.get(name).map(String::as_str) != Some(source.as_str()) {
            return Err(format!(
                "联合账户 artifact_locations.{name} 与 outcome 不一致"
            ));
        }
        files.push((
            name.to_string(),
            PathBuf::from(source),
            relative.to_string(),
        ));
    }
    files.push((
        "run_record".into(),
        PathBuf::from(
            outcome
                .run_record
                .as_deref()
                .ok_or("缺少联合父级 RunRecord")?,
        ),
        "parent/run-record.json".into(),
    ));
    Ok(files)
}

pub(super) fn build_path_map_reader(
    root: &Path,
    manifest: &JointEvidenceManifest,
    outcome: &MultiAccountJointBacktestOutcome,
    bundle_paths: &BTreeMap<(String, String), PathBuf>,
) -> Result<PortablePathMapReader, String> {
    let mut reader = PortablePathMapReader::new(root.to_path_buf());
    for (name, source, _) in parent_artifact_sources(outcome)? {
        let target = manifest
            .parent_artifacts
            .get(&name)
            .ok_or_else(|| format!("manifest 缺少父级产物 {name}"))?;
        reader.map_reference(&source, checked_package_file(root, &target.path)?)?;
    }
    for account in &outcome.accounts {
        let key = (account.exchange_id.clone(), account.account_id.clone());
        let bundle_path = bundle_paths
            .get(&key)
            .ok_or("账户便携 EvidenceBundle 缺失")?;
        let bundle = read_run_bundle(bundle_path)?;
        let EvidenceGeneration::Run(evidence) = &bundle.generation else {
            return Err("子账户便携 EvidenceBundle 类型无效".into());
        };
        let evidence_dir = format!("accounts/{}", account_directory_id(&key.0, &key.1));
        let summary_ref = evidence
            .artifacts
            .iter()
            .find(|artifact| artifact.name == "summary")
            .ok_or("子账户 EvidenceBundle 缺少 summary")?;
        let summary_path = checked_package_file(
            root,
            &format!(
                "{evidence_dir}/{}",
                validate_portable_relative(&summary_ref.path)?
            ),
        )?;
        reader.map_reference(&account.outcome.artifact_path, summary_path.clone())?;
        let summary: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&summary_path)
                .map_err(|error| format!("读取子账户 summary 失败: {error}"))?,
        )
        .map_err(|error| format!("子账户 summary JSON 无效: {error}"))?;
        for (name, pointer) in [
            ("run_manifest", "/run_manifest_path"),
            ("equity", "/equity_path"),
            ("equity_exact", "/equity_exact_path"),
            ("fills", "/fills_path"),
            ("event_trace", "/event_trace_path"),
            (
                "instrument_equity_contributions",
                "/instrument_equity_contributions_path",
            ),
        ] {
            let Some(source) = summary.pointer(pointer).and_then(serde_json::Value::as_str) else {
                continue;
            };
            let Some(portable) = evidence
                .artifacts
                .iter()
                .find(|artifact| artifact.name == name)
            else {
                return Err(format!("子账户 Evidence 缺少 {name} 工件"));
            };
            let target = checked_package_file(
                root,
                &format!(
                    "{evidence_dir}/{}",
                    validate_portable_relative(&portable.path)?
                ),
            )?;
            reader.map_reference(source, target)?;
        }
        for component in &evidence.dataset.components {
            let instrument = component
                .role
                .strip_prefix("market:")
                .ok_or("子账户 Evidence market role 无效")?;
            let source = summary
                .get("market_data")
                .and_then(serde_json::Value::as_array)
                .and_then(|items| {
                    items.iter().find(|item| {
                        item.get("instrument").and_then(serde_json::Value::as_str)
                            == Some(instrument)
                    })
                })
                .and_then(|item| item.get("path"))
                .and_then(serde_json::Value::as_str)
                .ok_or_else(|| format!("子账户 summary 缺少 {instrument} 输入路径"))?;
            let target = checked_package_file(
                root,
                &format!(
                    "{evidence_dir}/{}",
                    validate_portable_relative(&component.path)?
                ),
            )?;
            reader.map_reference(source, target)?;
            if let Some(quality_source) = summary
                .pointer(&format!("/dataset_quality/{instrument}/manifest_path"))
                .and_then(serde_json::Value::as_str)
            {
                let quality = component
                    .quality_manifest_path
                    .as_deref()
                    .ok_or("子账户 summary 有质量清单，但 portable 组件没有路径")?;
                let target = checked_package_file(
                    root,
                    &format!("{evidence_dir}/{}", validate_portable_relative(quality)?),
                )?;
                reader.map_reference(quality_source, target)?;
            }
        }
    }
    Ok(reader)
}

pub(super) struct PortablePathMapReader {
    root: PathBuf,
    paths: BTreeMap<String, PathBuf>,
}

impl PortablePathMapReader {
    fn new(root: PathBuf) -> Self {
        Self {
            root,
            paths: BTreeMap::new(),
        }
    }

    fn map_reference(&mut self, source: impl AsRef<Path>, target: PathBuf) -> Result<(), String> {
        let key = path_key(&source.as_ref().to_string_lossy());
        if let Some(existing) = self.paths.get(&key) {
            if existing != &target {
                return Err("一个来源工件路径映射到多个便携目标".into());
            }
        } else {
            self.paths.insert(key, target);
        }
        Ok(())
    }

    fn resolve(&self, reference: &str) -> Result<PathBuf, qx_app::AppError> {
        if let Some(path) = self.paths.get(&path_key(reference)) {
            return Ok(path.clone());
        }
        if Path::new(reference).is_absolute() {
            return Err(qx_app::AppError::new(
                qx_app::AppErrorCategory::StorageFailure,
                format!("便携报告引用了未登记的来源绝对路径: {reference}"),
            ));
        }
        checked_package_file(&self.root, reference)
            .map_err(|error| qx_app::AppError::new(qx_app::AppErrorCategory::StorageFailure, error))
    }
}

impl qx_app::artifact_store::ArtifactReader for PortablePathMapReader {
    fn open(
        &self,
        reference: &str,
    ) -> Result<qx_app::artifact_store::ArtifactStream, qx_app::AppError> {
        let path = self.resolve(reference)?;
        qx_app::artifact_store::ArtifactReader::open(
            &qx_app::artifact_store::FileArtifactReader,
            &path.to_string_lossy(),
        )
    }

    fn equivalent_references(&self, left: &str, right: &str) -> Result<bool, qx_app::AppError> {
        if path_key(left) == path_key(right) {
            return Ok(true);
        }
        let left = self
            .resolve(left)?
            .canonicalize()
            .map_err(|error| qx_app::AppError::from_io("解析左侧 portable 工件引用", &error))?;
        let right = self
            .resolve(right)?
            .canonicalize()
            .map_err(|error| qx_app::AppError::from_io("解析右侧 portable 工件引用", &error))?;
        Ok(left == right)
    }
}

fn path_key(value: &str) -> String {
    let normalized = value.replace('/', "\\");
    if cfg!(windows) {
        normalized.to_ascii_lowercase()
    } else {
        normalized
    }
}

pub(super) fn account_directory_id(exchange_id: &str, account_id: &str) -> String {
    let value = format!("{exchange_id}\0{account_id}");
    qx_strategy::sha256_hex(value.as_bytes())
}

pub(super) fn validate_portable_relative(value: &str) -> Result<String, String> {
    let path = Path::new(value);
    if value.trim().is_empty()
        || path.is_absolute()
        || path.components().any(|component| {
            matches!(
                component,
                std::path::Component::ParentDir
                    | std::path::Component::RootDir
                    | std::path::Component::Prefix(_)
            )
        })
        || value.contains('\\')
    {
        return Err(format!(
            "portable 证据引用必须是安全的包内相对路径: {value}"
        ));
    }
    Ok(value.to_string())
}

pub(super) fn checked_package_file(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = validate_portable_relative(relative)?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("解析包内文件失败: {error}"))?;
    if !path.starts_with(root) || !path.is_file() {
        return Err("便携证据引用越界或不是普通文件".into());
    }
    Ok(path)
}

pub(super) fn checked_package_dir(root: &Path, relative: &str) -> Result<PathBuf, String> {
    let relative = validate_portable_relative(relative)?;
    let path = root
        .join(relative)
        .canonicalize()
        .map_err(|error| format!("解析包内目录失败: {error}"))?;
    if !path.starts_with(root) || !path.is_dir() {
        return Err("便携证据引用越界或不是目录".into());
    }
    Ok(path)
}
