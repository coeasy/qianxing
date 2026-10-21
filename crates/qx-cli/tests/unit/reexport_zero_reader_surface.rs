use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// #206 判据：`pub use` 再导出行不算读者的公共面必须真的有读者。
///
/// 立案时的形态是"两条死入口长期装活"：`run_binance_user_stream_live` / `_testnet` 这类
/// 符号的整词命中只有两处——自己的定义行与门面里那行 `pub use`。门禁的数法是"在源码里
/// 找这个名字"，而那行再导出本身就含这个名字，于是它被当成读者，符号恒绿。本轮删掉
/// 四处门面里的九个符号与两份端点常量（logs/s261 与 s261b），这条判据把口径钉成常驻：
///
/// - `pub use` 语句（跨行时整段）不算读者：它只把名字搬到另一个 crate 可见；
/// - 定义行（`pub fn|struct|const|trait|enum|static|type NAME`）不算读者；
/// - 注释行不算读者；
/// - 其余任何整词命中算一次读者，用例目录里的命中也算（用例读者是回归证明，
///   按仓库口径另走"保留 + 登记 limitation"那条路，不在本判据的删除名单里）。
///
/// 覆盖面所限：`pub use foo::*;` 这种整通配再导出无法逐个点名，本判据跳过；
/// 名字被 `as Alias` 改名后对外可见的是 Alias，按 Alias 数读者。
///
/// 取数语料不含本文件：那条复活名单是纯字面量，把自己的行算成读者就等于对"名单里的名字
/// 重新回到门面"失明 —— s262 第一次跑 M206a 就是这样绿的，那时名单判据红、通用判据恒绿，
/// 两条判据互相掩盖。名单判据靠定义点与 `pub use` 成员两种形态咬，不需要字面量语料。
///
/// 全部 `crates/**/*.rs`。与 `all_crate_production_sources()` 的分别在于这里要连用例一起数：
/// 用例读者不等于零读者，本判据只抓"整棵树里除了定义与再导出没人提"的那一面。
fn all_rust_files() -> Vec<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("crates");
    let mut files = Vec::new();
    let mut stack = vec![root];
    while let Some(dir) = stack.pop() {
        let Ok(entries) = std::fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.is_dir() {
                if path.file_name().is_some_and(|name| name == "target") {
                    continue;
                }
                stack.push(path);
            } else if path.extension().is_some_and(|ext| ext == "rs") {
                if path
                    .file_name()
                    .is_some_and(|name| name == "reexport_zero_reader_surface.rs")
                {
                    continue;
                }
                files.push(path);
            }
        }
    }
    files.sort();
    files
}

/// 报错里要报仓库相对路径：绝对路径每次跑都不一样，判据红的时候对不上 logs 里的取证。
fn display_relative(path: &Path) -> String {
    let root = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .canonicalize()
        .expect("仓库根可比对");
    let absolute = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    absolute
        .strip_prefix(&root)
        .map(|rest| rest.display().to_string().replace('\\', "/"))
        .unwrap_or_else(|_| absolute.display().to_string().replace('\\', "/"))
}

fn ident_chars(token: &str) -> String {
    token
        .chars()
        .take_while(|ch| ch.is_ascii_alphanumeric() || *ch == '_')
        .collect()
}

/// 整词命中：`build_rebalance` 不该被活着的 `build_rebalance_plan` 满足，
/// 反过来 `RebalancePlan` 也不该被 `RebalanceDelta` 满足。
fn mentions(line: &str, name: &str) -> bool {
    let bytes = line.as_bytes();
    let target = name.as_bytes();
    if target.is_empty() {
        return false;
    }
    let word = |b: u8| b.is_ascii_alphanumeric() || b == b'_';
    let mut from = 0;
    while let Some(offset) = line[from..].find(name) {
        let start = from + offset;
        let end = start + target.len();
        let left_ok = start == 0 || !word(bytes[start - 1]);
        let right_ok = end == bytes.len() || !word(bytes[end]);
        if left_ok && right_ok {
            return true;
        }
        from = start + 1;
    }
    false
}

/// 定义行：`pub <kind> NAME`，kind 取类型/函数/常量这几类声明关键词。
fn defines_line(line: &str, name: &str) -> bool {
    const KINDS: [&str; 7] = ["fn", "struct", "const", "trait", "enum", "static", "type"];
    let trimmed = line.trim_start();
    let Some(rest) = trimmed.strip_prefix("pub ") else {
        return false;
    };
    let mut tokens = rest.split_whitespace();
    let Some(kind) = tokens.next() else {
        return false;
    };
    if !KINDS.contains(&kind) {
        return false;
    }
    tokens
        .next()
        .is_some_and(|token| ident_chars(token) == name)
}

/// 收集每条 `pub use` 语句：起始行号（1 基）、覆盖的行号集合、语句文本。
fn pub_use_statements(lines: &[String]) -> Vec<(usize, BTreeSet<usize>, String)> {
    let mut statements = Vec::new();
    let mut index = 0;
    while index < lines.len() {
        let stripped = lines[index].trim();
        if !stripped.starts_with("pub use ") {
            index += 1;
            continue;
        }
        let start = index;
        let mut statement = stripped.to_string();
        let mut covered = BTreeSet::new();
        covered.insert(start + 1);
        while !statement.contains(';') && index + 1 < lines.len() {
            index += 1;
            statement.push(' ');
            statement.push_str(lines[index].trim());
            covered.insert(index + 1);
        }
        statements.push((start + 1, covered, statement));
        index += 1;
    }
    statements
}

/// 一条 `pub use` 语句对外可见的名字：花括号里的每一项 + `as` 改名后的末段。
fn reexported_names(statement: &str) -> Vec<String> {
    let body = statement.trim().trim_end_matches(';').to_string();
    let pieces: Vec<String> = if let Some(open) = body.find('{') {
        let close = body.rfind('}').unwrap_or(body.len());
        body[open + 1..close]
            .split(',')
            .map(|piece| piece.trim().to_string())
            .collect()
    } else {
        let tail = body["pub use ".len()..].trim();
        if tail.ends_with("::*") {
            return Vec::new();
        }
        vec![tail.split("::").last().unwrap_or_default().to_string()]
    };
    pieces
        .into_iter()
        .filter(|piece| !piece.is_empty() && piece != "self")
        .map(|piece| {
            if let Some((_, alias)) = piece.split_once(" as ") {
                return ident_chars(alias.trim());
            }
            ident_chars(piece.split("::").last().unwrap_or_default())
        })
        .filter(|name| !name.is_empty())
        .collect()
}

/// #206：门面再导出的名字若整棵树只有"定义行 + 再导出行"，它就是零读者的死公共面。
#[test]
fn reexported_public_names_have_a_reader_beyond_their_own_reexport() {
    let files = all_rust_files();
    let mut texts: BTreeMap<PathBuf, Vec<String>> = BTreeMap::new();
    let mut export_lines: BTreeMap<PathBuf, BTreeSet<usize>> = BTreeMap::new();
    let mut reexported: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in &files {
        let source = std::fs::read_to_string(path).expect("crate 源码应可读");
        let lines = source.lines().map(str::to_string).collect::<Vec<_>>();
        for (start, covered, statement) in pub_use_statements(&lines) {
            let relative = display_relative(path);
            export_lines
                .entry(path.clone())
                .and_modify(|set| {
                    set.extend(covered.iter().copied());
                })
                .or_insert(covered);
            for name in reexported_names(&statement) {
                reexported
                    .entry(name)
                    .or_default()
                    .push(format!("{relative}:{start}"));
            }
        }
        texts.insert(path.clone(), lines);
    }
    // 解析器空转的自守：整棵树里至少认得出这么多再导出名字，否则"零候选"只是没取到数。
    assert!(
        reexported.len() > 80,
        "只解析出 {} 个再导出名字，取数口径本身要重新核对（#206 的判据面不能空转）",
        reexported.len()
    );

    let mut silent = Vec::new();
    for (name, sites) in &reexported {
        let mut readers = 0;
        for (path, lines) in &texts {
            let skip = export_lines.get(path);
            for (index, line) in lines.iter().enumerate() {
                if !mentions(line, name) {
                    continue;
                }
                let line_no = index + 1;
                if skip.is_some_and(|covered| covered.contains(&line_no)) {
                    continue;
                }
                // 再导出行的豁免只有上面这一处：`pub_use_statements` 把一条语句占的每一行都记进
                // `covered`，所以在这里再判一遍 `starts_with("pub use ")` 是永远走不到的分支。
                let stripped = line.trim_start();
                if stripped.starts_with("use ")
                    || stripped.starts_with("//")
                    || defines_line(line, name)
                {
                    continue;
                }
                readers += 1;
            }
        }
        if readers == 0 {
            silent.push(format!("{name} 再导出 {}", sites.join(", ")));
        }
    }
    assert!(
        silent.is_empty(),
        "#206：下面这些公共面只被门面的 `pub use` 行搬到 crate 外，整棵树里没有任何读者。\
         门禁把再导出行算成读者，所以它们恒绿；要么接上真读者，要么整块删掉并写进 CHANGELOG：\n{}",
        silent.join("\n")
    );
}

/// 接线判据：本轮按零读者删掉的九个符号与两份常量不许以任何形态回来。
///
/// 它们曾长期"装活"（`RebalanceDelta` 与 `Allocator`/`EqualWeight` 的唯一读者是同一文件里
/// 给它自己写的用例；`ShareSubscription` 零构造点；两条 Binance 端点预设包装与夹在中间的
/// `run_binance_user_stream_with_config` 委托没有任何调用点）。
///
/// 刻意不按"整棵树里不许出现这个名字"断言：本文件自己就得写出这批名字，那条口径会让判据与
/// 自己的名单打架（第一版就是这么红的）。这里只认真正算复活的两种形态——定义行回来了、或
/// 被某条 `pub use` 重新搬上门面。
#[test]
fn names_deleted_for_zero_readers_stay_off_the_surface() {
    const DELETED: [&str; 11] = [
        "build_rebalance",
        "RebalanceDelta",
        "Allocator",
        "EqualWeight",
        "pipeline_path",
        "ShareSubscription",
        "run_binance_user_stream_live",
        "run_binance_user_stream_testnet",
        "run_binance_user_stream_with_config",
        "DEFAULT_WS_HOST",
        "TESTNET_WS_HOST",
    ];
    let files = all_rust_files();
    let mut on_surface: BTreeSet<String> = BTreeSet::new();
    let mut definitions: BTreeMap<String, Vec<String>> = BTreeMap::new();
    for path in &files {
        let source = std::fs::read_to_string(path).expect("crate 源码应可读");
        let lines = source.lines().map(str::to_string).collect::<Vec<_>>();
        let relative = display_relative(path);
        for (start, _, statement) in pub_use_statements(&lines) {
            for name in reexported_names(&statement) {
                if DELETED.contains(&name.as_str()) {
                    on_surface.insert(format!("{name} ({relative}:{start})"));
                }
            }
        }
        for (index, line) in lines.iter().enumerate() {
            for name in DELETED {
                if defines_line(line, name) {
                    definitions
                        .entry(name.to_string())
                        .or_default()
                        .push(format!("{relative}:{}", index + 1));
                }
            }
        }
    }
    assert!(
        definitions.is_empty(),
        "#206 删掉的名字又有了定义点：{definitions:?}"
    );
    assert!(
        on_surface.is_empty(),
        "#206 删掉的名字又回到某条 `pub use` 的门面上：{:?}",
        on_surface.into_iter().collect::<Vec<_>>()
    );
    // `quantity_delta` 是 `RebalanceDelta` 的字段：定义行判据抓不到"结构体没了、字段名留下"，
    // 字段级取证是 #174 那条缺口的活，本轮先在人工探针里收（logs/s261 的残留自检）。
    let ledger = workspace_source("crates/qx-core/src/ledger/mod.rs");
    assert!(
        !ledger.lines().any(|line| mentions(line, "quantity_delta")),
        "调仓增量的字段名回来了：再平衡的表示是 `RebalancePlan` 的 target_qty"
    );
}
