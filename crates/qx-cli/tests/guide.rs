use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn isolated_directory() -> std::path::PathBuf {
    static SERIAL: AtomicUsize = AtomicUsize::new(0);
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../target")
        .join(format!(
            "qx-cli-guide-e2e-{}-{}",
            std::process::id(),
            SERIAL.fetch_add(1, Ordering::Relaxed)
        ));
    std::fs::create_dir_all(&root).expect("create isolated guide working directory");
    root
}

fn files_under(root: &std::path::Path) -> Vec<std::path::PathBuf> {
    let mut pending = vec![root.to_path_buf()];
    let mut files = Vec::new();
    while let Some(directory) = pending.pop() {
        for entry in std::fs::read_dir(directory).expect("read guide project directory") {
            let path = entry.expect("read guide directory entry").path();
            if path.is_dir() {
                pending.push(path);
            } else {
                files.push(path);
            }
        }
    }
    files
}

fn contains_result_hash(value: &serde_json::Value, expected: &str) -> bool {
    match value {
        serde_json::Value::Object(fields) => {
            fields
                .get("result_hash")
                .and_then(serde_json::Value::as_str)
                == Some(expected)
                || fields
                    .values()
                    .any(|child| contains_result_hash(child, expected))
        }
        serde_json::Value::Array(values) => values
            .iter()
            .any(|child| contains_result_hash(child, expected)),
        _ => false,
    }
}

#[test]
fn role_guides_are_reachable_and_keep_the_first_report_path_within_five_steps() {
    let binary = env!("CARGO_BIN_EXE_qx-cli");
    let cases = [
        ("researcher", "研究者 / Researcher"),
        ("strategy-author", "策略作者 / Strategy author"),
        ("operator", "运维 / Operator"),
        ("integrator", "集成者 / Integrator"),
    ];
    for (role, heading) in cases {
        let output = Command::new(binary)
            .args(["guide", role])
            .env_remove("QX_DEPLOY_DIR")
            .env_remove("QX_PYTHON")
            .output()
            .unwrap_or_else(|error| panic!("run qx-cli guide {role}: {error}"));
        let stdout = String::from_utf8_lossy(&output.stdout);
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert_eq!(output.status.code(), Some(0), "{role}: {stderr}");
        assert!(stdout.contains(heading), "missing role heading: {stdout}");
        let steps = stdout
            .lines()
            .filter(|line| {
                line.trim_start()
                    .chars()
                    .next()
                    .is_some_and(|first| ('1'..='5').contains(&first))
                    && line.trim_start().as_bytes().get(1) == Some(&b'.')
            })
            .count();
        assert_eq!(steps, 5, "{role} must give five actionable steps at most");
        assert!(
            stdout.contains("qx-cli "),
            "commands must be copyable: {stdout}"
        );
        assert!(
            stdout.contains("术语 / Terms"),
            "missing bilingual terms: {stdout}"
        );
    }

    let index = Command::new(binary)
        .arg("guide")
        .output()
        .expect("run qx-cli guide index");
    let stdout = String::from_utf8_lossy(&index.stdout);
    assert_eq!(index.status.code(), Some(0));
    assert!(stdout.contains("qx-cli guide researcher"));
    assert!(stdout.contains("qx-cli quickstart ./qianxing-demo"));
}

#[test]
fn researcher_guide_command_produces_a_report_that_can_be_read_back() {
    let binary = env!("CARGO_BIN_EXE_qx-cli");
    let guide = Command::new(binary)
        .args(["guide", "researcher"])
        .output()
        .expect("run researcher guide");
    let guide_text = String::from_utf8_lossy(&guide.stdout);
    let first_step = guide_text
        .lines()
        .find(|line| line.contains("quickstart ./qianxing-demo"))
        .expect("researcher guide must advertise its first-run command");
    let command = first_step
        .split('`')
        .nth(1)
        .expect("first-run command must be quoted for copying");
    let mut tokens = command.split_whitespace();
    assert_eq!(tokens.next(), Some("qx-cli"));
    let args = tokens.collect::<Vec<_>>();
    let working = isolated_directory();
    let first_run = Command::new(binary)
        .current_dir(&working)
        .args(args)
        .env_remove("QX_DEPLOY_DIR")
        .env_remove("QX_PYTHON")
        .output()
        .expect("run the command printed by the researcher guide");
    let first_text = String::from_utf8_lossy(&first_run.stdout);
    let first_error = String::from_utf8_lossy(&first_run.stderr);
    assert_eq!(first_run.status.code(), Some(0), "{first_error}");
    assert!(first_text.contains("[完成] 读回摘要"), "{first_text}");
    let result_hash = first_text
        .split("result_hash=")
        .nth(1)
        .and_then(|tail| tail.split_whitespace().next())
        .expect("first-run report must print its result_hash");
    let project = working.join("qianxing-demo");
    let project_files = files_under(&project);
    assert!(
        project_files
            .iter()
            .any(|path| path.to_string_lossy().ends_with(".summary.json")),
        "quickstart must leave a summary artifact under the project directory"
    );

    let report = Command::new(binary)
        .current_dir(&working)
        .args(["report", "./qianxing-demo/qianxing.runtime.json", "--json"])
        .output()
        .expect("read back the guide-generated report");
    let report_text = String::from_utf8_lossy(&report.stdout);
    let report_error = String::from_utf8_lossy(&report.stderr);
    assert_eq!(report.status.code(), Some(0), "{report_error}");
    let report_json: serde_json::Value =
        serde_json::from_str(&report_text).expect("report --json emits one JSON value");
    assert!(
        contains_result_hash(&report_json, result_hash),
        "read-back report must refer to the quickstart result {result_hash}: {report_json}"
    );
    std::fs::remove_dir_all(working).expect("remove isolated guide project");
}
