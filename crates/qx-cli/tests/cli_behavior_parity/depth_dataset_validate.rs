//! Fixed-input process replay for the L1/L2 dataset quality command.

use super::*;

#[test]
fn depth_dataset_validation_replays_both_tiers_and_persists_the_same_manifest() {
    let workspace = Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("qx-cli tests are two levels below the workspace root");
    let tiers = [
        (
            "l1",
            workspace.join("deploy/qianxing.depth-frame.l1.example.json"),
        ),
        (
            "l2",
            workspace.join("deploy/qianxing.depth-frame.example.json"),
        ),
    ];

    for (tier, frame) in tiers {
        let frame = frame.to_string_lossy().into_owned();
        let args = [
            "data-validate-depth",
            frame.as_str(),
            "--tier",
            tier,
            "1000",
            "depth-manifest.json",
        ];
        let label = format!("depth-validate-{tier}");
        let first = observe(&label, &args, 0);
        let second = observe(&label, &args, 1);

        assert_eq!(first.exit_code, 0, "{tier} first run: {}", first.stdout);
        assert_eq!(first, second, "{tier} fixed input must replay identically");
        assert_eq!(first.artifacts, ["depth-manifest.json"]);
        assert!(
            first.stdout.contains(&format!("tier={tier}")),
            "{}",
            first.stdout
        );
        assert!(first.stdout.contains("rows=80"), "{}", first.stdout);
        assert!(
            first.stdout.contains("missing=0 interval_ms=1000"),
            "{}",
            first.stdout
        );
        assert!(first.stdout.contains("verdict=已写出"), "{}", first.stdout);
    }
}
