//! 固定输入下的 CLI 过程行为回放：比较退出码、规范化 stdout、result_hash 多重集与产物文件集。
//! 它是 WP-2.1 行为等价矩阵的一组可重复基线；`--help` 覆盖仍单独处理，不计入本矩阵。

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

#[derive(Debug, PartialEq, Eq)]
struct Observation {
    exit_code: i32,
    stdout: String,
    result_hashes: Vec<String>,
    artifacts: Vec<String>,
    artifact_digests: Vec<(String, u64)>,
}

fn binary() -> PathBuf {
    PathBuf::from(env!("CARGO_BIN_EXE_qx-cli"))
}

fn temp_dir(label: &str, replay: usize) -> PathBuf {
    static SERIAL: AtomicUsize = AtomicUsize::new(0);
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let path = root.join("target").join(format!(
        "qx-cli-parity-{label}-{replay}-{}-{serial}",
        std::process::id(),
    ));
    std::fs::create_dir(&path).expect("create isolated CLI working directory");
    path
}

fn artifact_files(root: &Path) -> Vec<String> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(dir) = pending.pop() {
        let Ok(entries) = std::fs::read_dir(dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                pending.push(path);
            } else if let Ok(relative) = path.strip_prefix(root) {
                files.push(relative.to_string_lossy().replace('\\', "/"));
            }
        }
    }
    files.sort();
    files
}

fn artifact_digests(root: &Path, files: &[String]) -> Vec<(String, u64)> {
    files
        .iter()
        .map(|relative| {
            let bytes = std::fs::read(root.join(relative))
                .unwrap_or_else(|error| panic!("read CLI artifact {relative}: {error}"));
            // Stable FNV-1a is sufficient here: this is a replay equality fingerprint, not a
            // signature or an artifact identity exposed to users.
            let digest = bytes.iter().fold(0xcbf29ce484222325_u64, |hash, byte| {
                (hash ^ u64::from(*byte)).wrapping_mul(0x100000001b3)
            });
            (relative.clone(), digest)
        })
        .collect()
}

fn normalize_stdout(args: &[&str], working_dir: &Path, stdout: &str) -> String {
    let cwd = working_dir.to_string_lossy();
    let slash = cwd.replace('\\', "/");
    let backslash = cwd.replace('/', "\\");
    let stdout = stdout
        .replace(cwd.as_ref(), "<workdir>")
        .replace(&slash, "<workdir>")
        .replace(&backslash, "<workdir>");
    if args.first().copied() == Some("run") {
        return normalize_stdout(&args[1..], working_dir, &stdout);
    }
    if args.iter().any(|arg| *arg == "--json") {
        let mut value: serde_json::Value =
            serde_json::from_str(&stdout).expect("--json stdout must be a JSON value");
        normalize_json_paths(&mut value);
        return value.to_string();
    }
    match args {
        ["version"] => stdout
            .lines()
            .find(|line| line.starts_with("qianxing "))
            .and_then(|line| {
                line.split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .into()
            })
            .unwrap_or_default(),
        ["builtin-strategies"] | ["strategy", "list"] => stdout
            .lines()
            .filter(|line| line.contains('\t'))
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
        ["runtime-check"] => stdout
            .lines()
            .filter(|line| line.starts_with("[PASS]") || line.starts_with("[FAIL]"))
            .filter_map(|line| {
                line.split_whitespace()
                    .take(3)
                    .collect::<Vec<_>>()
                    .join(" ")
                    .into()
            })
            .collect::<Vec<_>>()
            .join("\n"),
        ["live-check"] => stdout
            .lines()
            .filter(|line| line.starts_with("[PASS]") || line.starts_with("[FAIL]"))
            .map(|line| {
                line.split_whitespace()
                    .take(2)
                    .collect::<Vec<_>>()
                    .join(" ")
            })
            .collect::<Vec<_>>()
            .join("\n"),
        ["paper" | "paper-check" | "paper-e2e", ..] => stdout
            .lines()
            .filter(|line| line.starts_with("[Paper · E2E]"))
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
        ["fast-backtest", ..] => {
            let mut lines = stdout.lines().map(str::trim_end).collect::<Vec<_>>();
            lines.sort_unstable();
            lines.join("\n")
        }
        _ => stdout
            .lines()
            .map(str::trim_end)
            .collect::<Vec<_>>()
            .join("\n"),
    }
}

fn normalize_json_paths(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(fields) => {
            for (name, value) in fields {
                if name.ends_with("_path") {
                    if let serde_json::Value::String(path) = value {
                        *path = path.replace('\\', "/");
                    }
                } else {
                    normalize_json_paths(value);
                }
            }
        }
        serde_json::Value::Array(values) => {
            for value in values {
                normalize_json_paths(value);
            }
        }
        _ => {}
    }
}

fn observe_in(cwd: &Path, args: &[&str]) -> Observation {
    let output: Output = Command::new(binary())
        .current_dir(cwd)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run qx-cli {args:?}: {error}"));
    let raw_stdout = String::from_utf8_lossy(&output.stdout);
    let stdout = normalize_stdout(args, cwd, &raw_stdout);
    let mut result_hashes = stdout
        .split_whitespace()
        .filter_map(|token| token.strip_prefix("result_hash="))
        .map(str::to_owned)
        .collect::<Vec<_>>();
    result_hashes.sort();
    let artifacts = artifact_files(cwd);
    let artifact_digests = artifact_digests(cwd, &artifacts);
    Observation {
        exit_code: output.status.code().unwrap_or(-1),
        stdout,
        result_hashes,
        artifacts,
        artifact_digests,
    }
}

#[path = "cli_behavior_parity/ashare_report_product.rs"]
mod ashare_report_product;
#[path = "cli_behavior_parity/config_lock_conflict.rs"]
mod config_lock_conflict;
#[path = "cli_behavior_parity/data_validate_parity.rs"]
mod data_validate_parity;
#[path = "cli_behavior_parity/dataset_bundle_parity.rs"]
mod dataset_bundle_parity;
#[path = "cli_behavior_parity/dataset_ingest_parity.rs"]
mod dataset_ingest_parity;
#[path = "cli_behavior_parity/depth_dataset_validate.rs"]
mod depth_dataset_validate;
#[path = "cli_behavior_parity/evidence_routes.rs"]
mod evidence_routes;
#[path = "cli_behavior_parity/external_adapter_fail_closed.rs"]
mod external_adapter_fail_closed;
#[path = "cli_behavior_parity/fast_backtest_parity.rs"]
mod fast_backtest_parity;
#[path = "cli_behavior_parity/full_health_readout.rs"]
mod full_health_readout;
#[path = "cli_behavior_parity/plan_readout_parity.rs"]
mod plan_readout_parity;
#[path = "cli_behavior_parity/quickstart_parity.rs"]
mod quickstart_parity;
#[path = "cli_behavior_parity/verify_json_parity.rs"]
mod verify_json_parity;

fn observe(label: &str, args: &[&str], replay: usize) -> Observation {
    let cwd = temp_dir(label, replay);
    let observation = observe_in(&cwd, args);
    let _ = std::fs::remove_dir_all(cwd);
    observation
}

#[test]
fn pure_local_entrypoints_replay_the_same_process_contract() {
    // Each case is a real non-help CLI entry, executed twice from clean fixed working directories.
    let cases: [(&str, &[&str], i32); 9] = [
        ("version", &["version"], 0),
        ("builtin-strategies", &["builtin-strategies"], 0),
        ("strategy-list", &["strategy", "list"], 0),
        ("runtime-check", &["runtime-check"], 0),
        ("live-check", &["live-check"], 2),
        ("ecosystem-json", &["ecosystem", "--json"], 0),
        ("config-explain-json", &["config", "explain", "--json"], 0),
        ("config-validate", &["config", "validate"], 0),
        ("config-fingerprint", &["config", "fingerprint"], 0),
    ];
    for (label, args, expected_exit) in cases {
        let first = observe(label, args, 1);
        let second = observe(label, args, 2);
        assert_eq!(
            first.exit_code, expected_exit,
            "qx-cli {args:?} exit contract changed: {first:?}"
        );
        assert_eq!(
            first, second,
            "qx-cli {args:?} changed across clean replays"
        );
        assert!(
            first.stdout.trim().len() > 0,
            "qx-cli {args:?} emitted no contract output"
        );
        assert!(
            first.result_hashes.is_empty(),
            "pure listing command unexpectedly emitted a result hash: {first:?}"
        );
        assert!(
            first.artifacts.is_empty(),
            "pure listing command wrote files: {first:?}"
        );
    }
}

#[test]
fn doctor_and_status_json_replay_the_same_fixed_runtime_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let runtime = root
        .join("deploy/qianxing.runtime.example.json")
        .to_string_lossy()
        .into_owned();
    for (label, command) in [("doctor", "doctor"), ("status", "status")] {
        let args = [command, runtime.as_str(), "--json"];
        let first = observe(&format!("{label}-json",), &args, 1);
        let second = observe(&format!("{label}-json",), &args, 2);
        assert_eq!(first.exit_code, 0, "qx-cli {args:?} failed: {first:?}");
        assert_eq!(first, second, "qx-cli {args:?} changed on a clean replay");
        assert!(
            serde_json::from_str::<serde_json::Value>(&first.stdout).is_ok(),
            "qx-cli {args:?} must emit structured JSON"
        );
        assert!(first.result_hashes.is_empty());
        assert!(
            first.artifacts.is_empty(),
            "read-only route wrote files: {first:?}"
        );
    }
}

#[test]
fn strategy_init_force_replay_keeps_the_project_artifact_contract() {
    let cwd = temp_dir("strategy-init", 1);
    let first = observe_in(&cwd, &["strategy", "init", "sma_cross", "runtime.json"]);
    assert_eq!(first.exit_code, 0, "strategy init failed: {first:?}");
    let forced = observe_in(
        &cwd,
        &["strategy", "init", "sma_cross", "runtime.json", "--force"],
    );
    assert_eq!(
        forced.exit_code, 0,
        "strategy init --force failed: {forced:?}"
    );
    assert_eq!(
        first, forced,
        "strategy init replay changed its files or output"
    );
    assert!(first
        .stdout
        .contains("[Strategy · Init] strategy=sma_cross"));
    assert!(first.artifacts.contains(&"runtime.json".to_string()));
    assert!(first
        .artifacts
        .contains(&"qianxing.project.json".to_string()));
    assert!(
        first.artifact_digests.len() > 2,
        "expected runtime, manifest, and copied assets"
    );

    let rejected = Command::new(binary())
        .current_dir(&cwd)
        .args(["strategy", "init", "sma_cross", "runtime.json"])
        .output()
        .expect("run strategy init conflict process");
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert_eq!(rejected.status.code(), Some(2), "{stderr}");
    assert!(stderr.contains("目标策略配置已存在"), "{stderr}");
    assert_eq!(artifact_files(&cwd), first.artifacts);
    assert_eq!(
        artifact_digests(&cwd, &first.artifacts),
        first.artifact_digests
    );
    let _ = std::fs::remove_dir_all(cwd);
}

#[test]
fn config_lock_replay_preserves_the_locked_artifact_bytes() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let runtime = root
        .join("deploy/qianxing.runtime.example.json")
        .to_string_lossy()
        .into_owned();
    let args = ["config", "lock", runtime.as_str(), "locked.json"];
    let first = observe("config-lock", &args, 1);
    let second = observe("config-lock", &args, 2);
    assert_eq!(first.exit_code, 0, "config lock failed: {first:?}");
    assert_eq!(
        first, second,
        "config lock replay changed its process or artifact contract"
    );
    assert_eq!(first.artifacts, ["locked.json"]);
    assert_eq!(first.artifact_digests.len(), 1);
    assert!(first.stdout.contains("locked=true"), "{first:?}");
    assert!(first.stdout.contains("fingerprint="), "{first:?}");
}

#[test]
fn run_paper_aliases_match_the_top_level_paper_process_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let runtime = root
        .join("deploy/qianxing.runtime.paper-strategy.example.json")
        .to_string_lossy()
        .into_owned();
    let direct = observe("paper-direct", &["paper-e2e", runtime.as_str()], 1);
    for (label, alias) in [
        ("run-paper", vec!["run", "paper", runtime.as_str()]),
        (
            "run-paper-check",
            vec!["run", "paper-check", runtime.as_str()],
        ),
    ] {
        let routed = observe(label, &alias, 1);
        assert_eq!(
            direct.exit_code, 0,
            "top-level Paper route failed: {direct:?}"
        );
        assert_eq!(routed.exit_code, direct.exit_code, "{label}: {routed:?}");
        assert_eq!(
            routed.stdout, direct.stdout,
            "{label} changed Paper result fields"
        );
        assert_eq!(routed.result_hashes, direct.result_hashes);
        assert_eq!(
            routed.artifacts, direct.artifacts,
            "{label} wrote a different file set"
        );
        assert!(
            !direct.artifacts.is_empty(),
            "Paper pipeline should persist local artifacts"
        );
    }
}

#[test]
fn readonly_json_entrypoints_replay_structured_contracts_without_artifacts() {
    for (label, args) in [
        ("doctor-json", &["doctor", "--json"][..]),
        ("status-json", &["status", "--json"][..]),
    ] {
        let first = observe(label, args, 1);
        let second = observe(label, args, 2);
        assert_eq!(
            first.exit_code, 0,
            "qx-cli {args:?} must succeed: {first:?}"
        );
        let first_json: serde_json::Value =
            serde_json::from_str(&first.stdout).unwrap_or_else(|error| {
                panic!(
                    "qx-cli {args:?} must emit one JSON value: {error}\n{}",
                    first.stdout
                )
            });
        let second_json: serde_json::Value =
            serde_json::from_str(&second.stdout).unwrap_or_else(|error| {
                panic!(
                    "qx-cli {args:?} must emit one JSON value: {error}\n{}",
                    second.stdout
                )
            });
        assert!(
            first_json.is_object(),
            "JSON contract must be an object: {first_json}"
        );
        assert_eq!(
            first_json, second_json,
            "qx-cli {args:?} JSON fields changed across clean replays"
        );
        assert_eq!(first.result_hashes, second.result_hashes);
        assert_eq!(first.artifacts, second.artifacts);
        assert!(
            first.artifacts.is_empty(),
            "read-only command wrote files: {first:?}"
        );
    }
}

#[test]
fn run_readonly_aliases_match_their_top_level_process_contracts() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let runtime = root
        .join("deploy/qianxing.runtime.example.json")
        .to_string_lossy()
        .into_owned();
    let production = root
        .join("deploy/qianxing.production.example.json")
        .to_string_lossy()
        .into_owned();
    // The umbrella intentionally selects a different default runtime. Give both routes one
    // explicit input so this compares handler behavior rather than their distinct defaults.
    let equivalent_pairs = [
        (
            "run-doctor-json",
            vec!["doctor", runtime.as_str(), "--json"],
            vec!["run", "doctor", runtime.as_str(), "--json"],
            0,
        ),
        (
            "run-runtime-check-json",
            vec!["runtime-check", "--json", runtime.as_str()],
            vec!["run", "runtime-check", "--json", runtime.as_str()],
            0,
        ),
        (
            "run-live-check",
            vec!["live-check", production.as_str()],
            vec!["run", "live-check", production.as_str()],
            2,
        ),
    ];
    for (label, top_level, run_alias, expected_exit) in equivalent_pairs {
        let direct = observe(&format!("{label}-direct"), &top_level, 1);
        let alias = observe(&format!("{label}-alias"), &run_alias, 1);
        assert_eq!(
            direct.exit_code, expected_exit,
            "direct route changed: {direct:?}"
        );
        assert_eq!(direct, alias, "run alias diverged from its top-level route");
        assert!(
            direct.artifacts.is_empty(),
            "read-only route wrote files: {direct:?}"
        );
    }
}
