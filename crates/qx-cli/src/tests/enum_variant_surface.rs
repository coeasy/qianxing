//! 全仓枚举变体的**可见面**清点：门禁的单行扫描看不见的那些，必须由这条判据兜住
//! （V13 R2 第二十三遍 #243，接住第二十二遍立案的 #241）。
//!
//! 门禁 `enum_variant_producer_check`（`tools/check_architecture.py`）取变体的方式是逐行匹配
//! `Name,`，且枚举体内遇到第一个 `}` 行就退出。本遍（第二十三遍）实测：按花括号配平能解析出 390 颗
//! `pub enum` 变体，门禁只认到 248 颗 —— 142 颗（含 `EventKind` 整本 14 颗）从来没有被判据看过一眼。
//! `EventKind::Timer`/`MarketBar` 正是靠这个盲区藏了 21 遍：三处下游臂认它们，全仓却没有任何一处
//! 能把它们造出来（#239 删的就是这两颗）。
//!
//! 本文件把盲区里的变体变成判据，口径与门禁一致（同一份取数语料：`crates/*/src/**/*.rs`，
//! 排除 `tests` 目录，剥掉 `#[cfg(test)]` 整项与注释行），只做两件门禁没做的事：
//! 1. 变体集合按花括号配平枚举，跨行/带字段/带元组的形状都算；
//! 2. 对"配平看得见、单行看不见"的那一批，要求每一颗都有生产限定名引用
//!    （`Enum::Variant`，或 `impl Enum` 块体内的 `Self::Variant`）—— 零引用又不许进允许清单的
//!    就是下一颗 `EventKind::Timer`。
//!
//! 覆盖面地板与"四本整盲的枚举各自至少贡献一颗"是防空转的：扫描退化时这里先红，而不是让
//! 第 2 条对着空集合自证。

use super::*;
use std::collections::{BTreeMap, BTreeSet};

/// 本轮实测的取数地板：低于这些数字说明扫描退化（口径漂了、语料少了），判据本身先红。
/// 三个数字都来自本用例的打印行（`s566_enum_surface_run1.txt`）：枚举 77 本、变体 390 颗、盲点 142 颗。
const VARIANT_SURFACE_FLOOR: usize = 390;
const ENUM_SURFACE_FLOOR: usize = 77;
const BLIND_VARIANT_FLOOR: usize = 142;
/// 门禁的单行扫描**整本看不见**的四本枚举：它们贡献的盲变体数必须 >0，
/// 否则说明这里的配平口径与门禁的差异被写没了，第 2 条判据会跟着失去对象。
const WHOLLY_BLIND_ENUMS: [&str; 4] = [
    "EventKind",
    "RuntimeExternalEvent",
    "StorageError",
    "QxError",
];

/// 允许清单：配平看得见、生产限定名数不到，却有正当生产者的变体。
/// 本清单为空 —— 本轮实测「盲且零引用」为 0 颗。新增条目必须写清理由，且理由要能被独立复核
/// （线格式生产者要能在同一份源码里数到 `Deserialize`），否则就是给下一颗孤儿开门。
const BLIND_WITHOUT_PRODUCTION_REF: [(&str, &str); 0] = [];

/// 生产语料：与门禁同一口径的文本（剥 `#[cfg(test)]` 整项、丢注释行）。
fn production_blob(path: &Path) -> String {
    let raw = std::fs::read_to_string(path)
        .unwrap_or_else(|error| panic!("读取 {} 失败: {error}", path.display()));
    let text = raw.replace("\r\n", "\n");
    let mut kept = String::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut index = 0_usize;
    while index < lines.len() {
        let trimmed = lines[index].trim();
        if trimmed == "#[cfg(test)]" {
            index += 1;
            while index < lines.len() && !lines[index].contains('{') && !lines[index].contains(';')
            {
                index += 1;
            }
            let mut depth = 0_i32;
            while index < lines.len() {
                depth += lines[index].matches('{').count() as i32
                    - lines[index].matches('}').count() as i32;
                index += 1;
                if depth <= 0 {
                    break;
                }
            }
            continue;
        }
        if !trimmed.starts_with("//") {
            kept.push_str(lines[index]);
            kept.push('\n');
        }
        index += 1;
    }
    kept
}

fn identifier_prefix(token: &str) -> String {
    token
        .chars()
        .take_while(|c| c.is_alphanumeric() || *c == '_')
        .collect()
}

/// 一行是否是变体声明的开头：大写标识符 + `,`/`{`/`(`/`=`/行尾。
fn variant_at_line_start(trimmed: &str) -> Option<String> {
    let name = identifier_prefix(trimmed);
    if name.is_empty() || !name.starts_with(|c: char| c.is_uppercase()) {
        return None;
    }
    let rest = trimmed[name.len()..].trim_start();
    if rest.is_empty()
        || rest.starts_with(',')
        || rest.starts_with('{')
        || rest.starts_with('(')
        || rest.starts_with('=')
    {
        return Some(name);
    }
    None
}

/// 从 `text[at..]` 的第一个 `{` 起做花括号配平，返回块体（不含外层花括号）。
fn balanced_block(text: &str, at: usize) -> Option<&str> {
    let open = text[at..].find('{')? + at;
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

/// 全仓生产语料的文本：`all_crate_production_sources` 已经跳过了用例目录，
/// 所以本文件自身的允许清单字面量不会进语料，判据不会自证。
fn production_corpus() -> Vec<String> {
    all_crate_production_sources()
        .iter()
        .map(|path| production_blob(path))
        .collect()
}

/// 一个 `pub enum` 的两套变体集合：花括号配平的（全）与门禁单行扫描的（可见子集）。
fn enum_variant_faces(text: &str) -> Vec<(String, BTreeSet<String>, BTreeSet<String>)> {
    let mut faces = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    let mut offset = 0_usize;
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("pub enum ") {
            let enum_name = identifier_prefix(rest.trim_start());
            if !enum_name.is_empty() {
                // 配平口径：枚举体整块取出，跨行的带字段/带元组形状都算；顶层变体按深度 0 认。
                let mut balanced = BTreeSet::new();
                if let Some(body) = balanced_block(text, offset) {
                    let mut depth = 0_i32;
                    for body_line in body.lines() {
                        if depth == 0 {
                            if let Some(name) = variant_at_line_start(body_line.trim()) {
                                balanced.insert(name);
                            }
                        }
                        depth += body_line.matches('{').count() as i32
                            - body_line.matches('}').count() as i32;
                    }
                }
                // 门禁口径：逐行 `Name,`，遇到第一个 `}` 行即退出。
                let mut visible = BTreeSet::new();
                for body_line in lines[index + 1..].iter() {
                    if body_line.trim_start().starts_with('}') {
                        break;
                    }
                    let token = body_line.trim();
                    let name = identifier_prefix(token);
                    if !name.is_empty()
                        && name.starts_with(|c: char| c.is_uppercase())
                        && matches!(token[name.len()..].trim(), "" | ",")
                    {
                        visible.insert(name);
                    }
                }
                faces.push((enum_name, balanced, visible));
            }
        }
        offset += line.len() + 1;
    }
    faces
}

/// `impl Enum` / `impl Trait for Enum` 的块体（要求块头的 `{` 写在同一行，与 rustfmt 一致）。
fn impl_block_bodies(text: &str, enum_name: &str) -> Vec<String> {
    let mut bodies = Vec::new();
    let lines: Vec<&str> = text.lines().collect();
    for (index, line) in lines.iter().enumerate() {
        let trimmed = line.trim_start();
        let Some(after_impl) = trimmed.strip_prefix("impl") else {
            continue;
        };
        if !line.trim_end().ends_with('{') {
            continue;
        }
        let head = after_impl.trim_end().trim_end_matches('{').trim();
        let tail = match head.strip_suffix('>') {
            Some(at_arrow) => {
                let open = at_arrow.rfind('<').unwrap_or(0);
                at_arrow[..open].trim()
            }
            None => head,
        };
        if !tail.ends_with(enum_name) {
            continue;
        }
        let mut depth = 0_i32;
        let mut body = String::new();
        for block_line in &lines[index..] {
            let before = depth;
            depth +=
                block_line.matches('{').count() as i32 - block_line.matches('}').count() as i32;
            if before >= 1 {
                body.push_str(block_line);
                body.push('\n');
            }
            if before >= 1 && depth <= 0 {
                break;
            }
        }
        bodies.push(body);
    }
    bodies
}

/// 在 `text` 里找 `needle` 的全部起点，两端都不许接标识符字符。
fn bounded_hits(text: &str, needle: &str) -> usize {
    let is_ident = |c: Option<char>| c.is_some_and(|c| c.is_alphanumeric() || c == '_');
    let mut hits = 0_usize;
    let mut cursor = 0_usize;
    while let Some(found) = text[cursor..].find(needle) {
        let at = cursor + found;
        let before = text[..at].chars().next_back();
        let after = text[at + needle.len()..].chars().next();
        if !is_ident(before) && !is_ident(after) {
            hits += 1;
        }
        cursor = at + needle.len();
    }
    hits
}

/// 全仓生产语料里的变体清点结果：`(枚举, 全量变体, 门禁可见变体)` 按文件展开。
fn workspace_variant_faces() -> Vec<(String, BTreeSet<String>, BTreeSet<String>)> {
    let mut all = Vec::new();
    for path in all_crate_production_sources() {
        all.extend(enum_variant_faces(&production_blob(&path)));
    }
    all
}

/// 第 2 条判据：门禁看不见的每一颗变体都要有生产限定名引用。
#[test]
fn variants_the_gate_cannot_see_still_have_production_producers() {
    let blobs = production_corpus();
    let mut scope_cache: BTreeMap<String, String> = BTreeMap::new();
    let mut self_scope = |enum_name: &str| -> String {
        if let Some(cached) = scope_cache.get(enum_name) {
            return cached.clone();
        }
        let joined = blobs
            .iter()
            .filter(|blob| blob.contains("impl "))
            .map(|blob| impl_block_bodies(blob, enum_name).join("\n"))
            .collect::<Vec<_>>()
            .join("\n");
        scope_cache.insert(enum_name.to_string(), joined.clone());
        joined
    };
    let mut blind_total = 0_usize;
    let mut variants_total = 0_usize;
    let mut enums_total = BTreeSet::new();
    let mut blind_by_enum: BTreeSet<String> = BTreeSet::new();
    let mut offenders: Vec<String> = Vec::new();
    for (enum_name, balanced, visible) in workspace_variant_faces() {
        if balanced.is_empty() {
            continue;
        }
        enums_total.insert(enum_name.clone());
        variants_total += balanced.len();
        let mut blind = Vec::new();
        for variant in balanced.difference(&visible) {
            blind.push(variant.clone());
        }
        if blind.is_empty() {
            continue;
        }
        let scope = self_scope(&enum_name);
        for variant in blind {
            blind_total += 1;
            blind_by_enum.insert(enum_name.clone());
            let qualified = blobs
                .iter()
                .map(|blob| bounded_hits(blob, &format!("{enum_name}::{variant}")))
                .sum::<usize>();
            let shorthand = bounded_hits(&scope, &format!("Self::{variant}"));
            let allowed = BLIND_WITHOUT_PRODUCTION_REF
                .iter()
                .any(|(name, v)| *name == enum_name.as_str() && *v == variant.as_str());
            if qualified + shorthand == 0 && !allowed {
                offenders.push(format!("{enum_name}::{variant}"));
            }
        }
    }
    println!(
        "枚举={} 变体全量={} 门禁盲点={} 盲且零引用={}",
        enums_total.len(),
        variants_total,
        blind_total,
        offenders.len()
    );
    assert!(
        variants_total >= VARIANT_SURFACE_FLOOR,
        "配平扫描只数出 {variants_total} 颗变体，低于本轮实测的 {VARIANT_SURFACE_FLOOR} 颗：\
         取数口径先修，否则下面的判据是空转"
    );
    assert!(
        enums_total.len() >= ENUM_SURFACE_FLOOR,
        "配平扫描只数出 {} 本 pub enum，低于地板 {ENUM_SURFACE_FLOOR} 本：语料或口径漂了",
        enums_total.len()
    );
    assert!(
        blind_total >= BLIND_VARIANT_FLOOR,
        "本轮实测门禁的盲点是 {blind_total} 颗（地板 {BLIND_VARIANT_FLOOR}）：如果盲点变小，\
         说明门禁升级了扫描口径，本判据的分工要重新对表而不是静默空转"
    );
    for wholly_blind in WHOLLY_BLIND_ENUMS {
        assert!(
            blind_by_enum.contains(wholly_blind),
            "{wholly_blind} 不再向盲点集合贡献任何变体：配平口径与门禁口径的差异在这一本上\
             消失了，本判据的对照物少了一整本词汇表"
        );
    }
    assert!(
        offenders.is_empty(),
        "这些变体既在门禁的盲点里、又在生产里没有任何限定名引用（构造不出来也无人认）：\
         {offenders:?}。第二十二遍的 EventKind::Timer 就是这个形状藏了 21 遍。"
    );
}

/// 允许清单不许腐坏：条目要么真的还是零引用，要么就该被删掉（防止清单变成常青豁免）。
#[test]
fn the_blind_allowlist_is_not_stale() {
    let blobs = production_corpus();
    for (enum_name, variant) in BLIND_WITHOUT_PRODUCTION_REF {
        let hits = blobs
            .iter()
            .map(|blob| bounded_hits(blob, &format!("{enum_name}::{variant}")))
            .sum::<usize>();
        assert_eq!(
            hits, 0,
            "允许清单里的 {enum_name}::{variant} 现在有了 {hits} 处生产限定名引用：\
             它不再需要豁免，请把条目删掉"
        );
    }
}
