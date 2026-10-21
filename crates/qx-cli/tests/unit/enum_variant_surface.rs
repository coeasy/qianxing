//! 全仓 `pub enum` 变体的生产引用检查。按 Rust 花括号配平读取跨行、带字段和元组变体，
//! 对生产源码中的 `Enum::Variant` 与 `impl Enum` 内的 `Self::Variant` 引用进行核对；
//! 不依赖固定枚举数量或旧扫描器的盲区规模。

use super::*;
use std::collections::{BTreeMap, BTreeSet};

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

/// 覆盖通用扫描器未识别的变体形状，避免未被主检查器覆盖的孤儿变体漏过。
#[test]
fn unrecognized_variant_shapes_still_have_production_producers() {
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
    let mut variants_total = 0_usize;
    let mut enums_total = BTreeSet::new();
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
        "枚举={} 变体全量={} 未解析生产引用={}",
        enums_total.len(),
        variants_total,
        offenders.len()
    );
    assert!(!enums_total.is_empty(), "源码扫描没有发现任何 pub enum");
    assert!(variants_total > 0, "源码扫描没有发现任何 pub enum 变体");
    assert!(
        offenders.is_empty(),
        "这些变体没有生产限定名引用（构造不出来也无人认）：{offenders:?}"
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
