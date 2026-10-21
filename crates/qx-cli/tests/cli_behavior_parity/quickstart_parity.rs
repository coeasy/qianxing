use super::{observe_in, temp_dir, Observation};

fn assert_same_process_contract(first: &Observation, second: &Observation) {
    assert_eq!(first.exit_code, second.exit_code);
    assert_eq!(first.stdout, second.stdout);
    assert_eq!(first.result_hashes, second.result_hashes);
    assert_eq!(first.artifacts, second.artifacts);
}

#[test]
fn quickstart_replays_the_same_five_step_backtest_contract() {
    // Reuse one absolute project path across clean replays. `init` binds storage to that path,
    // and its config fingerprint correctly contributes to the run identity.
    let cwd = temp_dir("quickstart", 1);
    let args = ["quickstart", "project"];
    let first = observe_in(&cwd, &args);
    std::fs::remove_dir_all(&cwd).expect("remove first quickstart replay");
    std::fs::create_dir(&cwd).expect("recreate quickstart replay root");
    let second = observe_in(&cwd, &args);
    std::fs::remove_dir_all(&cwd).expect("remove second quickstart replay");

    assert_eq!(first.exit_code, 0, "quickstart failed: {first:?}");
    assert!(
        !first.result_hashes.is_empty(),
        "quickstart must expose its backtest result identity: {first:?}"
    );
    assert_same_process_contract(&first, &second);

    for step in ["建项目", "静态检查", "跑一轮回测", "读回摘要", "看安全状态"]
    {
        assert!(
            first.stdout.contains(step),
            "missing quickstart step {step}: {first:?}"
        );
    }
    for artifact in ["project/qianxing.project.json", "project/data"] {
        assert!(
            first
                .artifacts
                .iter()
                .any(|path| path == artifact || path.starts_with(&format!("{artifact}/"))),
            "quickstart did not create {artifact}: {first:?}"
        );
    }
    assert!(
        first
            .artifacts
            .iter()
            .any(|path| path.ends_with(".summary.json")),
        "quickstart must persist the backtest summary: {first:?}"
    );
}

#[test]
fn init_replays_project_artifacts_and_rejects_overwrite_without_force() {
    let cwd = temp_dir("init-command", 1);
    let args = ["init", "project/runtime.json"];
    let first = observe_in(&cwd, &args);
    std::fs::remove_dir_all(&cwd).expect("remove first init replay");
    std::fs::create_dir(&cwd).expect("recreate init replay root");
    let second = observe_in(&cwd, &args);

    assert_eq!(first.exit_code, 0, "init failed: {first:?}");
    assert_eq!(first, second, "init artifacts changed across clean replays");
    assert!(first.result_hashes.is_empty());
    for artifact in [
        "project/runtime.json",
        "project/README.qianxing.md",
        "project/qianxing.bar-frame.example.json",
        "project/qianxing.dataset-bundle.bar-frame.example.json",
    ] {
        assert!(
            first.artifacts.iter().any(|path| path == artifact),
            "init did not create {artifact}: {first:?}"
        );
    }

    let conflict = observe_in(&cwd, &args);
    let conflict_replay = observe_in(&cwd, &args);
    assert_eq!(
        conflict.exit_code, 2,
        "init must refuse overwrite: {conflict:?}"
    );
    assert_eq!(
        conflict, conflict_replay,
        "overwrite refusal changed on replay"
    );
    assert_eq!(conflict.artifacts, first.artifacts);
    assert_eq!(conflict.artifact_digests, first.artifact_digests);
    let _ = std::fs::remove_dir_all(cwd);
}
