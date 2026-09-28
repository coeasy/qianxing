use super::*;

/// 在临时目录落一份 CCXT 形状的现货规格：与仓库里的 swap 规格只差 `market_type`，
/// 用来把"产品形态"单独变成一个可对照的变量。
fn write_spot_spec(dir: &Path, name: &str, base: &str) -> String {
    write_spot_spec_settled(dir, name, base, "USDT")
}

/// 同 [`write_spot_spec`]，但把结算币种单独留成变量：两条腿各自记账在哪本账上，
/// 只有从这里才能造出"同一条链上两种钱"的组合（V11 D 轮 S2）。
fn write_spot_spec_settled(dir: &Path, name: &str, base: &str, settle: &str) -> String {
    let path = dir.join(name);
    std::fs::write(
        &path,
        serde_json::to_string(&serde_json::json!({
            "market_type": "spot",
            "base": base,
            "quote": "USDT",
            "settle": settle,
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

/// V11 D 轮 S2：`combined_return_bps` 把两条腿的本金与权益**相加**，所以两条腿必须记在
/// 同一本账上。一条 USDT 结算、一条 USD 结算的两腿跑出来的那个数是两种钱的和，任何一本
/// 账上都不存在它，产物却会照常落盘 —— 币种不同的两腿只能在规格闸门处被拒。
#[test]
fn legs_on_different_booking_currencies_are_refused_before_matching() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let root = temp_root("spec-guard-currency");
    let primary_spec = write_spot_spec_settled(&root, "btc-usdt.spec.json", "BTC", "USDT");
    let reference_spec = write_spot_spec_settled(&root, "eth-usd.spec.json", "ETH", "USD");
    let legs = [
        primary.as_str(),
        reference.as_str(),
        primary_spec.as_str(),
        reference_spec.as_str(),
    ];
    let out = root.join("run");
    let (code, stdout, stderr) = backtest(&out, "2", "0", &legs);
    assert_ne!(
        code, 0,
        "两条腿记账币种不同仍必须失败，而不是把两种钱相加: {stdout}"
    );
    assert!(
        stderr.contains("两条腿的记账币种不同"),
        "报错必须点名币种不可通约: {stderr}"
    );
    assert!(
        stderr.contains("primary=USDT") && stderr.contains("reference=USD"),
        "报错必须带回两条腿各自记账在哪本账上: {stderr}"
    );
    assert!(
        !stdout.contains("[Multi-leg · Backtest]"),
        "币种闸门必须早于任何腿级撮合: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 币种闸门问的必须是腿级引擎真正拿去落现金腿的那一份口径，而不是 spec 里的另一格字段。
/// 缺 spec 的腿按引擎默认 USDT 记账（单腿链同一兜底），所以"一条显式 USDT
/// 规格 + 一条无规格"是在同一本账上，不得被拒；把它换成就地抄 spec 的写法
/// （`spec.map(|s| s.settlement_currency.clone()).unwrap_or_default()`）就得到一个空串，
/// 于是"只给主腿补规格"这一合法形状被当场挡在链外（V11 D 轮 S2 的另一半）。
#[test]
fn leg_without_a_spec_is_compared_by_the_currency_it_actually_books_in() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let root = temp_root("spec-guard-default-currency");
    let primary_spec = write_spot_spec_settled(&root, "btc-usdt.spec.json", "BTC", "USDT");
    let legs = [primary.as_str(), reference.as_str(), primary_spec.as_str()];
    let out = root.join("run");
    let (code, stdout, stderr) = backtest(&out, "2", "0", &legs);
    assert_eq!(
        code, 0,
        "无规格的 reference 腿按引擎默认 USDT 记账，与显式 USDT 主腿同账，不该被币种闸门拒: {stderr}"
    );
    assert!(
        stdout.contains("[Multi-leg · Backtest]"),
        "闸门放行后必须真的跑到腿级撮合: {stdout}"
    );
    let _ = std::fs::remove_dir_all(&root);
}

/// 同一兜底的反向那一半：缺规格的腿报错时必须报成它真正记账的那个币种，而不是空着。
/// `primary=USD、reference=USDT` 说的是"两条腿各在哪本账上"，写成 `reference=` 就等于
/// 让读者自己去猜那份没写出来的默认值。
#[test]
fn missing_spec_reports_the_default_booking_currency_instead_of_an_empty_one() {
    let primary = fixture("qianxing.bar-frame.pairs-primary.example.json");
    let reference = fixture("qianxing.bar-frame.pairs-reference.example.json");
    let root = temp_root("spec-guard-default-name");
    let primary_spec = write_spot_spec_settled(&root, "btc-usd.spec.json", "BTC", "USD");
    let legs = [primary.as_str(), reference.as_str(), primary_spec.as_str()];
    let out = root.join("run");
    let (code, stdout, stderr) = backtest(&out, "2", "0", &legs);
    assert_ne!(
        code, 0,
        "USD 主腿与 USDT 默认账的 reference 腿必须被拒: {stdout}"
    );
    assert!(
        stderr.contains("两条腿的记账币种不同"),
        "报错必须点名币种不可通约: {stderr}"
    );
    assert!(
        stderr.contains("primary=USD") && stderr.contains("reference=USDT"),
        "缺规格的那条腿要报出它实际记账的币种，不能留空: {stderr}"
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
