//! T1-1 / 退出门 G1 第一条：运行证据包（`qx report --evidence`）。
//!
//! 这一族用例跑的是**真回测产物**（复用 `backtest_input_provenance` 的夹具链），因为证据包要
//! 证明的是「同一运行可由 RunManifest 离线复算」——另造一份假摘要会让这句话失去对照物。

use super::backtest_input_provenance::{bar_chain_products, clean_up};
use super::*;
use qx_spec::FoundationDocument;

/// 产物目录里有没有以 `suffix` 结尾的文件（用于钉「没给 --evidence 时不许凭空多出产物」）。
fn artifact_suffix_present(strategy_root: &Path, suffix: &str) -> bool {
    std::fs::read_dir(strategy_root.join("runs"))
        .unwrap_or_else(|error| panic!("读取产物目录失败: {error}"))
        .filter_map(|entry| entry.ok())
        .any(|entry| entry.file_name().to_string_lossy().ends_with(suffix))
}

/// T1-1 / 退出门 G1 第一条：一次已完成的回测必须能聚出一份可离线复算的运行证据包。
///
/// 四件事一起钉：(a) 证据包真的落盘且能过统一规格漏斗；(b) 它的身份块与**同一轮** RunManifest
/// 逐字相等——这是「同一运行」的判据，不是「长得像」；(c) 未验证清单非空（一份本地回测证据包若
/// 声称没有任何未验证项，那是在盖单一 verified 标签，T1-7 明令禁止）；(d) 它是 **opt-in** 的，
/// 默认面产物集合不许变——`tools/backtest_acceptance.py` 按种类比对产物，默认面是它的基线。
#[test]
fn report_evidence_aggregates_a_recomputable_run_evidence_bundle() {
    let (summary, runtime, dirs) = bar_chain_products("t11-evidence");
    let strategy_root = dirs[1].clone();

    run_report(&runtime, false, false).expect("不带 --evidence 的报告应通过");
    assert!(
        !artifact_suffix_present(&strategy_root, ".evidence.json"),
        "没给 --evidence 时不许写证据包：默认面产物集合是验收脚本的基线"
    );

    run_report_with_output(&runtime, false, false, true, None).expect("带 --evidence 的报告应通过");
    let payload = read_first_artifact(&strategy_root, ".evidence.json");
    let bundle = qx_spec::RunEvidenceBundle::from_json(&payload.to_string())
        .expect("写出的证据包必须过统一规格漏斗");
    let manifest = read_first_artifact(&strategy_root, ".run.json");

    assert_eq!(bundle.schema_version, qx_spec::RUN_EVIDENCE_SCHEMA_VERSION);
    assert_eq!(bundle.artifacts.len(), 4, "离线复算四件套必须齐");
    // (b) 同一运行：合成数据指纹、结果哈希、构建身份都必须来自这一轮的 RunManifest。
    assert_eq!(
        serde_json::json!(bundle.run.data_fingerprint),
        manifest["data_fingerprint"]
    );
    assert_eq!(
        serde_json::json!(bundle.run.result_hash),
        summary["result_hash"]
    );
    assert_eq!(
        bundle.dataset.composed_fingerprint, bundle.run.data_fingerprint,
        "数据块的合成指纹必须与 run 块逐字相等（对象层已把这条钉成会拒的判据）"
    );
    // 内容指纹与合成指纹是**两格**：前者是「这份文件是什么」，后者是「这次运行认哪份数据」。
    assert_eq!(
        serde_json::json!(bundle.dataset.content_fingerprint),
        summary["input"]["fingerprint"]
    );
    assert_eq!(bundle.dataset.kind, "barframe");
    assert!(
        bundle.dataset.quality_report.is_none(),
        "回测链的输入身份是 v1 数据集清单、不带质量报告：这一格必须如实写 null，不许造一份全 0 的假报告"
    );
    // (c) 本地回测证据只到 L1，且未验证清单必须点名缺席的是哪一类证据。
    assert_eq!(
        bundle.verification.capability_level,
        qx_spec::CapabilityLevel::L1
    );
    assert_eq!(
        bundle.verification.replay_verdict,
        qx_spec::ReplayVerdict::Verified
    );
    assert!(bundle.verification.artifact_digests_verified);
    assert_eq!(bundle.verification.verified_artifact_count, 4);
    assert!(!bundle.unverified.is_empty(), "未验证清单不得为空");
    assert!(
        bundle.unverified.iter().any(|item| item.contains("venue")),
        "未验证清单必须点名缺席的是外部 venue 证据: {:?}",
        bundle.unverified
    );
    assert_eq!(
        bundle.recompute.expected_result_hash, bundle.run.result_hash,
        "复算指引指的必须是这一次运行"
    );
    assert!(bundle
        .assumptions
        .iter()
        .any(|assumption| assumption.name == "matching_kernel"));
    clean_up(dirs);
}

/// 证据包写在**复核链之后**：复核失败时不许留下半份证据包。
///
/// 这条钉的是顺序而不是错误文案——`run_report_with_output` 里若把 `write_run_evidence` 提到
/// `recompute_declared_backtest_input` 之前，拒绝路径上就会先落一份
/// `artifact_digests_verified=true` 的证据包，而那个 true 从来没被核对过。
#[test]
fn report_evidence_is_not_written_when_the_verification_chain_refuses() {
    let (summary, runtime, dirs) = bar_chain_products("t11-evidence-refused");
    let strategy_root = dirs[1].clone();
    let record_path = summary["run_record"]
        .as_str()
        .expect("v5 摘要要有 RunRecord");
    let record =
        qx_spec::RunRecord::from_json(&std::fs::read_to_string(record_path).unwrap()).unwrap();
    let fills = record
        .artifact_refs
        .iter()
        .find(|artifact| artifact.name == "fills")
        .expect("RunRecord 必须引用成交明细");
    let mut bytes = std::fs::read(&fills.path).unwrap();
    bytes.extend_from_slice(b"tampered");
    std::fs::write(&fills.path, bytes).unwrap();

    assert!(
        run_report_with_output(&runtime, false, false, true, None).is_err(),
        "产物被篡改时带 --evidence 的报告必须拒绝"
    );
    assert!(
        !artifact_suffix_present(&strategy_root, ".evidence.json"),
        "复核链拒绝之后不得留下证据包（那一格 artifact_digests_verified 会是个没被核对过的 true）"
    );
    clean_up(dirs);
}
