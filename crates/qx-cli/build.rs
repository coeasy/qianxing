//! 把构建时的代码身份烧进二进制。
//!
//! `RunManifest.code_commit` 参与结果摘要，此前恒为 `"workspace"`，于是两份不同代码
//! 跑出的回测清单长得一模一样，事后无法追溯结果出自哪个提交。git 不可用（源码包、
//! 没有 `.git` 的环境）时回落到 `unknown`；工作树有未提交改动时追加 `-dirty`，避免把
//! "哈希相同"读成"代码相同"。未跟踪文件不参与构建，因此不会把工作树标脏。

use std::path::Path;
use std::process::Command;

fn git(args: &[&str]) -> Option<String> {
    let output = Command::new("git").args(args).output().ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim().to_owned();
    (!text.is_empty()).then_some(text)
}

/// 让 cargo 在代码身份可能变化时重跑本脚本。只盯包内文件是不够的：提交代码后
/// `src/` 并未改动，旧哈希会被继续沿用。HEAD 与分支 ref 一旦被 `commit`/`checkout`
/// 改写就会触发重跑；路径不存在时不发指令，避免 cargo 退化成每次构建都重跑。
fn watch_head_and_branch() {
    watch(git(&["rev-parse", "--git-path", "HEAD"]));
    let head_ref = git(&["symbolic-ref", "--quiet", "HEAD"]);
    watch(
        head_ref
            .as_deref()
            .and_then(|reference| git(&["rev-parse", "--git-path", reference])),
    );
}

fn watch(path: Option<String>) {
    let Some(path) = path else { return };
    // build script 的工作目录就是包根，`--git-path` 返回的相对路径同基准，可直接判定。
    if Path::new(&path).exists() {
        println!("cargo:rerun-if-changed={path}");
    }
}

fn main() {
    watch_head_and_branch();
    let commit = git(&["rev-parse", "HEAD"]).filter(|value| {
        value.len() == 40 && value.chars().all(|character| character.is_ascii_hexdigit())
    });
    let identity = match commit {
        Some(commit) => {
            let dirty = git(&["status", "--porcelain", "--untracked-files=no"])
                .is_some_and(|status| !status.trim().is_empty());
            if dirty {
                format!("{commit}-dirty")
            } else {
                commit
            }
        }
        None => "unknown".to_string(),
    };
    println!("cargo:rustc-env=QX_GIT_COMMIT={identity}");
}
