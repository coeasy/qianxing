from pathlib import Path


def replace_once(path: str, old: str, new: str) -> None:
    p = Path(path)
    text = p.read_text()
    if new in text:
        return
    count = text.count(old)
    if count != 1:
        raise SystemExit(f"{path}: expected exactly one guarded match, got {count}")
    p.write_text(text.replace(old, new, 1))


main = "crates/qx-cli/src/main.rs"

replace_once(
    main,
    """fn resolve_runtime_relative_path(runtime_path: &Path, configured: &str) -> PathBuf {
    let path = Path::new(configured);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        runtime_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}
""",
    """fn is_portable_absolute_path(configured: &str) -> bool {
    let bytes = configured.as_bytes();
    Path::new(configured).is_absolute()
        || configured.starts_with("\\\\")
        || (bytes.len() >= 3
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && matches!(bytes[2], b'/' | b'\\\\'))
}

fn resolve_runtime_relative_path(runtime_path: &Path, configured: &str) -> PathBuf {
    let path = Path::new(configured);
    if is_portable_absolute_path(configured) {
        path.to_path_buf()
    } else {
        runtime_path
            .parent()
            .unwrap_or_else(|| Path::new("."))
            .join(path)
    }
}
""",
)

replace_once(
    main,
    """    if Path::new(configured).is_absolute() || config_candidate.exists() {
        return config_candidate;
    }
""",
    """    if is_portable_absolute_path(configured) || config_candidate.exists() {
        return config_candidate;
    }
""",
)

replace_once(
    main,
    """        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        config.scheduler.jobs_path = workspace_root
""",
    """        let workspace_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("..").join("..");
        let execution = config
            .workers
            .iter_mut()
            .find(|worker| worker.id == "paper-execution")
            .unwrap();
        execution.instrument_spec_path = Some(
            workspace_root
                .join("deploy")
                .join("qianxing.binance.spot.spec.json")
                .to_string_lossy()
                .into_owned(),
        );
        config.scheduler.jobs_path = workspace_root
""",
)
