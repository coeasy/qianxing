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

/// 把仓库 `deploy/` 顶层的示例配置烧进二进制（V13 第三十一遍 ②-b #266）。
///
/// `cargo install` 之后删掉 clone、或把单颗 exe 拷到另一台机器时，构建期源码树那一格候选根
/// 就没了，运行时模板只能靠仓库——`init` 与几条只读入口当场跑不通。这一层让 52 份示例配置随
/// 二进制走；清单为空时构建直接失败，发布物不该带着"模板缺失"这件事悄悄出门。
/// 清单同时给出内容签名（FNV-1a 覆盖每个名字与正文），运行期用它给落盘目录分桶，
/// 因此"同一目录名、不同内容"的陈旧副本不可能被读到。
fn embed_deploy_templates() {
    let manifest = std::env::var("CARGO_MANIFEST_DIR").expect("build script 由 cargo 驱动");
    let deploy = Path::new(&manifest).join("..").join("..").join("deploy");
    println!("cargo:rerun-if-changed={}", deploy.display());
    let mut names: Vec<String> = Vec::new();
    for entry in std::fs::read_dir(&deploy)
        .unwrap_or_else(|error| panic!("读取 deploy 目录失败 {}: {error}", deploy.display()))
        .flatten()
    {
        let path = entry.path();
        if path.extension().and_then(|ext| ext.to_str()) != Some("json") {
            continue;
        }
        let Some(name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        println!("cargo:rerun-if-changed={}", path.display());
        names.push(name.to_string());
    }
    names.sort();
    assert!(
        !names.is_empty(),
        "deploy/ 顶层没有 JSON 模板，构建产物不该缺这一层"
    );
    let mut table =
        String::from("// 由 build.rs 生成，勿手改：仓库 deploy/ 顶层 JSON 的构建期快照。\n");
    table.push_str("pub(crate) const DEPLOY_TEMPLATES: &[(&str, &str)] = &[\n");
    let mut signature: u64 = 0xcbf2_9ce4_8422_2325;
    for name in &names {
        let path = deploy.join(name);
        let body = std::fs::read_to_string(&path)
            .unwrap_or_else(|error| panic!("读取模板失败 {}: {error}", path.display()));
        let location = path.display().to_string();
        assert!(
            !location.contains('"') && !name.contains('"'),
            "路径里的引号会毁掉 include_str!: {location}"
        );
        for byte in name.as_bytes().iter().chain(body.as_bytes()) {
            signature ^= u64::from(*byte);
            signature = signature.wrapping_mul(0x0000_0100_0000_01b3);
        }
        table.push_str(&format!(
            "    (r\"{name}\", include_str!(r\"{location}\")),\n"
        ));
    }
    table.push_str("];\n");
    table.push_str(&format!(
        "pub(crate) const DEPLOY_EMBED_SIGNATURE: &str = \"{:016x}\";\n",
        signature
    ));
    let out_dir = std::env::var("OUT_DIR").expect("cargo 会交 OUT_DIR");
    std::fs::write(Path::new(&out_dir).join("deploy_templates.rs"), table)
        .expect("写入内置模板清单失败");
}

fn main() {
    watch_head_and_branch();
    embed_deploy_templates();
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
    // 提交号不足以区分"同一份代码的两种构建"：debug 与 release、Windows 与 Linux 的
    // 产物哈希与性能都不同。TARGET/PROFILE 由 cargo 交给 build script，显式转发一次，
    // 不依赖 rustc 内置 env! 是否可见。
    for (directive, variable, fallback) in [
        ("TARGET_TRIPLE", "TARGET", "unknown"),
        ("BUILD_PROFILE", "PROFILE", "unknown"),
    ] {
        let value = std::env::var(variable).unwrap_or_else(|_| fallback.to_string());
        println!("cargo:rustc-env=QX_{directive}={value}");
    }
}
