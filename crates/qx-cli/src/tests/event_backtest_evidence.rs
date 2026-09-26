//! 研究快照声明的事件回测证据必须在本地 `runs/` 里复核得住（V12 §18-B #115）。
//!
//! 这些用例只打一条断链：`event_verified` 曾是一格没人核对的手抄摘要，编一个 `u64`
//! 就能让实盘闸门以为拿到了事件回测证据。每条用例只改「声明与产物之间的那格关系」，
//! 好让失败信息能指回具体对不上的是哪一格。

use super::*;

const DECLARED_FICTION: u64 = 0xabcd_1234;

fn evidence_manifest(
    run_id: &str,
    strategy_version: &str,
    clock_start: u64,
    clock_end: u64,
) -> RunManifest {
    RunManifest {
        run_id: run_id.into(),
        code_commit: "commit-v1".into(),
        config_hash: "config-v1".into(),
        data_fingerprint: "bars-1".into(),
        input_components: BTreeMap::new(),
        clock_start,
        clock_end,
        global_seed: 7,
        determinism_mode: true,
        result_hash: "result-v1".into(),
        strategy_version: strategy_version.into(),
        instrument_spec_version: "instrument-v1".into(),
        model_fingerprint: "model-v1".into(),
        input_event_hash: "input-v1".into(),
        output_event_hash: "output-v1".into(),
        runtime_version: "runtime-v1".into(),
    }
}

/// 一份声明了事件回测的研究快照，连同它该去哪个 `runs/` 找证据。
struct ClaimedResearch {
    root: PathBuf,
    research: StrategyResearchSnapshot,
}

fn claimed_research(declared: Option<u64>) -> ClaimedResearch {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-event-evidence-{}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
        declared.map_or("unclaimed".to_string(), |value| format!("{value:016x}"))
    ));
    std::fs::create_dir_all(root.join("runs")).unwrap();
    let instrument = InstrumentId::parse("BTCUSDT.BINANCE").unwrap();
    let artifact = FeatureArtifact {
        feature_key: "momentum@v1".into(),
        input_fingerprint: "bars-1".into(),
        as_of: 20,
        coverage_bps: 10_000,
        values: [(instrument, 100)].into_iter().collect(),
    };
    let mut catalog = FactorCatalog::default();
    catalog
        .register_definition(FeatureDefinition {
            name: "momentum".into(),
            version: "v1".into(),
            formula: "close / close[-20] - 1".into(),
            input_fields: vec!["close".into()],
            dependencies: Vec::new(),
            point_in_time: true,
        })
        .unwrap();
    catalog.publish_artifact(artifact.clone()).unwrap();
    let mut candidate = catalog
        .bind_candidate(CandidateRequest {
            strategy_version: "strategy-v1".into(),
            universe_version: "universe-v1".into(),
            parameters: qx_guanxing::ParameterSet::default(),
            data_fingerprint: "bars-1".into(),
            factor_keys: vec!["momentum@v1".into()],
            cost_bps: 8,
            train_start: 1,
            train_end: 10,
            validation_start: 11,
            validation_end: 20,
            intended_exposure: BTreeMap::new(),
            constraints: BTreeMap::new(),
            execution_model: "event-backtest@v1".into(),
            risk_model: "default-risk@v1".into(),
        })
        .unwrap();
    candidate.event_verified = declared.is_some();
    candidate.event_manifest_digest = declared;
    ClaimedResearch {
        root,
        research: StrategyResearchSnapshot {
            schema_version: StrategyResearchSnapshot::SCHEMA_VERSION,
            candidate,
            artifacts: vec![artifact],
            reports: Vec::new(),
            as_of: 20,
        },
    }
}

/// 按回测产物的命名口径把清单放进 `runs/`：文件名带的是「声明的摘要」，内容与它是两回事。
fn plant_manifest(claim: &mut ClaimedResearch, named_digest: u64, manifest: &RunManifest) {
    claim.research.candidate.event_manifest_digest = Some(named_digest);
    std::fs::write(
        claim
            .root
            .join("runs")
            .join(format!("event-backtest-{named_digest:016x}.run.json")),
        manifest.to_json().unwrap(),
    )
    .unwrap();
}

fn cleanup(claim: &ClaimedResearch) {
    let _ = std::fs::remove_dir_all(&claim.root);
}

#[test]
fn a_claim_with_no_local_artifact_is_refused() {
    let claim = claimed_research(Some(DECLARED_FICTION));
    let error = verify_event_backtest_evidence(&claim.root, &claim.research).unwrap_err();
    assert!(
        error.contains("指不到真实产物"),
        "空 runs/ 必须报「声明指不到产物」，实际: {error}"
    );
    cleanup(&claim);
}

#[test]
fn a_manifest_whose_content_disagrees_with_its_name_is_refused() {
    let claim = claimed_research(Some(DECLARED_FICTION));
    // 内容另算一个摘要，但文件名照声明写：只认文件名等于什么都没复核。
    let tampered = evidence_manifest("tampered-run", "strategy-v1", 11, 21);
    std::fs::write(
        claim
            .root
            .join("runs")
            .join(format!("event-backtest-{DECLARED_FICTION:016x}.run.json")),
        tampered.to_json().unwrap(),
    )
    .unwrap();
    let error = verify_event_backtest_evidence(&claim.root, &claim.research).unwrap_err();
    assert!(
        error.contains("按内容重算"),
        "改名冒充必须报「按内容重算不符」，实际: {error}"
    );
    cleanup(&claim);
}

#[test]
fn evidence_from_another_run_is_refused() {
    let claim = claimed_research(Some(DECLARED_FICTION));
    // 本地只有一本与声明无关的真实清单：名字与内容都自洽，但摘要不是声明的那格。
    let foreign = evidence_manifest("other-run", "other-strategy", 11, 20);
    std::fs::write(
        claim
            .root
            .join("runs")
            .join(format!("event-backtest-{:016x}.run.json", foreign.digest())),
        foreign.to_json().unwrap(),
    )
    .unwrap();
    let error = verify_event_backtest_evidence(&claim.root, &claim.research).unwrap_err();
    assert!(
        error.contains("指不到真实产物"),
        "声明指不到本地任何一本清单时必须报错，实际: {error}"
    );
    cleanup(&claim);
}

#[test]
fn evidence_with_the_wrong_lineage_is_refused() {
    let mut claim = claimed_research(Some(DECLARED_FICTION));
    // 摘要这格对得上，但清单是别的策略版本跑出来的：血缘不同就不能替 candidate 作证。
    let foreign = evidence_manifest("other-run", "other-strategy", 1, 25);
    plant_manifest(&mut claim, foreign.digest(), &foreign);
    let error = verify_event_backtest_evidence(&claim.root, &claim.research).unwrap_err();
    assert!(
        error.contains("strategy_version 不符"),
        "血缘不符必须点名是哪个字段，实际: {error}"
    );
    cleanup(&claim);
}

#[test]
fn evidence_that_does_not_cover_the_validation_window_is_refused() {
    let mut claim = claimed_research(Some(DECLARED_FICTION));
    // 同血缘同摘要，但事件回测只跑到验证窗口中间（1..15 而 candidate 验证到 20）。
    let short = evidence_manifest("event-run", "strategy-v1", 1, 15);
    plant_manifest(&mut claim, short.digest(), &short);
    let error = verify_event_backtest_evidence(&claim.root, &claim.research).unwrap_err();
    assert!(
        error.contains("没有覆盖 candidate 的验证窗口"),
        "区间不足必须报错，实际: {error}"
    );
    cleanup(&claim);
}

#[test]
fn honest_evidence_passes_and_an_unclaimed_snapshot_needs_no_artifact() {
    let mut claim = claimed_research(Some(DECLARED_FICTION));
    let honest = evidence_manifest("event-run", "strategy-v1", 1, 25);
    plant_manifest(&mut claim, honest.digest(), &honest);
    verify_event_backtest_evidence(&claim.root, &claim.research).unwrap();
    cleanup(&claim);

    // 没声明的快照不需要任何产物：闸门不替研究层编造「已验证」，也不逼它去跑事件回测。
    let unclaimed = claimed_research(None);
    verify_event_backtest_evidence(&unclaimed.root.join("no-runs-here"), &unclaimed.research)
        .unwrap();
    cleanup(&unclaimed);
}

#[test]
fn a_claim_that_forgets_its_digest_is_refused_before_any_lookup() {
    let mut claim = claimed_research(Some(DECLARED_FICTION));
    // 快照的 from_json 不校验这对字段，而这条复核跑在 validate_for 之前：只声明不给摘要必须当场挡住。
    claim.research.candidate.event_manifest_digest = None;
    let error = verify_event_backtest_evidence(&claim.root, &claim.research).unwrap_err();
    assert!(
        error.contains("没写事件回测 RunManifest 摘要"),
        "只声明不给摘要必须当场拒绝，实际: {error}"
    );
    cleanup(&claim);
}
