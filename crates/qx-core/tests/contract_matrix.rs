//! `qx_core::contract::CONTRACT_MATRIX` 的形状用例（§7 M1）。
//!
//! 门禁 `contract_matrix_check` 把矩阵与**真实代码**对账（规范单点唯一、同名兄弟真在盘、adapter
//! 真有读者、没有未登记的第三个同名声明）。这里补的是**矩阵自身**的自洽：每行都要说得出话、
//! 序列化得出来、且"刻意不同层"的行必须写明理由——否则矩阵会退化成一张没人能读的装饰表。

use qx_core::contract::{contract_matrix, contract_matrix_json, CONTRACT_MATRIX};

/// 矩阵非空，且每一行的必填格都不为空。
#[test]
fn every_row_speaks_for_itself() {
    assert!(!CONTRACT_MATRIX.is_empty(), "契约矩阵不能是空表");
    for row in CONTRACT_MATRIX {
        assert!(!row.concept.is_empty(), "concept 不能为空");
        assert!(!row.canonical_types.is_empty(), "canonical_types 不能为空");
        assert!(
            row.canonical_source.starts_with("crates/"),
            "canonical_source 必须是仓内相对路径，实际 {}",
            row.canonical_source
        );
        assert!(!row.note.is_empty(), "note 不能为空：{}", row.concept);
        // 没有 adapter 的行必须靠 note 交代"为什么不用桥接"（同名但刻意不同层那一类）。
        for duplicate in row.duplicates {
            assert!(
                duplicate.contains('@'),
                "duplicates 每格写 `类型名@路径`，实际 {duplicate}"
            );
        }
    }
}

/// 视图函数与常量同源，JSON 形态把每一行都印出来（`GET /schema/contract-matrix` 读的就是它）。
#[test]
fn matrix_view_and_json_are_the_same_table() {
    assert_eq!(contract_matrix().len(), CONTRACT_MATRIX.len());
    let json = contract_matrix_json();
    for row in CONTRACT_MATRIX {
        assert!(
            json.contains(row.concept),
            "JSON 形态漏了 {}：{json}",
            row.concept
        );
    }
}

/// 三个**同名兄弟**概念必须在册——它们是 M1 要收的那三对（P1-5）。
#[test]
fn the_three_same_name_siblings_are_registered() {
    for concept in ["market_data_bar", "strategy_context", "data_provider"] {
        let row = CONTRACT_MATRIX
            .iter()
            .find(|row| row.concept == concept)
            .unwrap_or_else(|| panic!("契约矩阵缺少 {concept} 一行"));
        assert!(
            !row.duplicates.is_empty(),
            "{concept} 是同名兄弟，duplicates 不能为空"
        );
    }
    // 同名兄弟必须有桥接或写明"刻意不同层"，两者不能都没有。
    for row in CONTRACT_MATRIX.iter().filter(|row| !row.duplicates.is_empty()) {
        assert!(
            !row.adapter.is_empty() || row.note.contains("刻意"),
            "{} 既没 adapter 也没交代刻意不同层",
            row.concept
        );
    }
}
