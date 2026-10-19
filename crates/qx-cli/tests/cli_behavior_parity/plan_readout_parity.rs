use super::{observe, observe_in, temp_dir};
use std::path::Path;

#[test]
fn plan_schema_registry_replays_a_read_only_json_contract() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let manifest = root
        .join("maturity/schema-registry.json")
        .to_string_lossy()
        .into_owned();
    let args = ["plan", "schema-registry", manifest.as_str(), "--json"];

    let first = observe("plan-schema-registry", &args, 1);
    let second = observe("plan-schema-registry", &args, 2);
    assert_eq!(first.exit_code, 0, "plan failed: {first:?}");
    assert_eq!(first, second, "plan changed across clean read-only replays");
    let document: serde_json::Value =
        serde_json::from_str(&first.stdout).expect("plan --json emits one JSON document");
    assert_eq!(document["schema_version"], 1);
    assert!(
        document["entries"]
            .as_array()
            .is_some_and(|entries| !entries.is_empty()),
        "schema registry must contain at least one registered schema: {document}"
    );
    assert!(first.result_hashes.is_empty());
    assert!(
        first.artifacts.is_empty(),
        "plan must not write files: {first:?}"
    );
}

#[test]
fn plan_rejections_replay_without_creating_artifacts() {
    let cases: [(&str, &str, Option<&str>); 3] = [
        ("unknown-kind", "not-a-foundation-kind", Some("{}")),
        ("invalid-project", "project", Some(r#"{"not":"a project"}"#)),
        ("missing-file", "project", None),
    ];

    for (label, kind, payload) in cases {
        let first_dir = temp_dir(&format!("plan-{label}"), 1);
        let second_dir = temp_dir(&format!("plan-{label}"), 2);
        let relative_file = "input.json";
        if let Some(payload) = payload {
            std::fs::write(first_dir.join(relative_file), payload).unwrap();
            std::fs::write(second_dir.join(relative_file), payload).unwrap();
        }

        let args = ["plan", kind, relative_file];
        let first = observe_in(&first_dir, &args);
        let second = observe_in(&second_dir, &args);
        assert_eq!(
            first.exit_code, 2,
            "plan {kind} rejection changed: {first:?}"
        );
        assert_eq!(
            first, second,
            "plan {kind} rejection changed across replays"
        );
        assert_eq!(
            first.stdout.trim_end(),
            "牵星 Qianxing — 分级校准，量天定位",
            "rejected plan emitted anything beyond the normal CLI banner: {first:?}"
        );
        assert_eq!(
            first.artifacts,
            if payload.is_some() {
                vec![relative_file.to_string()]
            } else {
                Vec::new()
            }
        );
        assert_eq!(first.result_hashes, Vec::<String>::new());

        let _ = std::fs::remove_dir_all(first_dir);
        let _ = std::fs::remove_dir_all(second_dir);
    }
}
