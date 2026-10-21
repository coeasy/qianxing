//! #141 的第四条缺口：变体生产者判据**看不见结构体式变体**，本轮实测到 EventKind 整本词汇表。
//!
//! 门禁 `enum_variant_producer_check` 的变体扫描是单行的：一行 `Name,` 才算变体，而枚举体内
//! 遇到第一个 `^\s*}` 就停。`crates/qx-core/src/event.rs` 里 `EventKind` 的 14 颗变体有 13 颗是
//! `Name {` 跨行形状，第一颗 `MarketQuote {` 的字段闭合 `},` 就把扫描赶出了这个枚举 —— 探针实测
//! 门禁对 EventKind 数出的变体集合是空的（`logs/s521_pass22_probe_before.txt`）。同一把尺子在全仓
//! 的读数：按花括号配平能解析出 383 颗变体，门禁的单行扫描只认到 248 颗，135 颗不在判据里。
//!
//! 于是 `EventKind::Timer` 与 `EventKind::MarketBar` 长期挂着三处下游臂（摘要、API 投影、恢复
//! 忽略），却在整个仓库里没有任何一处能把它们造出来 —— 只有 `#[cfg(test)]` 内联用例在写。日志
//! 词汇表里有一颗永远写不出的事实种类，读代码的人会以为"bar 可以从事件日志重放"，而实际不会。
//! 本轮删掉这两颗，并把这本词汇表变成机器判据：变体集合按花括号配平枚举，每颗都要有生产构造点，
//! 摘要标签不得重号也不得复用退役的那两个。

use super::*;
use std::collections::{BTreeMap, BTreeSet};

const EVENT_KIND_SOURCE: &str = "crates/qx-core/src/event.rs";

/// 本轮退役的两颗：全仓不许再出现任何一处引用（残留臂是"编译通过但永不命中"的孤儿）。
const RETIRED_KINDS: [&str; 2] = ["Timer", "MarketBar"];
/// 摘要里退役的两个标签位不许被复用：别的版本写出的日志会被读成另一种事实，且没有任何一处会报错。
const RETIRED_DIGEST_TAGS: [&str; 2] = ["1", "2"];

/// 按花括号配平枚举 `pub enum EventKind { … }` 的顶层变体，跨行与带字段的形状都算。
fn declared_variants() -> Vec<String> {
    let source = workspace_source(EVENT_KIND_SOURCE);
    let header = source
        .find("pub enum EventKind {")
        .expect("qx-core 必须声明 EventKind");
    let body = balanced_block(&source, header + "pub enum EventKind".len())
        .expect("EventKind 的枚举体没有闭合");
    let mut variants = Vec::new();
    for line in body.lines() {
        if !line.starts_with("    ") || line[4..].starts_with(' ') {
            continue; // 4 空格是顶层变体；更深缩进是字段行
        }
        let token = line.trim();
        if token.starts_with('#') || token.starts_with('/') {
            continue;
        }
        let name: String = token
            .chars()
            .take_while(|c| c.is_alphanumeric() || *c == '_')
            .collect();
        if name.is_empty() || !name.starts_with(|c: char| c.is_uppercase()) {
            continue;
        }
        assert!(
            variants.iter().all(|existing| existing != &name),
            "EventKind 里 {name} 出现了两次，判据的集合口径会失效"
        );
        variants.push(name);
    }
    variants
}

/// 从 `open_at` 之后第一个 `{` 起做花括号配平，返回块体（不含外层花括号）。
fn balanced_block(text: &str, open_at: usize) -> Option<&str> {
    let open = text[open_at..].find('{')? + open_at;
    let mut depth = 0_usize;
    for (offset, char) in text[open..].char_indices() {
        match char {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(&text[open + 1..open + offset]);
                }
            }
            _ => {}
        }
    }
    None
}

/// 同一份源码，切掉尾部 `#[cfg(test)] mod tests {` 之后的全部文本。
///
/// 按行找 `mod tests {`，且它上一行必须是 `#[cfg(test)]`：本仓的内联用例都是这个形状。
fn production_text(path: &Path) -> String {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("读取 {} 失败: {error}", path.display()));
    let text = raw.replace("\r\n", "\n");
    for (offset, _) in text.match_indices("\nmod tests {") {
        let previous = text[..offset]
            .lines()
            .rev()
            .find(|line| !line.trim().is_empty())
            .unwrap_or_default()
            .trim()
            .to_string();
        if previous == "#[cfg(test)]" {
            return text[..offset].to_string();
        }
    }
    text
}

/// 在 `text` 里找 `needle` 的全部起点，且要求两端都不接标识符字符。
///
/// 边界是必需的而不是讲究：`crates/qx-strategy/src/c_api.rs` 里的 C-ABI 枚举写作
/// `QxMarketEventKind::Timer`，裸 `contains` 会把这颗策略层的活变体读成已删除的
/// `EventKind::Timer` 的残留引用（本轮第二发变异实测到的正是这个假红）。
fn bounded_occurrences(text: &str, needle: &str) -> Vec<usize> {
    let is_ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut hits = Vec::new();
    let mut cursor = 0_usize;
    while let Some(found) = text[cursor..].find(needle) {
        let at = cursor + found;
        let before = text[..at].chars().next_back();
        let after = text[at + needle.len()..].chars().next();
        if !is_ident(before) && !is_ident(after) {
            hits.push(at);
        }
        cursor = at + needle.len();
    }
    hits
}

/// 生产源码里 `EventKind::<V>` 的**构造点**计数（按变体名）。
///
/// 只数构造：`src/tests/**` 目录与文件尾部的 `#[cfg(test)] mod tests` 都不算生产者（用例读者是
/// 回归证明，不是词汇表的入口），匹配臂也不算 —— 一条永不命中的 `| EventKind::X { .. }` 正是本轮
/// 要抓的形状。判定式见 [`site_is_construction`]。
fn production_constructors(variants: &[String]) -> BTreeMap<String, usize> {
    let mut ledger = BTreeMap::new();
    for path in all_crate_production_sources() {
        let text = production_text(&path);
        for variant in variants {
            let needle = format!("EventKind::{variant}");
            for at in bounded_occurrences(&text, &needle) {
                if site_is_construction(&text, at, at + needle.len()) {
                    *ledger.entry(variant.clone()).or_insert(0) += 1;
                }
            }
        }
    }
    ledger
}

/// 这一处 `EventKind::V` 是构造还是模式匹配。
///
/// 认四种模式形状：或模式臂（行首是 `|`）、体内带 `..` 的省略模式、闭合花括号之后紧跟 `=>`
/// 的臂、体内每一行都只是裸字段名的解构（构造点必须给出键值或字面量）。注释行两边都不算。
fn site_is_construction(text: &str, at: usize, name_end: usize) -> bool {
    let line_start = text[..at].rfind('\n').map_or(0, |index| index + 1);
    let line_end = text[at..]
        .find('\n')
        .map_or(text.len(), |offset| at + offset);
    let line = text[line_start..line_end].trim_start();
    if line.starts_with("//") || line.starts_with('*') || line.starts_with('|') {
        return false;
    }
    let rest = text[name_end..].trim_start();
    if rest.starts_with('{') {
        let open = name_end + text[name_end..].find('{').expect("上面已确认以 { 开头");
        let body = balanced_block(text, open).unwrap_or_default();
        if body.contains("..") || text[open + body.len() + 1..].trim_start().starts_with("=>") {
            return false;
        }
        let fields = body
            .lines()
            .map(str::trim)
            .filter(|field| !field.is_empty())
            .collect::<Vec<_>>();
        let bare_fields = !fields.is_empty()
            && fields.iter().all(|field| {
                field
                    .trim_end_matches(',')
                    .chars()
                    .all(|c| c.is_lowercase() || c.is_ascii_digit() || c == '_')
            });
        return !bare_fields;
    }
    !(rest.starts_with("=>") || rest.starts_with('.') || rest.starts_with("::"))
}

#[test]
fn event_kind_variants_are_enumerated_beyond_the_first_struct_body() {
    let variants = declared_variants();
    // 门禁的单行扫描在这里数出 0 颗：这条判据先自证它走到了枚举体的末尾。
    assert!(
        variants.len() >= 14,
        "EventKind 只解析出 {} 颗变体，少于本轮实测的 14 颗：{}",
        variants.len(),
        variants.join(",")
    );
    assert!(
        variants.last().is_some_and(|last| last == "Settle"),
        "解析在第一个结构体式变体处就断了（末颗是 {:?} 而不是 Settle）——这正是门禁那把尺子的失效形状",
        variants.last()
    );
    for retired in RETIRED_KINDS {
        assert!(
            !variants.iter().any(|name| name == retired),
            "{retired} 是本轮删掉的零构造变体，又回到了词汇表里"
        );
    }
}

#[test]
fn every_event_kind_variant_has_a_production_constructor() {
    let variants = declared_variants();
    let ledger = production_constructors(&variants);
    let total = ledger.values().sum::<usize>();
    assert!(
        total >= 29,
        "全仓只数出 {total} 处 EventKind 构造点，比本轮实测的 29 处少：判定式需要先修，否则这条判据空转\
         （台账 {ledger:?}）"
    );
    let missing = variants
        .into_iter()
        .filter(|variant| !ledger.contains_key(variant))
        .collect::<Vec<_>>();
    assert!(
        missing.is_empty(),
        "EventKind 里这些种类在生产代码里没有任何构造点：{missing:?}。日志词汇表里挂着一颗写不出的事实，\
         就等于对外承诺了「这种事实可以从日志重放」——本轮删掉的 Timer/MarketBar 正是这个形状（摘要、API \
         投影、恢复忽略三处臂都在，生产者只剩 #[cfg(test)] 用例）。要么接上生产者，要么删掉变体并一起收掉\
         三处臂；本判据不设豁免名单，因为线格式生产者在这里能被构造点直接证伪。"
    );
}

#[test]
fn retired_orphan_kinds_leave_no_dangling_arm() {
    for retired in RETIRED_KINDS {
        let needle = format!("EventKind::{retired}");
        let mut hits = Vec::new();
        for path in all_crate_production_sources() {
            let raw = std::fs::read_to_string(&path).unwrap_or_default();
            let text = raw.replace("\r\n", "\n");
            // 只扫生产源码（`all_crate_production_sources` 已跳过任何名为 tests 的目录）：
            // 本判据自己的立案文档就要写下这两颗名字，扫到用例面就会把自己判红。
            // 用例里重新引用一颗已删的变体编不过，`cargo check --all-targets` 那一侧守着。
            if !bounded_occurrences(&text, &needle).is_empty() {
                hits.push(path.display().to_string());
            }
        }
        assert!(
            hits.is_empty(),
            "{needle} 又出现在 {hits:?}：变体已删，这些引用要么是编译不过的死文本，要么是重新加回的孤儿。\
             这里按标识符边界扫，所以 `QxMarketEventKind::{retired}` 那种策略层的活类型不会被误算。"
        );
    }
}

#[test]
fn event_kind_digest_tags_are_unique_and_never_reuse_retired_slots() {
    let source = workspace_source(EVENT_KIND_SOURCE);
    let digest_at = source
        .find("pub fn digest(")
        .expect("Event 必须有 digest 摘要函数");
    let body = &source[digest_at..];
    let mut tags = BTreeSet::new();
    let mut seen = Vec::new();
    for variant in declared_variants() {
        let needle = format!("EventKind::{variant}");
        let found = body
            .find(&needle)
            .unwrap_or_else(|| panic!("{needle} 在摘要函数体里没有了：每颗变体都要有一条摘要臂"));
        let arm = &body[found..];
        let next_arm = arm[needle.len()..]
            .find("EventKind::")
            .map_or(arm.len(), |offset| needle.len() + offset);
        let head = arm[..next_arm]
            .split_once("=>")
            .map(|(_, after)| after)
            .unwrap_or_else(|| panic!("{needle} 没有 `=>` 摘要臂"));
        let value = first_write_tag(head).unwrap_or_else(|| {
            panic!("{needle} 的摘要臂开头没有 `h.write_u64(<整数>)`：摘要不再区分事实种类")
        });
        assert!(
            tags.insert(value.clone()),
            "摘要标签 {value} 被 {needle} 与更早的某条臂同时使用：两种事实哈希成同一个码"
        );
        assert!(
            !RETIRED_DIGEST_TAGS.contains(&value.as_str()),
            "{needle} 占用了退役标签位 {value}（原本属于 {} 这两颗）：旧日志里同名的标签会被读成另一种事实",
            RETIRED_KINDS.join("/")
        );
        seen.push(format!("{variant}={value}"));
    }
    assert!(
        tags.len() >= 14,
        "摘要标签只数出 {} 个：{}",
        tags.len(),
        seen.join(",")
    );
}

/// 取一段文本里第一个 `h.write_u64(` 后面那串数字；不是整数开头时返回 `None`。
fn first_write_tag(text: &str) -> Option<String> {
    let at = text.find("h.write_u64(")? + "h.write_u64(".len();
    let digits = text[at..]
        .chars()
        .take_while(|c| c.is_ascii_digit())
        .collect::<String>();
    (!digits.is_empty()).then_some(digits)
}
