//! 运行时身份的单源（U1）：版本、构建提交、目标三元组与构建档。
//!
//! `build.rs` 已把 git 提交烧进二进制，但这份身份此前散在 6 处 `env!(...)` 字面量里，
//! 命令面却没有任何入口能把它们打给用户——回测产物能追溯到提交，用户手上跑的二进制反而
//! 说不出自己是谁。本模块把"读身份"收成一处，`version` 出口、`doctor` 检查项与
//! `RunManifest` 字段都从这里取数。

/// cargo 包版本：与 `Cargo.toml` 单一来源。
pub(crate) const RUNTIME_VERSION: &str = env!("CARGO_PKG_VERSION");
/// 构建时的 git 提交（完整 40 位，脏工作树带 `-dirty`，无 git 时为 `unknown`）。
pub(crate) const BUILD_REVISION: &str = env!("QX_GIT_COMMIT");
/// 构建目标三元组，用于区分同版本号的不同安装包。
pub(crate) const TARGET_TRIPLE: &str = env!("QX_TARGET_TRIPLE");
/// 构建档（`release` / `debug`）：debug 二进制的哈希与性能都不可与 release 互证。
pub(crate) const BUILD_PROFILE: &str = env!("QX_BUILD_PROFILE");

/// 短提交号：保留 `-dirty` 后缀，否则"脏"这一格信息会在截断里丢失。
pub(crate) fn short_revision() -> String {
    match BUILD_REVISION.split_once('-') {
        Some((hash, suffix)) => format!("{}-{}", &hash[..hash.len().min(7)], suffix),
        None => BUILD_REVISION[..BUILD_REVISION.len().min(7)].to_string(),
    }
}

/// 一行的完整身份串：`version` 出口、`doctor` 检查项与文档核对共用这一份措辞。
pub(crate) fn identity_line() -> String {
    format!(
        "qianxing {} (build {}, target {}, profile {})",
        RUNTIME_VERSION,
        short_revision(),
        TARGET_TRIPLE,
        BUILD_PROFILE
    )
}

/// `version` 入口：只打印一行，不带横幅，便于脚本 `$(qx-cli version)` 取值。
pub(crate) fn print_identity() {
    println!("{}", identity_line());
}

/// doctor 报告里的一格：让"这个二进制能不能代表最新代码"成为可诊断项而不是口头承诺。
pub(crate) fn doctor_check() -> serde_json::Value {
    serde_json::json!({
        "name": "build_identity",
        "status": "pass",
        "message": identity_line(),
    })
}
