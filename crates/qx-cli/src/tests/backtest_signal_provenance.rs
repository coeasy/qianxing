//! 生效信号口径落进回测产物摘要（V12 R4-j）。
//!
//! 改之前：那行 `[Strategy · Signal] source=… fast_window=… period=… threshold_bps=…` 只印在
//! stdout，摘要里没有这套参数。读完产物的人既不知道这一轮跑的是 fast=5 还是 fast=20，也无从
//! 区分"没人提这一项"与"配置里写了与默认相同的值"。Q64 修的是"参数被静默忽略"，这一轮修它的
//! 另一半：参数确实生效了，但产物不认账。

use super::*;

/// 用给定的 `strategy.builtin_*` 四项跑一遍策略链，返回摘要。
///
/// `kind` 由用例点名：内核对各 kind 读哪几项参数并不一致（清单在 `builtin_signal.rs`，
/// MACD 的 12/26/9 是内核常数），"换一项结果必须换"这一条只能在该项确实上场的 kind
/// 上验证。下面那两条窗口用例选 `sma_cross`：快慢线两档都进信号。
fn strategy_summary_with_builtin_signal(
    label: &str,
    kind: &str,
    fast: Option<usize>,
    slow: Option<usize>,
    period: Option<usize>,
    threshold: Option<i128>,
) -> serde_json::Value {
    let (deploy, frame, template) = builtin_backtest_example_paths();
    let mut config = read_runtime_config(&template).unwrap();
    config.strategy.builtin_strategy = Some(kind.into());
    config.strategy.builtin_fast_window = fast;
    config.strategy.builtin_slow_window = slow;
    config.strategy.builtin_period = period;
    config.strategy.builtin_threshold_bps = threshold;
    let (root, runtime) = isolated_backtest_runtime(&deploy, &config, label);
    run_strategy_backtest(&runtime, &frame, None).unwrap();
    let summary = read_first_backtest_summary(&root);
    let _ = std::fs::remove_dir_all(root);
    summary
}

/// 摘要里那一格的文本取值；缺键或形状不对都直接 panic，用例要红在"没写"而不是"读成 0"。
fn signal_text(summary: &serde_json::Value, key: &str) -> String {
    summary["signal"][key]
        .as_str()
        .unwrap_or_else(|| {
            panic!(
                "摘要的 signal.{key} 必须落盘为字符串: {}",
                summary["signal"]
            )
        })
        .to_string()
}

fn signal_usize(summary: &serde_json::Value, key: &str) -> usize {
    summary["signal"][key]
        .as_u64()
        .unwrap_or_else(|| panic!("摘要的 signal.{key} 必须落盘为整数: {}", summary["signal"]))
        as usize
}

/// 换一项窗口：摘要跟着换，结果哈希也必须跟着换——否则摘要写的不是生效的那套。
#[test]
fn the_summary_tracks_the_effective_windows_and_the_result_changes() {
    let default_run =
        strategy_summary_with_builtin_signal("r4j-default", "sma_cross", None, None, None, None);
    // 没人提这一项：来源必须说 builtin-default，而不是留空让人以为核对过。
    assert_eq!(signal_text(&default_run, "source"), "builtin-default");
    // kind 说的是配置点名的那个策略：摘要写着 macd 而跑的是 sma_cross 也算说谎。
    assert_eq!(signal_text(&default_run, "kind"), "sma_cross");

    // 取一个与缺省不同的快线窗口：唯一变量就是这一项。
    let changed_window = signal_usize(&default_run, "fast_window") + 3;
    let moved = strategy_summary_with_builtin_signal(
        "r4j-fast",
        "sma_cross",
        Some(changed_window),
        None,
        None,
        None,
    );
    assert_eq!(signal_usize(&moved, "fast_window"), changed_window);
    assert_eq!(signal_text(&moved, "source"), "config");
    assert_eq!(
        signal_usize(&moved, "slow_window"),
        signal_usize(&default_run, "slow_window"),
        "只给 fast 时 slow 必须留在原处，否则来源标注无从判断"
    );
    assert_ne!(
        moved["result_hash"], default_run["result_hash"],
        "摘要写着换了窗口，结果哈希却一动不动，说明写进去的不是生效的那套"
    );
}

/// "没配" 与 "配了同样的值" 必须在产物里可区分，而结果必须一致。
#[test]
fn declaring_the_default_values_says_config_without_changing_the_result() {
    let default_run =
        strategy_summary_with_builtin_signal("r4j-plain", "sma_cross", None, None, None, None);
    let explicit = strategy_summary_with_builtin_signal(
        "r4j-explicit-default",
        "sma_cross",
        Some(signal_usize(&default_run, "fast_window")),
        Some(signal_usize(&default_run, "slow_window")),
        Some(signal_usize(&default_run, "period")),
        Some(signal_text(&default_run, "threshold_bps").parse().unwrap()),
    );
    for key in [
        "fast_window",
        "slow_window",
        "period",
        "threshold_bps",
        "quantity_raw",
    ] {
        assert_eq!(
            explicit["signal"][key], default_run["signal"][key],
            "signal.{key} 必须与缺省那轮同值，才能只比出来源这一件事"
        );
    }
    assert_eq!(signal_text(&explicit, "source"), "config");
    assert_eq!(
        explicit["result_hash"], default_run["result_hash"],
        "四项数值都没变，结果却变了，说明生效口径另有出处"
    );
}

/// 清单外的旋钮：不改结果、不拒这一轮，但必须在产物里被点名（V12 #102）。
///
/// 改之前有两个方向都错：`fast_window>=slow_window` 这类"这个 kind 根本不读"的取值会把整轮
/// 拒掉（拒了一次结果没变过的运行），而产物又只印数值不印生效面，读者以为 5 真的被选了。
/// 内核那侧的"改不动结果"由 `qx-strategy/tests/builtin_signal_knobs.rs` 逐 kind 钉；这里钉
/// 命令面这一半：一轮都不许多失败，也不许少说话。
#[test]
fn knobs_outside_the_list_neither_fail_the_run_nor_change_the_summary_numbers() {
    let untouched =
        strategy_summary_with_builtin_signal("r4l-macd-baseline", "macd", None, None, None, None);
    assert_eq!(signal_text(&untouched, "source"), "builtin-default");
    assert_ne!(
        untouched["fills"].as_u64().unwrap_or(0),
        0,
        "零成交下「结果没变」可以永远成立，下面的对比是空的: {untouched}"
    );
    // 9/3 这一对窗口会把任何读快慢窗口的 kind 整轮拒掉，-7 越出阈值下界；macd 一项都不读，
    // 所以这四项对它全在清单外：既不许拒这一轮，也不许改结果。
    let overloaded = strategy_summary_with_builtin_signal(
        "r4l-macd-unused",
        "macd",
        Some(9),
        Some(3),
        Some(2),
        Some(-7),
    );
    assert_eq!(signal_text(&overloaded, "knobs"), "none");
    assert_eq!(
        signal_text(&overloaded, "declared_unused"),
        "builtin_fast_window,builtin_slow_window,builtin_period,builtin_threshold_bps"
    );
    assert_eq!(signal_text(&overloaded, "source"), "config");
    // 数值照原样落盘（读者看得见配置写了什么），结果必须与没写时逐字节相同。
    assert_eq!(signal_usize(&overloaded, "fast_window"), 9);
    assert_eq!(
        overloaded["result_hash"], untouched["result_hash"],
        "清单外的四项改了结果，说明 kind 旋钮清单没有约束住内核"
    );
    assert_eq!(overloaded["fills"], untouched["fills"]);
}

/// 清单只报"这个 kind 读哪几项"，与配置写了什么无关：一项都没声明时 `declared_unused` 写 none。
#[test]
fn the_list_names_the_kind_knobs_regardless_of_the_declaration() {
    let only_unused = strategy_summary_with_builtin_signal(
        "r4l-sma-unused",
        "sma_cross",
        None,
        None,
        Some(3),
        Some(9),
    );
    assert_eq!(signal_text(&only_unused, "kind"), "sma_cross");
    assert_eq!(
        signal_text(&only_unused, "knobs"),
        "fast_window,slow_window"
    );
    assert_eq!(
        signal_text(&only_unused, "declared_unused"),
        "builtin_period,builtin_threshold_bps",
        "声明了却没上场的两项必须逐项点名，让读者知道周期与阈值这一轮不参与信号"
    );
    // 只声明清单内的一项：`declared_unused` 说的是"声明了的项里谁没上场"，没声明的不算。
    let listed_only = strategy_summary_with_builtin_signal(
        "r4l-sma-listed",
        "sma_cross",
        Some(7),
        None,
        None,
        None,
    );
    assert_eq!(
        signal_text(&listed_only, "knobs"),
        "fast_window,slow_window"
    );
    assert_eq!(signal_text(&listed_only, "declared_unused"), "none");
    assert_ne!(
        listed_only["result_hash"], only_unused["result_hash"],
        "清单内的快窗换了却没换结果，说明清单反过来把活参数也说死了"
    );
}

/// 深度链同样落盘，且 `quantity_raw` 走全仓统一的定点 raw 口径（stdout 那行印的是人数）。
#[test]
fn depth_summary_declares_signal_params_with_raw_quantity() {
    const QUANTITY_UNITS: i64 = 3;
    let (deploy, _, template) = builtin_backtest_example_paths();
    let config = read_runtime_config(&template).unwrap();
    let (root, runtime) = isolated_backtest_runtime(&deploy, &config, "r4j-depth");
    let out = temp_cli_case_dir("r4j-depth-out");
    run_depth_backtest(
        "l1",
        "sma_cross",
        &deploy.join("qianxing.depth-frame.l1.example.json"),
        None,
        QUANTITY_UNITS,
        None,
        DepthExecutionModel::default(),
        &out,
        Some(&runtime),
    )
    .unwrap();
    let summary = read_first_backtest_summary(&out);
    assert_eq!(
        signal_text(&summary, "quantity_raw"),
        (QUANTITY_UNITS as i128 * qx_core::SCALE).to_string(),
        "下单数量必须按定点 raw 落盘，与 account/initial_cash_raw 同一口径"
    );
    // 示例配置声明了那四项，深度链认它（V11 Q64 那条腿）：来源必须写 config，
    // 否则摘要会把"读到了配置"说成"用了缺省值"。
    assert_eq!(signal_text(&summary, "source"), "config");
    assert_eq!(signal_text(&summary, "kind"), "sma_cross");
    // 加法不是替换：这一轮新写的块不得顶掉已有的身份格。
    for key in ["input", "account", "replay"] {
        assert!(
            summary.get(key).is_some(),
            "摘要的 {key} 块必须还在: {}",
            summary["schema_version"]
        );
    }
    for path in [root, out] {
        let _ = std::fs::remove_dir_all(path);
    }
}
