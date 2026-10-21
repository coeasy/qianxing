//! The generated run-route inventory must stay connected to real CLI dispatch.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

fn routes() -> Vec<String> {
    let manifest: serde_json::Value =
        serde_json::from_str(include_str!("../src/cli_command_manifest.json"))
            .expect("generated CLI command manifest is valid JSON");
    manifest["commands"]
        .as_array()
        .expect("manifest commands is an array")
        .iter()
        .filter(|route| route["parent"] == "run")
        .map(|route| {
            route["name"]
                .as_str()
                .expect("run route name is a string")
                .to_owned()
        })
        .collect()
}

fn workdir() -> PathBuf {
    static SERIAL: AtomicUsize = AtomicUsize::new(0);
    let serial = SERIAL.fetch_add(1, Ordering::Relaxed);
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli integration tests are two levels below the workspace root");
    let path = root
        .join("target")
        .join(format!("qx-cli-run-entry-{}-{serial}", std::process::id()));
    std::fs::create_dir_all(&path).expect("create isolated run-route working directory");
    path
}

fn run_in(cwd: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .current_dir(cwd)
        .args(args)
        .output()
        .unwrap_or_else(|error| panic!("run qx-cli {args:?}: {error}"))
}

fn diagnostic(output: &Output) -> String {
    format!(
        "{}\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
}

#[test]
fn every_generated_run_route_reaches_dispatch_and_unknown_route_is_usage_error() {
    let entries = routes();
    assert!(!entries.is_empty());
    let unique = entries.iter().collect::<std::collections::BTreeSet<_>>();
    assert_eq!(
        unique.len(),
        entries.len(),
        "run route names must be unique"
    );

    let cwd = workdir();
    let help = run_in(&cwd, &["run", "--help"]);
    assert_eq!(help.status.code(), Some(0), "run --help failed");
    let help_text = diagnostic(&help);
    for entry in &entries {
        assert!(
            help_text.contains(entry),
            "run help must advertise generated route {entry}: {help_text}"
        );
    }

    let missing_runtime = cwd.join("missing-runtime.json");
    let missing_runtime = missing_runtime.to_string_lossy().into_owned();
    for entry in &entries {
        let output = run_in(&cwd, &["run", entry, &missing_runtime]);
        let message = diagnostic(&output);
        assert_ne!(
            output.status.code(),
            Some(0),
            "run {entry} with a missing runtime must fail closed: {message}"
        );
        assert!(
            !message.contains("不支持"),
            "generated route run {entry} has no dispatch branch: {message}"
        );
    }

    let output = run_in(&cwd, &["run", "definitely-not-an-entry"]);
    let unknown = diagnostic(&output);
    assert_eq!(
        output.status.code(),
        Some(2),
        "unknown run route: {unknown}"
    );
    assert!(unknown.contains("unrecognized subcommand"), "{unknown}");
    assert!(unknown.contains("qx-cli run [COMMAND]"), "{unknown}");
    let _ = std::fs::remove_dir_all(cwd);
}
