//! 持久化类型的升级路径：缺键必须走**具名缺省**，而不是整份拒绝（V13 R27）。
//!
//! `JobSpec` 不是一次性请求体——`JsonStateStore::save_scheduler_at` 把它写进 `scheduler.json`，
//! `load_scheduler_at` 又在下一次启动时读回。`input_refs`/`output_refs`/`permission_scope` 三格是
//! **零生产读者**的声明格（`JobSpec::validate` 一格都不看），它们当初按必填落地，于是任何在本构建
//! 之前写下的状态文件都读不回来：实测 `qx-cli paper-e2e` 在仓库根死在第 427 列
//! `Io("missing field `input_refs`")`，而报错一个字都没提"这份状态是旧版本写的"。
//!
//! 这三条用例把"旧文件读得回来"从一句声明变成可执行的断言——静态判据（门禁里的
//! `persisted_state_upgrade_path_check`）只是快闸，真正的牙齿在这里。

use qx_scheduler::{JobSpec, JobWindow, RetryPolicy, Scheduler, Trigger};

/// 与 `src/lib.rs` 的用例夹具同形：一份"当代"的作业声明（三格都写全）。
fn job(id: &str) -> JobSpec {
    JobSpec {
        job_id: id.into(),
        job_version: "v1".into(),
        owner: "research".into(),
        enabled: true,
        trigger: Trigger::Manual,
        window: JobWindow::Any,
        depends_on: Vec::new(),
        input_refs: vec!["bars".into()],
        output_refs: vec![format!("out-{id}")],
        timeout_seconds: 60,
        retry_policy: RetryPolicy::default(),
        concurrency_key: id.into(),
        idempotency_key: format!("{id}-daily"),
        permission_scope: "research".into(),
        audit_reason: format!("test {id}"),
        dry_run: true,
    }
}

#[test]
fn persisted_job_spec_tolerates_missing_declaration_fields() {
    let original = job("paper-strategy-order");
    let mut value = serde_json::to_value(&original).unwrap();
    for key in ["input_refs", "output_refs", "permission_scope"] {
        assert!(
            value.as_object_mut().unwrap().remove(key).is_some(),
            "{key} 本该出现在序列化结果里"
        );
    }
    let restored: JobSpec = serde_json::from_value(value).expect("旧文件必须读得回来");
    assert!(restored.input_refs.is_empty());
    assert!(restored.output_refs.is_empty());
    // 具名缺省，不是空串：空串会把"写于本字段存在之前"与"显式声明了空权限"读成同一件事。
    assert_eq!(restored.permission_scope, "legacy-undeclared");
    // 具名缺省不该顺带改写别的字段。
    assert_eq!(restored.job_id, original.job_id);
    assert_eq!(restored.idempotency_key, original.idempotency_key);
    assert_eq!(restored.timeout_seconds, original.timeout_seconds);
}

#[test]
fn persisted_job_spec_still_rejects_a_missing_required_field() {
    // 反向：真正必填的字段缺了仍要拒绝——否则"容忍缺键"会退化成"什么都能读"。
    let original = job("paper-strategy-order");
    for key in ["job_id", "idempotency_key", "timeout_seconds"] {
        let mut value = serde_json::to_value(&original).unwrap();
        assert!(value.as_object_mut().unwrap().remove(key).is_some());
        assert!(
            serde_json::from_value::<JobSpec>(value).is_err(),
            "{key} 是必填格，缺了必须拒绝"
        );
    }
}

#[test]
fn scheduler_state_written_before_the_declaration_fields_still_loads() {
    // 真正的持久化读侧是 `Scheduler::from_json`（`load_scheduler_at` 就调它）：整份调度状态里的
    // JobSpec 缺那三格时读回必须成功——这条比上面两条更贴近事故现场。
    let mut scheduler = Scheduler::default();
    scheduler.register(job("paper-strategy-order")).unwrap();
    let mut value: serde_json::Value = serde_json::from_str(&scheduler.to_json().unwrap()).unwrap();
    let entry = value
        .get_mut("jobs")
        .and_then(|jobs| jobs.as_object_mut())
        .and_then(|jobs| jobs.values_mut().next())
        .expect("调度状态里应有一格作业");
    for key in ["input_refs", "output_refs", "permission_scope"] {
        assert!(entry.as_object_mut().unwrap().remove(key).is_some());
    }
    let restored = Scheduler::from_json(&serde_json::to_string(&value).unwrap())
        .expect("旧状态文件必须读得回来");
    assert_eq!(restored.len(), 1);
}
