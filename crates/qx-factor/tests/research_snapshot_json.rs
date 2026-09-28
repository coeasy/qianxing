//! 研究快照 JSON 的对外契约：字段逐层必须齐全，拼错的键必须报错而不是被按默认值读回
//! （快照由仓库外的因子环节导出，运行时闸门却直接把它当实盘前置条件；V12 §18-B #119）。
//!
//! 这里刻意只用 JSON 夹具：从 Rust 结构体 `to_json()` 出来的文本永远不会带未知键，
//! 所以解析侧的宽严只能在手抄入口这一侧被证明。

use qx_factor::StrategyResearchSnapshot;

/// 沿 JSON 路径下钻；`"artifacts/0"` 这样的段先按对象键、再按数组下标。
fn descend<'a>(root: &'a mut serde_json::Value, path: &[&str]) -> &'a mut serde_json::Value {
    let mut node = root;
    for segment in path {
        for step in segment.split('/') {
            node = match step.parse::<usize>() {
                Ok(index) => node.get_mut(index).expect("夹具里应当有这个数组元素"),
                Err(_) => node.get_mut(step).expect("夹具里应当有这个字段"),
            };
        }
    }
    node
}

/// 一份逐层齐全的研究快照声明：与 deploy/README.md 的字段清单同源。
fn canonical_snapshot() -> serde_json::Value {
    serde_json::json!({
        "schema_version": 1,
        "candidate": {
            "config": {
                "strategy_version": "strategy-v1",
                "universe_version": "universe-v1",
                "feature_version": "momentum@v1",
                "parameters": {},
                "data_fingerprint": "bars-1",
                "intended_exposure": [],
                "constraints": {},
                "execution_model": "event-backtest@v1",
                "risk_model": "default-risk@v1"
            },
            "factor_keys": ["momentum@v1"],
            "cost_bps": 8,
            "train_start": 1,
            "train_end": 10,
            "validation_start": 11,
            "validation_end": 20,
            "event_verified": false,
            "event_manifest_digest": null
        },
        "artifacts": [
            {
                "feature_key": "momentum@v1",
                "input_fingerprint": "bars-1",
                "as_of": 20,
                "coverage_bps": 10_000,
                "values": [{ "instrument": "600000.XSHG", "value": 123 }]
            }
        ],
        "reports": [
            {
                "feature_key": "momentum@v1",
                "input_fingerprint": "bars-1",
                "observation_hash": 7,
                "analysis_start": 1,
                "analysis_end": 20,
                "sample_count": 2,
                "coverage_bps": 10_000,
                "ic_bps": 12,
                "rank_ic_bps": 10,
                "turnover_bps": 100,
                "transform": null,
                "missing_policy": "reject",
                "decay_bps": 0,
                "capacity_raw": 1,
                "exposures": {}
            }
        ],
        "as_of": 20
    })
}

/// 拼错的键必须报错而不是被 serde 静默丢掉：静默缺格会让闸门拿默认值放行一份
/// 生产者以为完整的声明。
#[test]
fn research_snapshot_json_rejects_unknown_field_names_at_every_level() {
    let base = canonical_snapshot();
    // 同一份 JSON 在没有任何多余键时必须能读回来，否则下面的断言只是夹具本身错了。
    let canonical = StrategyResearchSnapshot::from_json(&base.to_string()).unwrap();
    assert_eq!(canonical.as_of, 20);
    for (path, key) in [
        (vec![], "schema_verion"),
        (vec!["candidate"], "event_verifed"),
        (vec!["candidate", "config"], "data_fingerprnt"),
        (vec!["artifacts/0"], "coverage_bp"),
        (vec!["reports/0"], "ic_bp"),
    ] {
        let mut leaked = base.clone();
        descend(&mut leaked, &path)[key] = serde_json::Value::from(true);
        let error = StrategyResearchSnapshot::from_json(&leaked.to_string())
            .err()
            .unwrap_or_else(|| panic!("未知字段 {key} 被静默忽略，读回了缺格的快照声明"));
        assert!(
            format!("{error:?}").contains(key),
            "报错必须点名拼错的键 {key}，实际：{error:?}"
        );
    }
}

/// 缺格同样不能被读回：运行时闸门按这些字段判定血缘，少一格就不是同一份声明。
#[test]
fn research_snapshot_json_requires_every_required_field() {
    let base = canonical_snapshot();
    for (path, key) in [
        (vec!["candidate"], "event_verified"),
        (vec!["candidate", "config"], "data_fingerprint"),
        (vec!["artifacts/0"], "as_of"),
        (vec![], "as_of"),
    ] {
        let mut missing = base.clone();
        descend(&mut missing, &path)
            .as_object_mut()
            .expect("待删键所在层必须是对象")
            .remove(key);
        assert!(
            StrategyResearchSnapshot::from_json(&missing.to_string()).is_err(),
            "缺键 {key} 却被读回，说明该层声明可有可无"
        );
    }
}
