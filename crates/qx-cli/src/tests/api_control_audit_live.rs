//! 控制面审计流水必须现读 worker 写进去的那本 store（V11 H1）。
//!
//! `/control/audit` 此前读 `ApiState.control`——服务装配时装进来的那一份，此后只会被**本进程**的
//! POST 换掉。执行 worker 在另一个进程用 `ControlPlane::execute` 把 `Executed`/`Failed` 追加进
//! store，HTTP 侧就永远把命令念成 `Accepted`；而同一本 store 在 `/ready` 那里已经是每请求
//! `load()` 一次——同一个对象，两个端点讲两个时点。

use super::*;

fn pause_command(command_id: u64) -> ControlCommand {
    ControlCommand {
        command_id,
        request_id: format!("control-audit-live:{command_id}"),
        operator_id: "ops".into(),
        reason: "控制面读模型时机".into(),
        kind: CommandKind::PauseStrategy,
        target: "main/paper".into(),
        payload: BTreeMap::new(),
        permission: Permission::Trading,
        dry_run: false,
    }
}

/// 模拟"另一个进程"走完一次命令生命周期：提交进 store，再由执行侧把终态追加进同一本 store。
///
/// 写的是 store 而不是 `ApiState`，这正是 worker 与 serve 之间的关系。
fn submit_and_execute_out_of_process(config: &RuntimeConfig, command_id: u64) {
    let store = configured_control_store(config).expect("paper 拓扑应能解析出控制面 store");
    store
        .transact(|plane| plane.submit_as(pause_command(command_id), Permission::Trading, 10))
        .map(|(_, record)| record)
        .expect("控制命令写入 store 成功")
        .expect("命令身份与权限校验通过");
    store
        .transact(|plane| plane.execute(command_id, 11, |_| Ok("H1_EXECUTED".into())))
        .map(|(_, record)| record)
        .expect("执行终态写入 store 成功")
        .expect("终态回写被接受");
}

/// `/control/audit` 一次交出两格：有界审计窗口，和窗口之外的累计退场摘要（V11 R5-1）。
fn audit_body(
    service: &ApiService,
) -> (Vec<qx_control::AuditRecord>, qx_control::RetirementSummary) {
    let response = service.handle("GET", "/control/audit", "", runtime_timestamp_ms());
    assert_eq!(response.status, 200, "控制面可读时 /control/audit 不该降级");
    let body: serde_json::Value =
        serde_json::from_str(&response.body).expect("审计流水按契约反序列化");
    (
        serde_json::from_value(body["records"].clone()).expect("records 是窗口内审计记录数组"),
        serde_json::from_value(body["retirement"].clone())
            .expect("retirement 是退场累计摘要，缺它等于把裁剪念成总量"),
    )
}

/// boot 之前与之后各有一条命令走完生命周期：两条都必须出现在 HTTP 侧。
///
/// 前一条挡住"改现读之后只读增量"这种修法，后一条才是这颗缺陷本身。
#[test]
fn control_audit_reads_the_store_the_workers_write_into() {
    let root = temp_cli_case_dir("control-audit-live");
    let data_dir = root.join("data");
    std::fs::create_dir_all(&data_dir).unwrap();
    let config = paper_runtime_config(&data_dir);
    let config_path = data_dir.join("runtime.json");
    std::fs::write(&config_path, config.to_json().unwrap()).unwrap();

    submit_and_execute_out_of_process(&config, 9801);
    let service = build_configured_api_service(&config, &config_path).unwrap();
    // boot 之后由"另一个进程"追加的那一条：读进程内那份副本永远看不见它。
    submit_and_execute_out_of_process(&config, 9802);

    // 先问 trait、再问 HTTP：`control_plane()` 每次成功现读都会替进程内副本喂热，摘要这颗
    // 因此必须排在最前——晚一步就再也读不出「等别人喂热的那一份副本」了（V11 R7-8 变异实测：
    // 只把摘要面退回副本，排在 HTTP 之后的断言根本不会红）。
    let cold_retirement = service.query_port().control_retirement();
    let cold_audit = service.query_port().control_audit();
    let (audit, retirement) = audit_body(&service);
    for command_id in [9801_u64, 9802] {
        let records: Vec<_> = audit
            .iter()
            .filter(|record| record.command_id == command_id)
            .collect();
        assert_eq!(records.len(), 2, "每条命令都要有 Accepted 与终态两笔");
        assert_eq!(records[0].status, qx_control::CommandStatus::Accepted);
        assert_eq!(records[1].status, qx_control::CommandStatus::Executed);
        assert_eq!(records[1].result_code, "H1_EXECUTED");
    }
    // 两条命令都已终态退场：主表读不到它们，窗口与累计摘要必须同时说出这件事（V11 R5-1）。
    assert_eq!(
        (retirement.retired_total, retirement.executed),
        (2, 2),
        "退场计数必须跟着现读一起走，不能停在 boot 那一份"
    );
    // `QueryPort` 的 trait 读点与 HTTP 端点必须走同一个出口，而不是各念一份（V11 S3 的理由）。
    assert_eq!(
        cold_audit, audit,
        "trait 读点与 HTTP 端点说的是同一份审计流水"
    );
    // 窗口之外那一半也要走得到 trait：只挂在 HTTP 响应里，进程内的读者就只能把"最近这么多
    // 条"当成"总共这么多条"——正是这颗缺陷在 L1/L2 那一族里的形状（V11 R7-8）。
    assert_eq!(
        cold_retirement, retirement,
        "退场累计摘要必须与流水同源：两个读面不能各说一份总量"
    );
    // 回填：`ApiState.control` 是 pub 字段、提交路径会写它，现读之后它不能还停在 boot。
    let mirror = service
        .state()
        .lock()
        .expect("api state mutex poisoned")
        .control
        .audit()
        .to_vec();
    assert_eq!(
        mirror, audit,
        "进程内那份副本必须跟着现读一起前进，否则字段与 store 又是两个时点"
    );
    let _ = std::fs::remove_dir_all(root);
}
