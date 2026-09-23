//! 多腿（配对套利）组级归因的端到端链路测试：费用闭合、保证金/资金费口径与确定性。

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

fn fixture(name: &str) -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
        .to_string_lossy()
        .to_string()
}

fn temp_root(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-multi-leg-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("创建多腿归因产物目录失败");
    root
}

fn run(args: &[&str]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("启动 qx-cli 多腿回测失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

/// 用给定标的（可选衍生品规格）跑一次多腿回测，`--root` 落在 `out` 下。
fn backtest(out: &Path, quantity: &str, funding_bps: &str, legs: &[&str]) -> (i32, String, String) {
    let root = out.to_string_lossy().to_string();
    let mut args = vec!["backtest", "multi-builtin", "pairs_arbitrage"];
    args.extend(legs.iter().copied());
    args.extend([
        "--quantity",
        quantity,
        "--funding-bps",
        funding_bps,
        "--root",
        root.as_str(),
    ]);
    run(&args)
}

fn attribution_fields(stdout: &str) -> BTreeMap<String, String> {
    line_fields(stdout, "[Multi-leg · Attribution]")
}

fn integrity_fields(stdout: &str) -> BTreeMap<String, String> {
    line_fields(stdout, "[Multi-leg · Integrity]")
}

fn reconcile_fields(stdout: &str) -> BTreeMap<String, String> {
    line_fields(stdout, "[Multi-leg · Reconcile]")
}

fn line_fields(stdout: &str, prefix: &str) -> BTreeMap<String, String> {
    let line = stdout
        .lines()
        .find(|line| line.starts_with(prefix))
        .unwrap_or_else(|| panic!("输出缺少 {prefix}"));
    line.split_whitespace()
        .filter_map(|token| token.split_once('='))
        .map(|(key, value)| (key.to_string(), value.to_string()))
        .collect()
}

fn result_hashes(stdout: &str) -> String {
    stdout
        .lines()
        .find(|line| line.starts_with("[Multi-leg · Backtest]"))
        .and_then(|line| line.split("result_hashes=").nth(1))
        .and_then(|rest| rest.split_whitespace().next())
        .map(str::to_string)
        .expect("输出缺少 [Multi-leg · Backtest] result_hashes")
}

fn attribution_artifact(root: &Path) -> serde_json::Value {
    let runs = root.join("runs");
    let path = std::fs::read_dir(&runs)
        .expect("读取 runs 目录失败")
        .filter_map(|entry| entry.ok().map(|entry| entry.path()))
        .find(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.ends_with(".spread-attribution.json"))
        })
        .expect("缺少多腿归因产物");
    let payload = std::fs::read_to_string(&path).expect("读取多腿归因产物失败");
    serde_json::from_str(&payload).expect("多腿归因产物 JSON 非法")
}

fn field(fields: &BTreeMap<String, String>, key: &str) -> i128 {
    fields
        .get(key)
        .unwrap_or_else(|| panic!("归因摘要缺少 {key}"))
        .parse::<i128>()
        .unwrap_or_else(|_| panic!("归因摘要 {key} 不是整数"))
}

/// 从 CLI 产物里取出一组组归因，并交给领域类型复核合计一致性。
fn validated_groups(payload: &serde_json::Value) -> Vec<qx_zhenlu::SpreadGroupAttribution> {
    payload["groups"]
        .as_array()
        .expect("groups 必须是数组")
        .iter()
        .map(|group| {
            qx_zhenlu::SpreadGroupAttribution::from_json(&group.to_string())
                .unwrap_or_else(|error| panic!("组级归因未通过领域校验: {error}"))
        })
        .collect()
}

#[test]
fn spot_multi_leg_attribution_closes_fees_and_stays_deterministic() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let legs = [primary.as_str(), reference.as_str()];
    let root = temp_root("spot");
    let first = root.join("first");
    let (code, stdout, stderr) = backtest(&first, "2", "0", &legs);
    assert_eq!(code, 0, "现货多腿回测失败: {stderr}");
    let fields = attribution_fields(&stdout);
    let turnover_raw = field(&fields, "turnover_raw");
    let fees_raw = field(&fields, "fees_raw");
    assert_eq!(field(&fields, "groups"), 3);
    assert_eq!(field(&fields, "residual_filled_qty_raw"), 0);
    assert_eq!(field(&fields, "residual_fees_raw"), 0);
    // 现货没有保证金与资金费；费用必须是名义额的 taker 5bp，而不是 1e9 分之一。
    assert_eq!(field(&fields, "margin_peak_raw"), 0);
    assert_eq!(field(&fields, "funding_raw"), 0);
    assert_eq!(turnover_raw, 385_200_000_000_000);
    assert_eq!(fees_raw, turnover_raw * 5 / 10_000);
    assert_eq!(field(&fields, "net_cost_raw"), fees_raw);
    assert_eq!(field(&fields, "cost_bps"), 5);

    // Q0e：成交诚实性摘要必须与归因同源——计划量全部成交，才允许没有任何裸腿。
    let integrity = integrity_fields(&stdout);
    for leg in ["primary", "reference"] {
        assert_eq!(
            integrity
                .get(&format!("{leg}_unfilled_planned_qty_raw"))
                .map(String::as_str),
            Some("0"),
            "{leg} 腿计划与成交必须一致"
        );
        assert_eq!(
            integrity
                .get(&format!("{leg}_filled_qty_raw"))
                .map(String::as_str),
            integrity
                .get(&format!("{leg}_planned_qty_raw"))
                .map(String::as_str)
        );
        assert_eq!(field(&integrity, &format!("{leg}_vetoed_signals")), 0);
        assert_eq!(field(&integrity, &format!("{leg}_rejected_orders")), 0);
        assert_eq!(
            integrity
                .get(&format!("{leg}_rejection_reasons"))
                .map(String::as_str),
            Some("none")
        );
    }
    assert_eq!(field(&reconcile_fields(&stdout), "pending"), 0);

    let payload = attribution_artifact(&first);
    assert_eq!(payload["schema_version"], 2);
    assert_eq!(
        payload["pending_reconcile"]["items"]
            .as_array()
            .expect("items 必须是数组")
            .len(),
        0
    );
    assert_eq!(
        payload["legs"].as_array().expect("legs 必须是数组").len(),
        2
    );
    assert_eq!(payload["strategy_id"], "builtin-pairs_arbitrage-multi");
    assert_eq!(payload["totals"]["fees_raw"], fees_raw.to_string());
    assert_eq!(payload["totals"]["turnover_raw"], turnover_raw.to_string());
    assert_eq!(payload["totals"]["residual_fees_raw"], "0");
    // Q58：零保证金/零资金费必须说清是"现货口径"还是"漏配规格"，只报数字分不出这两件事。
    assert_eq!(payload["market_specs"]["primary"], serde_json::Value::Null);
    assert_eq!(
        payload["market_specs"]["reference"],
        serde_json::Value::Null
    );
    assert_eq!(payload["margin_model"], "none-no-derivative-leg-spec");
    assert_eq!(payload["totals"]["margin_peak_raw"], "0");
    assert_eq!(payload["totals"]["funding_raw"], "0");
    let groups = validated_groups(&payload);
    assert_eq!(groups.len(), 3);
    let summed_fees: i128 = groups
        .iter()
        .map(|group| group.legs.iter().map(|leg| leg.fees_raw).sum::<i128>())
        .sum();
    assert_eq!(
        summed_fees, fees_raw,
        "腿级费用合计必须等于摘要费用，FIFO 分配不得漏计"
    );
    assert!(groups.iter().all(|group| group.legs.len() == 2));
    assert!(groups
        .iter()
        .all(|group| group.total_fees_raw > 0 && group.total_turnover_raw > 0));

    let second = root.join("second");
    let (code, second_stdout, _) = backtest(&second, "2", "0", &legs);
    assert_eq!(code, 0);
    assert_eq!(
        result_hashes(&second_stdout),
        result_hashes(&stdout),
        "同参数二次回测结果指纹漂移"
    );
    assert_eq!(attribution_fields(&second_stdout), fields);

    let changed = root.join("changed");
    let (code, changed_stdout, _) = backtest(&changed, "1", "0", &legs);
    assert_eq!(code, 0);
    assert_ne!(
        result_hashes(&changed_stdout),
        result_hashes(&stdout),
        "数量变化必须改变结果指纹"
    );
    assert_eq!(
        field(&attribution_fields(&changed_stdout), "filled_qty_raw"),
        6_000_000_000
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn derivative_multi_leg_attribution_charges_margin_and_funding_on_realized_fills() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let primary_spec = fixture("qianxing.market.binance.btc-swap.spec.json");
    let reference_spec = fixture("qianxing.market.binance.eth-swap.spec.json");
    let legs = [
        primary.as_str(),
        reference.as_str(),
        primary_spec.as_str(),
        reference_spec.as_str(),
    ];
    let root = temp_root("swap");
    let margined = root.join("margined");
    let (code, stdout, stderr) = backtest(&margined, "1", "30", &legs);
    assert_eq!(code, 0, "衍生品多腿回测失败: {stderr}");
    let fields = attribution_fields(&stdout);
    // 两条腿都成交：保证金按 |实际成交持仓| × bar 收盘 × 1 倍杠杆。
    assert_eq!(field(&fields, "margin_peak_raw"), 64_200_000_000_000);
    assert_eq!(field(&fields, "funding_raw"), -39_600_000_000);
    assert_eq!(field(&fields, "residual_fees_raw"), 0);
    assert_eq!(field(&fields, "residual_filled_qty_raw"), 0);

    let zero_funding = root.join("zero-funding");
    let (code, zero_stdout, _) = backtest(&zero_funding, "1", "0", &legs);
    assert_eq!(code, 0);
    let zero_fields = attribution_fields(&zero_stdout);
    assert_eq!(field(&zero_fields, "funding_raw"), 0);
    assert_eq!(
        field(&zero_fields, "margin_peak_raw"),
        field(&fields, "margin_peak_raw"),
        "资金费率不应改变保证金口径"
    );
    assert_eq!(
        field(&zero_fields, "net_cost_raw"),
        field(&zero_fields, "fees_raw"),
        "资金费为 0 时净成本等于费用"
    );

    let doubled = root.join("doubled");
    let (code, doubled_stdout, _) = backtest(&doubled, "1", "60", &legs);
    assert_eq!(code, 0);
    let doubled_fields = attribution_fields(&doubled_stdout);
    assert_eq!(
        field(&doubled_fields, "funding_raw"),
        field(&fields, "funding_raw") * 2,
        "资金费必须随费率线性缩放"
    );
    assert_eq!(
        field(&doubled_fields, "fees_raw"),
        field(&fields, "fees_raw"),
        "资金费率不得改变费用合计"
    );
    let _ = std::fs::remove_dir_all(&root);
}

#[test]
fn multi_leg_attribution_rejects_unknown_flags() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let (code, _, stderr) = run(&[
        "backtest",
        "multi-builtin",
        "pairs_arbitrage",
        primary.as_str(),
        reference.as_str(),
        "--typo",
    ]);
    assert_eq!(code, 2, "未知参数没有被拒绝: {stderr}");
    assert!(stderr.contains("未知参数"), "报错未指出未知参数: {stderr}");
}

/// 指定策略 kind 与额外旗标跑一次多腿回测。
fn multi_backtest(
    out: &Path,
    kind: &str,
    quantity: &str,
    funding_bps: &str,
    legs: &[&str],
    extra: &[&str],
) -> (i32, String, String) {
    let root = out.to_string_lossy().to_string();
    let mut args = vec!["backtest", "multi-builtin", kind];
    args.extend(legs.iter().copied());
    args.extend([
        "--quantity",
        quantity,
        "--funding-bps",
        funding_bps,
        "--root",
        root.as_str(),
    ]);
    args.extend(extra.iter().copied());
    run(&args)
}

/// V11 Q0e：四条多腿 kind 都必须真的成交并完成归因，且计划量没有落空。
/// 只有 `pairs_arbitrage` 有用例时，另外三条 kind 的装配（腿序、规格、归因）
/// 可以整体坏掉而无人报警。
#[test]
fn every_multi_leg_kind_reports_fully_filled_legs() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let legs = [primary.as_str(), reference.as_str()];
    let root = temp_root("kinds");
    for kind in [
        "pairs_arbitrage",
        "basis_arbitrage",
        "cross_venue_arbitrage",
        "spot_futures_arbitrage",
    ] {
        let out = root.join(kind);
        let (code, stdout, stderr) = multi_backtest(&out, kind, "2", "0", &legs, &[]);
        assert_eq!(code, 0, "{kind} 多腿回测失败: {stderr}");
        let fields = attribution_fields(&stdout);
        assert!(
            field(&fields, "groups") > 0 && field(&fields, "turnover_raw") > 0,
            "{kind} 没有产生任何成组套利：归因链路是死的"
        );
        assert_eq!(field(&fields, "residual_filled_qty_raw"), 0);
        assert_eq!(field(&fields, "residual_fees_raw"), 0);
        let integrity = integrity_fields(&stdout);
        for leg in ["primary", "reference"] {
            assert!(
                field(&integrity, &format!("{leg}_filled_qty_raw")) > 0,
                "{kind} 的 {leg} 腿零成交"
            );
            assert_eq!(
                field(&integrity, &format!("{leg}_unfilled_planned_qty_raw")),
                0,
                "{kind} 的 {leg} 腿有计划落空"
            );
        }
        assert_eq!(field(&reconcile_fields(&stdout), "pending"), 0);
        // 费用闭合：组级合计必须等于两腿独立合计减去未配对残余。
        let payload = attribution_artifact(&out);
        let grouped_fees: i128 = validated_groups(&payload)
            .iter()
            .map(|group| group.total_fees_raw)
            .sum();
        assert_eq!(grouped_fees, field(&fields, "fees_raw"));
        assert_eq!(payload["strategy_id"], format!("builtin-{kind}-multi"));
    }
    let _ = std::fs::remove_dir_all(&root);
}

/// V11 Q0e 的反向验证用例：一条腿被风控挡下时，另一条腿的成交**不能**再配成组，
/// 而是必须作为裸腿出现在 `pending_reconcile` 与 `residual_*` 里。
/// 把 `multi_leg_group_attributions` 的数量口径改回信号计划，本用例立即变红。
#[test]
fn vetoed_leg_never_pairs_against_a_filled_counterpart() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    // 配置声称产品是 perpetual，因此两条腿必须各自带 market spec（V11 Q58）：缺规格的
    // 名义额会按现货乘数 1 记账，那条名义额上限就变成对猜出来的口径做风控。
    let primary_spec = fixture("qianxing.market.binance.btc-swap.spec.json");
    let reference_spec = fixture("qianxing.market.binance.eth-swap.spec.json");
    let legs = [
        primary.as_str(),
        reference.as_str(),
        primary_spec.as_str(),
        reference_spec.as_str(),
    ];
    let root = temp_root("veto");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("创建风控配置目录失败");
    // 单腿名义额上限落在 ETH 腿（6e12）与 BTC 腿（1.2e14）之间：主腿每笔买都被
    // 前置风控挡下，对冲腿照常成交——这正是"一腿有、一腿没有"的裸腿场景。
    let mut config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("qianxing.runtime.example.json"))
            .expect("读取运行时示例配置失败"),
    )
    .expect("运行时示例配置 JSON 非法");
    config["strategy"]["product"] = serde_json::json!("perpetual");
    config["strategy"]["allow_short"] = serde_json::json!(true);
    config["strategy"]["risk_rules"] = serde_json::json!({
        "version": "q0e-notional-cap-v1",
        "max_notional_raw": 10_000_000_000_000_i64,
    });
    let config_path = config_dir.join("runtime.json");
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("写入运行时配置失败");
    let config_arg = config_path.to_string_lossy().to_string();

    let out = root.join("run");
    let (code, stdout, stderr) = multi_backtest(
        &out,
        "pairs_arbitrage",
        "2",
        "0",
        &legs,
        &["--config", config_arg.as_str()],
    );
    assert_eq!(code, 0, "风控挡腿场景回测失败: {stderr}");
    let integrity = integrity_fields(&stdout);
    assert_eq!(
        field(&integrity, "primary_filled_qty_raw"),
        0,
        "主腿本应被名义额上限全部挡下"
    );
    assert!(field(&integrity, "primary_rejected_orders") > 0);
    assert!(field(&integrity, "primary_vetoed_signals") > 0);
    let naked = field(&integrity, "reference_filled_qty_raw");
    assert!(naked > 0, "对冲腿应当照常成交，否则场景不成立");

    let fields = attribution_fields(&stdout);
    assert_eq!(field(&fields, "groups"), 0, "单腿成交绝不能配成套利组");
    assert_eq!(field(&fields, "turnover_raw"), 0);
    assert_eq!(
        field(&fields, "residual_filled_qty_raw"),
        naked,
        "裸腿成交必须原样计入 residual，不得静默丢弃"
    );
    let reconcile = reconcile_fields(&stdout);
    assert!(field(&reconcile, "pending") > 0, "裸腿必须登记为待对账");
    assert_eq!(field(&reconcile, "naked_filled_qty_raw"), naked);

    let payload = attribution_artifact(&out);
    let items = payload["pending_reconcile"]["items"]
        .as_array()
        .expect("pending_reconcile.items 必须是数组");
    assert_eq!(items.len(), field(&reconcile, "pending") as usize);
    for item in items {
        assert_eq!(item["leg"], "reference");
        assert_eq!(item["counterpart"], "primary");
        assert_eq!(item["counterpart_filled_qty_raw"], "0");
    }
    assert_eq!(
        payload["pending_reconcile"]["policy"],
        "mark-pending-reconcile-no-auto-close"
    );
    let legs_payload = payload["legs"].as_array().expect("legs 必须是数组");
    assert_eq!(legs_payload.len(), 2);
    assert_eq!(legs_payload[0]["unfilled_planned_qty_raw"], "6000000000");
    assert_eq!(legs_payload[0]["filled_qty_raw"], "0");
    assert!(
        legs_payload[0]["rejection_reasons"]
            .as_array()
            .is_some_and(|reasons| !reasons.is_empty()),
        "拒单原因必须写进产物，而不是只留在日志里"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// V11 Q0e：单腿定资算不出来时必须**报错**，不能把所需现金截断到可表示上限后
/// 让回测以零成交静默通过（§4.18 的同一类失真）。
#[test]
fn multi_leg_funding_bound_fails_loudly_instead_of_capping_cash() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let legs = [primary.as_str(), reference.as_str()];
    let root = temp_root("funding");
    let out = root.join("huge");
    let (code, stdout, stderr) = multi_backtest(
        &out,
        "pairs_arbitrage",
        "90000000000000000",
        "0",
        &legs,
        &[],
    );
    assert_ne!(
        code, 0,
        "买不起的计划必须失败，而不是静默跑成零成交: {stdout}"
    );
    assert!(
        stderr.contains("超出账户可表示上限"),
        "报错未指出定资上限: {stderr}"
    );
    assert!(
        stderr.contains("quantity"),
        "报错必须给出可执行的修正方向: {stderr}"
    );
    assert!(
        !stdout.contains("[Multi-leg · Attribution]"),
        "定资失败后不得产出归因摘要"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 在临时目录落一份 CCXT 形状的现货规格：与仓库里的 swap 规格只差 `market_type`，
/// 用来把"产品形态"单独变成一个可对照的变量。
fn write_spot_spec(dir: &Path, name: &str, base: &str) -> String {
    let path = dir.join(name);
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({
            "market_type": "spot",
            "base": base,
            "quote": "USDT",
            "settle": "USDT",
            "contract_size_raw": 1_000_000_000_i64,
            "price_tick_raw": 1_000_000_i64,
            "qty_step_raw": 1_000_000_i64,
            "min_qty_raw": 1_000_000_i64,
        }))
        .expect("编码现货规格失败"),
    )
    .expect("写入现货规格失败");
    path.to_string_lossy().to_string()
}

/// 按 `leg_id` 汇总组级归因的某一列（产物里是 `i128` 十进制字符串）。
fn leg_column_sum(payload: &serde_json::Value, leg_id: &str, column: &str) -> i128 {
    payload["groups"]
        .as_array()
        .expect("groups 必须是数组")
        .iter()
        .flat_map(|group| group["legs"].as_array().expect("legs 必须是数组").iter())
        .filter(|leg| leg["leg_id"].as_str() == Some(leg_id))
        .map(|leg| {
            leg[column]
                .as_str()
                .unwrap_or_else(|| panic!("组级归因缺少 {leg_id}.{column}"))
                .parse::<i128>()
                .unwrap_or_else(|error| panic!("组级归因 {leg_id}.{column} 非法: {error}"))
        })
        .sum()
}

/// V11 Q58：`--funding-bps` 非零却没有腿规格时必须在撮合前报错。缺规格的腿按现货乘数 1
/// 记账，跑完再补一句"资金费为 0"分不清"本来就不该有"与"规格漏了"。
#[test]
fn funding_without_leg_spec_is_refused_before_matching() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let legs = [primary.as_str(), reference.as_str()];
    let root = temp_root("spec-guard-missing");
    let out = root.join("run");
    let (code, stdout, stderr) = backtest(&out, "2", "30", &legs);
    assert_ne!(
        code, 0,
        "缺规格却要计提资金费必须失败，而不是静默记 0: {stdout}"
    );
    assert!(
        stderr.contains("腿没有 market spec"),
        "报错必须指出缺的是规格: {stderr}"
    );
    assert!(
        stderr.contains("primary 腿"),
        "报错必须点名哪条腿缺规格: {stderr}"
    );
    assert!(
        stderr.contains("--funding-bps"),
        "报错必须给出可执行的修正方向: {stderr}"
    );
    assert!(
        !stdout.contains("[Multi-leg · Backtest]"),
        "规格闸门必须早于任何腿级撮合: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 两条腿都声明为现货时，非零资金费率无处计提：落到现货腿上就是造一笔不存在的成本。
#[test]
fn funding_on_spot_only_legs_is_refused() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let root = temp_root("spec-guard-spot-only");
    let primary_spec = write_spot_spec(&root, "btc-spot.spec.json", "BTC");
    let reference_spec = write_spot_spec(&root, "eth-spot.spec.json", "ETH");
    let legs = [
        primary.as_str(),
        reference.as_str(),
        primary_spec.as_str(),
        reference_spec.as_str(),
    ];
    let out = root.join("run");
    let (code, stdout, stderr) = backtest(&out, "2", "30", &legs);
    assert_ne!(code, 0, "全现货腿计提资金费必须失败: {stdout}");
    assert!(
        stderr.contains("没有一条腿是衍生品"),
        "报错必须指出无处计提: {stderr}"
    );
    assert!(
        stderr.contains("primary=Some(Spot)"),
        "报错必须带回两条腿的实际产品形态: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 混合规格只向衍生品腿计费：现货腿的保证金/资金费必须恒为 0，且合计与逐腿之和闭合。
/// 反向验证：去掉 `multi_leg_leg_margin` 与资金费循环里的衍生品判定，本用例立刻变红
/// ——现货腿会按全额名义额背上一笔保证金。
#[test]
fn only_derivative_legs_bear_margin_and_funding() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let root = temp_root("spec-guard-mixed");
    let primary_spec = fixture("qianxing.market.binance.btc-swap.spec.json");
    let reference_spec = write_spot_spec(&root, "eth-spot.spec.json", "ETH");
    let legs = [
        primary.as_str(),
        reference.as_str(),
        primary_spec.as_str(),
        reference_spec.as_str(),
    ];
    let out = root.join("run");
    let (code, stdout, stderr) = backtest(&out, "1", "30", &legs);
    assert_eq!(code, 0, "衍生品主腿 + 现货对冲腿的回测失败: {stderr}");
    let fields = attribution_fields(&stdout);
    let payload = attribution_artifact(&out);
    assert!(
        field(&fields, "margin_peak_raw") > 0 && field(&fields, "funding_raw") != 0,
        "主腿本该被计提，否则本用例什么都没验到: {fields:?}"
    );
    assert_eq!(leg_column_sum(&payload, "reference", "margin_raw"), 0);
    assert_eq!(leg_column_sum(&payload, "reference", "funding_raw"), 0);
    assert!(leg_column_sum(&payload, "primary", "margin_raw") > 0);
    assert_eq!(
        leg_column_sum(&payload, "primary", "funding_raw"),
        field(&fields, "funding_raw"),
        "全部资金费必须落在衍生品腿上，合计才与逐腿之和闭合"
    );
    assert_eq!(
        payload["margin_model"],
        "realized-initial-margin-leverage-1"
    );
    assert_eq!(
        payload["market_specs"]["reference"],
        serde_json::Value::String(reference_spec)
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 配置把产品声明成衍生品、却没给主腿规格：即使 `--funding-bps 0`（保证金仍会漏计）
/// 也必须拒绝，否则名义额按现货乘数 1 记账，回测与实盘的产品口径就此分叉。
#[test]
fn declared_derivative_product_without_primary_spec_is_refused() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let legs = [primary.as_str(), reference.as_str()];
    let root = temp_root("spec-guard-declared");
    let config_dir = root.join("config");
    std::fs::create_dir_all(&config_dir).expect("创建配置目录失败");
    let mut config: serde_json::Value = serde_json::from_str(
        &std::fs::read_to_string(fixture("qianxing.runtime.example.json"))
            .expect("读取运行时示例配置失败"),
    )
    .expect("运行时示例配置 JSON 非法");
    config["strategy"]["product"] = serde_json::json!("perpetual");
    config["strategy"]["allow_short"] = serde_json::json!(true);
    let config_path = config_dir.join("runtime.json");
    std::fs::write(&config_path, serde_json::to_string_pretty(&config).unwrap())
        .expect("写入运行时配置失败");
    let config_arg = config_path.to_string_lossy().to_string();

    let out = root.join("run");
    let (code, stdout, stderr) = multi_backtest(
        &out,
        "pairs_arbitrage",
        "2",
        "0",
        &legs,
        &["--config", config_arg.as_str()],
    );
    assert_ne!(
        code, 0,
        "声称衍生品却没有主腿规格必须失败: {stdout}
{stderr}"
    );
    assert!(
        stderr.contains("必须提供 primary 腿的 market spec"),
        "报错必须点名主腿规格: {stderr}"
    );
    assert!(
        stderr.contains("strategy.product=Perpetual"),
        "报错必须带回声明值: {stderr}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// V11 Q71：`combined_return_bps` 必须是两条腿**按钱合起来**算的组合收益。
///
/// 两条腿的本金各按本腿自己的行情定资（这份夹具在 `quantity=100` 下主腿正好是对冲腿的 21 倍
/// 本金），所以"把两个腿级 `return_bps` 平均"念出来的既不是组合收益率也不是任何一条腿的收益
/// 率。本用例拿产物自己声明的两腿本金与期末权益重算一遍，要求 stdout 上那个数等于钱口径，并且
/// **不等于**平均值——两者在这份真实夹具上就是两个不同的数。
#[test]
fn combined_return_pools_both_legs_by_capital_instead_of_averaging_bps() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let root = temp_root("combined-return");
    let (code, stdout, stderr) =
        backtest(&root, "100", "0", &[primary.as_str(), reference.as_str()]);
    assert_eq!(code, 0, "多腿回测必须跑通: {stderr}");
    let line = stdout
        .lines()
        .find(|line| line.starts_with("[Multi-leg · Backtest]"))
        .expect("输出缺少 [Multi-leg · Backtest]");
    // 这一行写了两腿各自的 `return_bps=` 与一颗 `combined_return_bps=`；按整段 token 前缀取，
    // 才不会把组合那颗也当成腿级的。
    let leg_returns: Vec<i128> = line
        .split_whitespace()
        .filter_map(|token| token.strip_prefix("return_bps="))
        .map(|value| value.parse::<i128>().expect("腿级 return_bps 必须是整数"))
        .collect();
    assert_eq!(
        leg_returns.len(),
        2,
        "这一行必须写出两条腿各自的收益率: {line}"
    );
    let printed = line_fields(&stdout, "[Multi-leg · Backtest]");
    let combined = field(&printed, "combined_return_bps");

    let payload = attribution_artifact(&root);
    let funded = |key: &str, table: &str| -> i128 {
        payload[table][key]
            .as_str()
            .unwrap_or_else(|| panic!("产物缺少 {table}/{key}"))
            .parse::<i128>()
            .unwrap_or_else(|_| panic!("产物 {table}/{key} 不是整数字符串"))
    };
    let initial = [
        funded("primary_initial_cash", "accounts"),
        funded("reference_initial_cash", "accounts"),
    ];
    let final_equity = [
        funded("primary_final_equity_raw", "accounts"),
        funded("reference_final_equity_raw", "accounts"),
    ];
    assert_ne!(
        initial[0], initial[1],
        "两腿本金必须不等，否则下面的对比是空的: {initial:?}"
    );
    let funded_raw = initial[0] + initial[1];
    let pooled = (final_equity[0] + final_equity[1] - funded_raw) * 10_000 / funded_raw;
    assert_eq!(
        combined, pooled,
        "组合收益必须是两条腿合计盈亏 ÷ 两条腿合计本金: stdout={combined} 钱口径={pooled}"
    );
    assert_eq!(
        payload["totals"]["combined_return_bps"]
            .as_i64()
            .expect("产物缺少 totals/combined_return_bps 或非整数"),
        i64::try_from(pooled).expect("组合收益应在 i64 范围内"),
        "产物里那颗组合收益必须与 stdout 是同一份事实"
    );
    assert_ne!(
        combined,
        (leg_returns[0] + leg_returns[1]) / 2,
        "两腿分别 {leg_returns:?}bp：等权平均会念 {}bp，那正是 Q71 修掉的口径",
        (leg_returns[0] + leg_returns[1]) / 2
    );
    let _ = std::fs::remove_dir_all(&root);
}
