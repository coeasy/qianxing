//! 信封血缘的存在性诚实（V11 L2）：`ProjectionLineage` 的三格只在"有值可说"时上 wire。
//!
//! 三格此前是裸 `String`，两个构造点全靠 `..Default::default()` 写空串，读侧收到的是"血缘里有
//! 一格空值"而不是"这一层不知道"——与账户钱字段被 `0` 冒充"没算"是同一条缺陷（V11 Q67/R10）。
//! 住在 `tests/` 而不是内联用例里，是为了不把常驻反例的压力加到被行数棘轮看管的协议面上
//! （V11 T1/T3 的同一口径）。

use qx_protocol::{ProjectionEnvelope, ProjectionLineage, PROJECTION_ENVELOPE_SCHEMA_VERSION};

const LINEAGE_CELLS: [&str; 3] = ["dataset_version", "manifest_digest", "source_digest"];

fn envelope(lineage: ProjectionLineage) -> ProjectionEnvelope<String> {
    ProjectionEnvelope {
        schema_version: PROJECTION_ENVELOPE_SCHEMA_VERSION,
        kind: "account_snapshot".into(),
        tenant_id: "main".into(),
        run_id: "account:main:BINANCE".into(),
        account_id: "main".into(),
        portfolio_id: "default".into(),
        venue_id: "BINANCE".into(),
        as_of: 10,
        event_seq: 1,
        cursor: "1:0000000000000001".into(),
        state_hash: 1,
        source: "eventlog".into(),
        lineage,
        data: String::new(),
    }
}

/// 一份从没说过血缘的信封必须在 JSON 里三格全缺：空串与缺席是两份不同的表态。
#[test]
fn an_undeclared_lineage_cell_stays_off_the_wire() {
    let json =
        serde_json::to_value(envelope(ProjectionLineage::default())).expect("默认信封必须可序列化");
    let lineage = &json["lineage"];
    assert_eq!(
        lineage.as_object().map(serde_json::Map::len),
        Some(0),
        "没有血缘可说时 `lineage` 必须是空对象，而不是三格空串: {lineage}"
    );
    for cell in LINEAGE_CELLS {
        assert!(
            lineage.get(cell).is_none(),
            "{cell} 未声明时不得出现在线上: {lineage}"
        );
    }
}

/// 说了的格子原样往返；没说的那一格解回 `None`，既不是空串也不是上一条文档的残留。
#[test]
fn declared_lineage_round_trips_and_absence_reads_as_none() {
    let declared = ProjectionLineage {
        dataset_version: Some("qx-lineage-fixture-v1".into()),
        manifest_digest: Some("a641bb67631f2226".into()),
        source_digest: Some("21eff57909085502".into()),
    };
    let document = serde_json::to_value(envelope(declared.clone())).expect("完整血缘可序列化");
    for cell in LINEAGE_CELLS {
        assert!(
            document["lineage"].get(cell).is_some(),
            "{cell} 声明过就必须出现在线上: {}",
            document["lineage"]
        );
    }
    let restored: ProjectionEnvelope<String> =
        serde_json::from_value(document.clone()).expect("完整血缘可回读");
    assert_eq!(restored.lineage, declared);

    let mut stripped = document;
    let lineage = stripped["lineage"].as_object_mut().expect("lineage 是对象");
    for cell in ["dataset_version", "manifest_digest"] {
        lineage.remove(cell);
    }
    let stripped: ProjectionEnvelope<String> =
        serde_json::from_value(stripped).expect("缺两格的血缘仍可解析");
    assert_eq!(stripped.lineage.dataset_version, None);
    assert_eq!(stripped.lineage.manifest_digest, None);
    assert_eq!(
        stripped.lineage.source_digest, declared.source_digest,
        "同一份文档里说了的那一格不得被缺格的那两格带倒"
    );
    assert_ne!(
        stripped.lineage,
        ProjectionLineage::default(),
        "`None` 与 `Some(值)` 是两份状态，否则整格血缘兑回了同一个默认值"
    );
}
