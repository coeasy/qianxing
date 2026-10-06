//! `quickstart` 收尾计数的目录预算（V13 R2 第三十遍 ②）。
//!
//! 这里刻意在进程内调用被测函数，与 `init_onboarding.rs` 的"一律跑真 binary"相反：那条文件
//! 判的是"屏幕上那行命令能不能跑"，本文件判的是一个纯算术性质——计数走到预算必须停。用子
//! 进程只能靠超时区分"卡死"与"还在跑"，而卡死正是这条判据要防的东西（`Path::is_dir()` 跟随
//! 符号链接，一个指回祖先的链接就能让五步全部跑完后的收尾行永不印出）。真实项目的落盘份数
//! 由 `init_onboarding.rs` 的 9+5 判据钉住，这里只补一句：生产预算不会把真实项目截断。

use super::*;
use std::path::Path;

/// 造一棵「根目录 1 份文件 + 每个子目录各 1 份文件」的树，返回目录根。
/// 每个子目录份数相同，所以截断点的计数与 `read_dir` 的返回顺序无关。
fn tree_of(root: &Path, dirs: usize) {
    std::fs::write(root.join("root.txt"), b"x").expect("根文件应当可写");
    for index in 0..dirs {
        let child = root.join(format!("d{index}"));
        std::fs::create_dir(&child).expect("子目录应当可建");
        std::fs::write(child.join("f.txt"), b"x").expect("子文件应当可写");
    }
}

/// 预算足够时全数计入、不截断；预算不足时在预算处停下并如实报截断。
#[test]
fn project_file_count_honours_its_directory_budget() {
    let root = temp_cli_case_dir("r2-quickstart-count-budget");
    // 4 个目录（根 + 3 子）、4 份文件
    tree_of(&root, 3);
    assert_eq!(
        quickstart::project_file_count(&root, 100),
        (4, false),
        "预算足够时份数必须精确，否则收尾行会少计"
    );
    assert_eq!(
        quickstart::project_file_count(&root, 1),
        (1, true),
        "预算 1 只够打开根目录"
    );
    assert_eq!(
        quickstart::project_file_count(&root, 2),
        (2, true),
        "预算 2 只够再打开一个子目录"
    );
    assert_eq!(
        quickstart::project_file_count(&root, 0),
        (0, true),
        "预算 0 必须立刻返回，而不是把整棵树扫完再报截断"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 生产预算离真实项目还有两个数量级：`init` 落盘的项目不触发截断文案。
#[test]
fn production_budget_never_truncates_a_real_project() {
    let root = temp_cli_case_dir("r2-quickstart-production-budget");
    let runtime = root.join("qianxing.runtime.json");
    run_init_with_profile(&runtime, false, Some("macd"), Some("builtin")).expect("init 应当成功");
    let (files, truncated) =
        quickstart::project_file_count(&root, quickstart::PROJECT_FILE_COUNT_BUDGET);
    assert!(!truncated, "真实项目被生产预算截断了：份数 {files}");
    assert_eq!(files, 10, "与 init_onboarding.rs 的 10 份口径不一致");
    let _ = std::fs::remove_dir_all(root);
}
