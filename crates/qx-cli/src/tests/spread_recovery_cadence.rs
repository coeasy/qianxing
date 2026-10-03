use super::*;

/// #168c 行为判据：多腿对冲恢复的节律只按"这一轮扫描有没有推进"退避，并且**永不放弃**。
///
/// 停在 `HedgeRequired` 的分组是一条腿已成交、另一条还没对冲的裸腿。照行情链那套"连续
/// 失败到上限就具名报错"处理，等于让几次网络抖动之后把敞口永久晾着 —— 那比原来的
/// 每 100ms 起灭一个 Python 子进程更危险。所以这条链要的判据是"退避有界、循环无界"。
#[test]
fn spread_recovery_backoff_is_capped_but_never_gives_up() {
    assert_eq!(
        spread_recovery_poll_delay(0),
        Duration::from_millis(100),
        "没有积压时仍是 100ms 忙轮询"
    );
    assert_eq!(
        spread_recovery_poll_delay(1),
        Duration::from_millis(500),
        "第一档退避与 `qx-core::retry` 的口径同源"
    );
    let mut previous = Duration::from_millis(100);
    for stalls in 1..=200 {
        let delay = spread_recovery_poll_delay(stalls);
        assert!(delay >= previous, "退避必须单调不减: stalls={stalls}");
        previous = delay;
    }
    assert_eq!(
        previous,
        Duration::from_secs(8),
        "原地不动 200 轮后仍要给出一个正的间隔，而不是判死这条恢复链"
    );
}

/// #168c 接线判据 + #201 的名单派生化：每一条扫描待恢复分组的循环都必须真的按共享节律
/// 自节流，而**名单从源码里扫出来**。原来这一项把文件写成两三个名字，于是 #168 修完三条
/// 循环之后，两条执行 worker 里的第四、第五处内联扫描（Binance / CCXT）不在名单里，判据
/// 一路绿着放过了"每 100ms 起灭一个子进程"的回归。
#[test]
fn every_recovery_loop_polls_through_the_shared_cadence() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("crates")
        .join("qx-cli")
        .join("src");
    let mut scanners = Vec::new();
    collect_recovery_scanners(&root, &mut scanners);
    // 已知五处扫描分布在四个文件里；名单比这个下界还少，说明取数方式本身坏了（假绿）。
    assert!(
        scanners.len() >= 4,
        "扫描待恢复分组的文件名单只剩 {:?}：派生名单取不到源码",
        scanners
    );
    for path in &scanners {
        let source = std::fs::read_to_string(path).expect("恢复扫描所在文件可读");
        let rel = path
            .strip_prefix(&root)
            .unwrap_or(path.as_path())
            .display()
            .to_string();
        assert!(
            source.contains("if pending_after >= pending_before {"),
            "{rel} 没有按\"分组没减少就算原地不动\"记账"
        );
        assert!(
            source.contains("recovery_stalls.saturating_add(1)"),
            "{rel} 的原地不动计数没有累加"
        );
        assert!(
            source.contains("spread_recovery_poll_delay("),
            "{rel} 的恢复扫描不走共享节律函数"
        );
        // 节律有两种落法：循环尾按它睡（专职恢复/纸面链），或换算成两次扫描之间的墙钟闸门
        // （执行 worker：循环尾仍按 100ms 轮询命令队列，不能为了恢复把下单时延也拖长）。
        assert!(
            source.contains("thread::sleep(spread_recovery_poll_delay(recovery_stalls))")
                || source.contains("now >= recovery_next_at"),
            "{rel} 算了 stalls 却没有据此节流：内联扫描会退回每轮起灭一次恢复尝试"
        );
        // 用墙钟闸门节流的那一类，循环尾必须仍按固定节拍轮询命令队列：否则恢复退避会把
        // 下单时延一起拖长到 8 秒 —— 那正是把专职恢复链的节律错搬到执行链上的后果。
        if source.contains("now >= recovery_next_at") {
            assert!(
                source.contains("thread::sleep(Duration::from_millis(100))"),
                "{rel} 用 recovery_next_at 节流却没有留下 100ms 的队列轮询"
            );
        }
        assert!(
            !source.contains("has_pending_spread_recovery("),
            "{rel} 用回了\"有没有待恢复\"的布尔门：那条口径数不出\"这一轮有没有推进\""
        );
    }
    // 专职恢复循环所在的文件里不许再出现固定 100ms：那份节律只剩一处定义。
    // （paper_worker.rs 与两条执行 worker 不并入这条：它们的执行循环本就按固定节拍消费队列。）
    let worker_entry = std::fs::read_to_string(root.join("worker_entry.rs")).expect("可读");
    assert!(
        !worker_entry.contains("thread::sleep(Duration::from_millis(100))"),
        "worker_entry.rs 里固定 100ms 忙轮询回来了：分组原地不动时会每秒起灭 10 个恢复尝试"
    );
}

/// 递归收集"扫描待恢复分组"的源码文件；`src/tests/` 下的用例文件里出现的那串名字是判据
/// 自己的字面量，不能算成第五处扫描点。
fn collect_recovery_scanners(dir: &Path, found: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("源码目录可枚举") {
        let path = entry.expect("目录项可读").path();
        if path.is_dir() {
            if path.file_name().and_then(|name| name.to_str()) != Some("tests") {
                collect_recovery_scanners(&path, found);
            }
            continue;
        }
        let is_rust = path.extension().and_then(|name| name.to_str()) == Some("rs");
        if !is_rust {
            continue;
        }
        let text = std::fs::read_to_string(&path).expect("源码可读");
        // 只认调用形状（实参按引用取）：`spread.rs` 里那处定义的 `root: &Path` 不算扫描点。
        if text.contains("pending_spread_recovery_groups(&") {
            found.push(path);
        }
    }
}
