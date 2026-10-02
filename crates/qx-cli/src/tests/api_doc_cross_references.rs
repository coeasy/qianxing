//! 接口文档里按「」点名的"见上/见下"指代，方向必须与目标的相对位置一致（V13 R2 第九遍 #184）。
//!
//! 立案过程就是本文件要防的那件事：第八遍把第二张端点表从 `## Outbox 与 NATS JetStream` 搬进
//! `## Paper API`（#183）之后，那张表里 `/metrics` 那一格写的还是「见上『指标出口是逐行的』」，
//! 而那个小节此时在表的**下面**。#183 的判据只管"三样东西住在同一章内、相对次序不变"，
//! 管不到章内一次搬家会把方向词翻过来；按整篇文档取数的路由判据与字段判据也看不见这句话。
//!
//! 所以这里只核**方向词与目标标题的相对位置**：文档里每一处 `见上「X」` / `见下「X」`（方向词与
//! 「」之间允许夹"文/面/中"或换行，散文会折行）都按标题前缀找到 `## X…` 那一行，要求 X 在
//! 声明的方向上真的存在，而且不能两侧都存在（两侧都有同名标题时方向词无法告诉读者翻哪边）。
//! 目标不存在 = 悬空指代，也红。
//!
//! 口径边界（如实写出，不假装覆盖）：只核**带「」点名**的指代。"见下段""名单见下"这类没有点名的
//! 说法不在本判据里，因此循环之前有一条**条数地板**（按第九遍定稿时实测的 6 处），防止扫描口径一旦
//! 失灵就让整条判据退化成空转（#179 的"字面量计数是单向证据"同一条纪律）。地板是棘轮：文档里新增
//! 一处点名指代时应同时抬上来，删一处则必须红。

use super::*;

/// 文档里的标题：`(起始字节, 标题文字)`，只认行首的 `##`/`###`/`####`。
fn heading_positions(doc: &str) -> Vec<(usize, String)> {
    let mut starts_at = 0usize;
    let mut out = Vec::new();
    for line in doc.split_inclusive('\n') {
        let text = line.trim_end_matches(['\n', '\r']);
        let title = text
            .strip_prefix("#### ")
            .or_else(|| text.strip_prefix("### "))
            .or_else(|| text.strip_prefix("## "));
        if let Some(title) = title {
            out.push((starts_at, title.to_string()));
        }
        starts_at += line.len();
    }
    out
}

/// 全部 `见上「X」` / `见下「X」` 形态的指代：`(方向词的字节位置, 方向词, 点名的目标)`。
///
/// 方向词与 `「` 之间允许最多三个 `文`/`面`/`中`/空格/换行（"见上文「…」"、以及折行的"见上\n「…」"），
/// 再多的就不再算点名式指代，交给散文里那些"见下""如下"。
fn named_direction_refs(doc: &str) -> Vec<(usize, &'static str, String)> {
    let mut out = Vec::new();
    for word in ["见上", "见下"] {
        for (pos, _) in doc.match_indices(word) {
            let mut cursor = pos + word.len();
            let mut skipped = 0;
            while skipped < 3 {
                let hit = ['文', '面', '中', ' ', '\n', '\r']
                    .into_iter()
                    .find(|ch| doc[cursor..].starts_with(*ch));
                let Some(ch) = hit else { break };
                cursor += ch.len_utf8();
                skipped += 1;
            }
            let Some(after_open) = doc[cursor..].strip_prefix('「') else {
                continue;
            };
            let Some(close) = after_open.find('」') else {
                continue;
            };
            let target = after_open[..close].trim().to_string();
            if !target.is_empty() {
                out.push((pos, word, target));
            }
        }
    }
    out.sort();
    out
}

#[test]
fn directional_cross_references_point_the_right_side() {
    let raw = workspace_source("deploy/README.md");
    let doc = if raw.contains("\r\n") {
        raw.replace("\r\n", "\n")
    } else {
        raw.clone()
    };
    let headings = heading_positions(&doc);
    let refs = named_direction_refs(&doc);
    assert!(
        refs.len() >= 6,
        "只扫到 {} 处点名式方向指代，本判据的扫描口径已经与文档脱节（第九遍定稿时有 6 处）。\
         扫描失灵时这条判据会一路绿下去，所以先让它在失灵时红。",
        refs.len()
    );

    for (at, word, target) in &refs {
        let named: Vec<(usize, &str)> = headings
            .iter()
            .filter(|(_, title)| title.starts_with(target.as_str()))
            .map(|(start, title)| (*start, title.as_str()))
            .collect();
        assert!(
            !named.is_empty(),
            "「{word}「{target}」」点名了一个文档里不存在的小节（标题需以 {target:?} 开头）：\
             读者按那句话翻过去会落空"
        );
        let before = named.iter().filter(|(start, _)| start < at).count();
        let after = named.len() - before;
        assert!(
            !(before > 0 && after > 0),
            "「{word}「{target}」」有歧义：同名标题在这一处指代的上下各有一份（上 {before} / 下 {after}），\
             方向词无法告诉读者翻哪边"
        );
        if *word == "见上" {
            let nearest = named[before.saturating_sub(1)].0;
            assert!(
                before > 0,
                "「{word}「{target}」」写的朝上，但被点名的标题在这一处指代的下面：\
                 指代位置第 {at} 字节，标题在第 {nearest} 字节。\
                 搬小节就会翻错方向——#184 立案时端点表里那条 `/metrics` 正是这样坏的。"
            );
        } else {
            let nearest = named[if before < named.len() {
                before
            } else {
                named.len() - 1
            }]
            .0;
            assert!(
                after > 0,
                "「{word}「{target}」」写的朝下，但被点名的标题在这一处指代的上面：\
                 指代位置第 {at} 字节，标题在第 {nearest} 字节。\
                 搬小节就会翻错方向——#184 立案时端点表里那条 `/metrics` 正是这样坏的。"
            );
        }
    }

    // 双向点名：文档说出这条判据存在，判据认自己的定义点。
    assert!(
        doc.contains("directional_cross_references_point_the_right_side"),
        "接口文档不再点名本判据：读者看不出章内搬家会把方向词翻坏这件事有常驻核对"
    );
    let own_source = workspace_source("crates/qx-cli/src/tests/api_doc_cross_references.rs");
    // needle 由 format 拼：写字面量的话这一行自己就含 `fn …(`，定义点会被数成 2（自指）。
    let definition = format!(
        "fn {}(",
        "directional_cross_references_point_the_right_side"
    );
    let definitions = own_source.matches(definition.as_str()).count();
    assert_eq!(
        definitions, 1,
        "本判据的定义点数出 {definitions} 处，应为 1 处：0 处是判据被改名或搬走却没更新文档，\
         2 处是用例文件被复制出第二份"
    );
}
