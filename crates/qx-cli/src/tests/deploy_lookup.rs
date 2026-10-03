//! `deploy/` 查找面的性质（V13 第三十一遍 ①，易用性 F1/F2/F4）。
//!
//! 这里只测链条本身的形状：候选根的顺序、先命中先用、什么样的路径才允许被重定位、未命中说明有没有
//! 把找过的位置都点名。端到端"在无关目录里裸敲三条入口能不能跑通"归
//! `crates/qx-cli/tests/default_example_paths.rs`——那一半要的是真子进程与真屏幕。
//!
//! 本文件不碰 `QX_DEPLOY_DIR`（进程级环境变量，并行用例下会互相污染）：顺序由参数化的
//! `deploy_candidate_roots` 判，重定位由源码树那一个真实存在的根判。

use super::*;
use std::path::{Path, PathBuf};

/// 构建期源码树里的那份 `deploy/`，与 `deploy_lookup.rs` 用的是同一个表达式。
fn source_tree_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
}

/// 把候选根转成统一分隔符的字符串，便于跨 Windows/Linux 断言顺序。
fn listed(roots: &[PathBuf]) -> Vec<String> {
    roots
        .iter()
        .map(|root| root.to_string_lossy().replace('\\', "/"))
        .collect()
}

/// 内置模板桶是一个按内容签名命名的**共享目录**：落盘、删除与「没有落盘」三格判据都指着它，
/// 并行用例会互相把对方刚删掉的那份补回来。凡是要动这个桶的用例都先拿这把锁。
static DEPLOY_BUCKET_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

fn lock_deploy_bucket() -> std::sync::MutexGuard<'static, ()> {
    DEPLOY_BUCKET_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

/// 候选根的顺序就是文档承诺的顺序；空的 `QX_DEPLOY_DIR` 不占位。
#[test]
fn candidate_roots_follow_the_documented_priority() {
    let source = listed(&[source_tree_root()]).remove(0);
    assert_eq!(
        listed(&deploy_candidate_roots(
            Some("given"),
            Some(Path::new("bin/sub"))
        )),
        vec![
            "given".to_string(),
            "bin/sub/deploy".to_string(),
            "bin/deploy".to_string(),
            source.clone(),
            "deploy".to_string()
        ],
        "查找顺序变了就要同时改 deploy_lookup.rs 的模块说明与 README 安装段"
    );
    assert_eq!(
        listed(&deploy_candidate_roots(Some(""), None)),
        vec![source, "deploy".to_string()],
        "空 QX_DEPLOY_DIR 不该占一个位置"
    );
}

/// 先命中先用：两边都有时取靠前的根，只有靠后的根有时也要继续往后找。
#[test]
fn pick_deploy_file_takes_the_first_root_holding_the_name() {
    let root = temp_cli_case_dir("r31-deploy-pick");
    let first = root.join("first");
    let second = root.join("second");
    std::fs::create_dir_all(&first).expect("候选根应当可建");
    std::fs::create_dir_all(&second).expect("候选根应当可建");
    std::fs::write(first.join("both.json"), b"first").expect("样例文件应当可写");
    std::fs::write(second.join("both.json"), b"second").expect("样例文件应当可写");
    let only_second = second.join("only.json");
    std::fs::write(&only_second, b"{}").expect("样例文件应当可写");
    let roots = vec![first.clone(), second.clone()];
    assert_eq!(
        pick_deploy_file(&roots, "both.json").as_deref(),
        Some(first.join("both.json").as_path()),
        "两个根都有时必须取靠前的那个，否则 QX_DEPLOY_DIR 压不住源码树"
    );
    assert_eq!(
        pick_deploy_file(&roots, "only.json").as_deref(),
        Some(only_second.as_path()),
        "靠前的根没有该文件时要继续往后找"
    );
    assert_eq!(pick_deploy_file(&roots, "absent.json"), None);
    let _ = std::fs::remove_dir_all(root);
}

/// `..` 只做词汇折叠：不碰磁盘、不跟随符号链接，也无物可折时把 `..` 留着。
#[test]
fn parent_segments_collapse_lexically() {
    assert_eq!(
        collapse_parent_segments(Path::new("deploy/../qianxing/x.json")),
        PathBuf::from("qianxing/x.json")
    );
    assert_eq!(
        collapse_parent_segments(Path::new("./deploy/x.json")),
        PathBuf::from("deploy/x.json")
    );
    assert_eq!(
        collapse_parent_segments(Path::new("../deploy/x.json")),
        PathBuf::from("../deploy/x.json"),
        "上面没有可退的段时要把 .. 留着，折叠它会指向另一个位置"
    );
}

/// 只有「`deploy/<名字>` 且当前目录没有这一份」才重定位；其余形状原样通过。
#[test]
fn resolve_deploy_path_relocates_only_a_missing_example_shape() {
    let root = temp_cli_case_dir("r31-deploy-resolve");
    let absolute = root.join("deploy").join("x.json");
    assert_eq!(
        resolve_deploy_path(&absolute),
        absolute,
        "绝对路径不该被改写"
    );
    let other_shape = PathBuf::from("fixtures/sub/x.json");
    assert_eq!(
        resolve_deploy_path(&other_shape),
        other_shape,
        "不是 deploy/<名字> 的相对路径原样交给下游"
    );
    // 同名那份在候选根里真的存在：多一段就不算示例形状，链子不许越过使用者写下的层级。
    let nested = PathBuf::from("deploy/nested/qianxing.runtime.example.json");
    assert_eq!(
        resolve_deploy_path(&nested),
        nested,
        "deploy/<目录>/<名字> 不是示例形状，不能被收成根目录那份"
    );
    let absent = PathBuf::from("deploy/qianxing.r31-this-name-exists-nowhere.example.json");
    assert_eq!(
        resolve_deploy_path(&absent),
        absent,
        "哪都找不着时原样返回，让错误文案指向使用者写下的那条路径"
    );
    let example = PathBuf::from("deploy/qianxing.runtime.example.json");
    let resolved = resolve_deploy_path(&example);
    assert_eq!(
        resolved,
        collapse_parent_segments(&source_tree_root().join("qianxing.runtime.example.json")),
        "示例形状在候选根里有时，默认值该走查找链拿到那一份"
    );
    assert!(
        !resolved.to_string_lossy().contains(".."),
        "屏幕上那行路径不该还带 ..: {}",
        resolved.display()
    );
    assert!(
        resolved.is_file(),
        "查找链给出的路径必须真的存在: {}",
        resolved.display()
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 未命中说明把每个候选根都点名并给出处置办法；缺一条就等于把使用者送回 os error 3 那面墙前。
#[test]
fn miss_note_names_every_root_and_the_remedies() {
    let note = deploy_miss_note(
        "qianxing.runtime.example.json",
        &[
            PathBuf::from("first/deploy"),
            PathBuf::from("sub/../second/deploy"),
            PathBuf::from("deploy"),
        ],
    );
    // 屏幕上那行的分隔符由平台决定，断言只看折叠之后的位置本身。
    let listed = note.replace('\\', "/");
    for root in ["first/deploy", "second/deploy", "deploy"] {
        assert!(listed.contains(root), "说明里没有点名 {root}:\n{note}");
    }
    assert!(
        listed.contains(DEPLOY_DIR_ENV),
        "说明里没有给出 {DEPLOY_DIR_ENV} 这条出路:\n{note}"
    );
    assert!(
        listed.contains("内置模板清单"),
        "说明没交代二进制自带的那一层，读者不知道该不该期待内置副本:\n{note}"
    );
    assert!(
        listed.contains("1."),
        "说明没有编号，读者数不清找过几处:\n{note}"
    );
    assert!(
        !listed.contains("..") && !listed.contains("sub/"),
        "说明里的根还带着 .. 或 sub/，读者抄不回终端:\n{note}"
    );
}

/// 补话只在「显式给的示例形状读不到、而别处真有这一份」时出现，并点名那一份的位置。
#[test]
fn relocation_hint_only_arms_for_a_missing_relocatable_shape() {
    let roots = current_deploy_roots();
    let hint = deploy_relocation_hint(Path::new("deploy/qianxing.runtime.example.json"), &roots);
    assert!(
        hint.contains("这一份示例在别处存在"),
        "示例形状读不到时该给补话，实际: {hint:?}"
    );
    assert!(
        hint.contains("qianxing.runtime.example.json"),
        "补话要点名真正那一份的文件名，实际: {hint:?}"
    );
    assert_eq!(
        deploy_relocation_hint(
            Path::new("deploy/qianxing.r31-absent-name.example.json"),
            &roots,
        ),
        "",
        "哪都没有时不该编造一个位置"
    );
    assert_eq!(
        deploy_relocation_hint(Path::new("fixtures/sub/x.json"), &roots),
        "",
        "不是示例形状的路径由各自的错误口径负责"
    );
}

/// 唯一读取口把两半交付在同一句里，内置层一次落整份清单。两步合在一条用例里，是因为它们动的是
/// 同一个共享桶；桶本身另由 `DEPLOY_BUCKET_LOCK` 串起来，否则删下去的那几份会被并行用例补回来。
#[test]
fn the_funnel_names_the_other_copy_and_the_builtin_layer_brings_the_siblings() {
    let _bucket_guard = lock_deploy_bucket();
    let name = "qianxing.fast-backtest.ashare.example.json";
    let siblings = [
        "qianxing.runtime.ashare.example.json",
        "qianxing.ashare.bar-frame.example.json",
        "qianxing.ashare.spot.spec.json",
    ];
    let typed = Path::new(DEPLOY_DIR_NAME).join(name);
    let bucket_dir = std::env::temp_dir().join(format!("qianxing-deploy-{DEPLOY_EMBED_SIGNATURE}"));
    let bucket = bucket_dir.join(name);
    // 先把要判的那几份从桶里删掉：这台机器的桶常常已被上一轮落满整份清单，
    // 不删的话「内置层落整份」只是在确认环境残留，换不到一格真判据。
    for entry in std::iter::once(name).chain(siblings.iter().copied()) {
        let _ = std::fs::remove_file(bucket_dir.join(entry));
    }
    assert!(
        !typed.exists(),
        "这条判据要的是「当前目录没有、别处真有」那一格；cargo test 的工作目录是 crate 目录"
    );
    let error =
        read_example_json(&typed, "快速回测 manifest ").expect_err("crate 目录里没有这一份");
    assert!(
        error.contains("读取快速回测 manifest 失败 "),
        "报错正文的第一句口径不能改: {error}"
    );
    assert!(
        error.contains("这一份示例在别处存在"),
        "唯一读取口没接上补话，使用者仍会留在 os error 3 前:\n{error}"
    );
    assert!(
        !bucket.exists(),
        "读不到的那条路把内置模板落到了磁盘上: {}",
        bucket.display()
    );
    let found = locate_deploy_file(&[], name).expect("内置清单里该有这份 manifest");
    let dir = found.parent().expect("桶内路径该有父目录");
    assert!(
        bucket.is_file(),
        "内置层没把被点名的那份落下来: {}",
        bucket.display()
    );
    for sibling in siblings {
        assert!(
            dir.join(sibling).is_file(),
            "manifest 引用的 {sibling} 没跟着落盘，换到桶里的那条链会在下一格读取上断掉"
        );
    }
}

/// 内置清单必须与仓库 `deploy/` 顶层的 JSON 名单逐名一致：少了意味着安装产物缺模板，
/// 多了意味着仓库里删掉的那份还跟着二进制走。
#[test]
fn built_in_template_roster_matches_the_repository_deploy_folder() {
    let mut embedded = DEPLOY_TEMPLATES
        .iter()
        .map(|(name, _)| (*name).to_string())
        .collect::<Vec<_>>();
    embedded.sort();
    let mut on_disk = std::fs::read_dir(source_tree_root())
        .expect("候选根里该有源码树的 deploy/")
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            let name = path.file_name()?;
            if path.extension()?.to_str()? != "json" {
                return None;
            }
            Some(name.to_string_lossy().into_owned())
        })
        .collect::<Vec<_>>();
    on_disk.sort();
    assert_eq!(
        embedded, on_disk,
        "内置模板清单与仓库 deploy/ 顶层不同名（构建期快照过期或 deploy 面漂移）"
    );
    assert!(
        DEPLOY_TEMPLATES
            .iter()
            .map(|(_, body)| body.len())
            .sum::<usize>()
            > 100_000,
        "内置清单几乎为空，说明 build.rs 展开失败"
    );
}

/// 候选根全空时仍能拿到随二进制走的那一份，且内容逐字节等于仓库那一份；清单外的名字不许造文件。
#[test]
fn locate_deploy_file_falls_back_to_the_copy_shipped_in_the_binary() {
    let _bucket_guard = lock_deploy_bucket();
    let name = "qianxing.runtime.example.json";
    let found = locate_deploy_file(&[], name).expect("内置清单里该有这份示例配置");
    assert!(
        found.starts_with(std::env::temp_dir()),
        "内置模板应落在当前用户的临时目录，实际: {}",
        found.display()
    );
    assert_eq!(
        std::fs::read(&found).expect("读回内置模板失败"),
        std::fs::read(collapse_parent_segments(&source_tree_root().join(name)))
            .expect("读回仓库模板失败"),
        "内置那份与仓库那份内容不同——签名分桶没能挡住过期副本"
    );
    assert_eq!(
        locate_deploy_file(&[], "qianxing.r31-not-a-template.json"),
        None,
        "清单外的名字不该凭空造出一份文件"
    );
}

/// 报错路径只认目录、不碰内置层：把"找不到"说出来不该顺手往临时目录写东西。
///
/// 内置层在这台开发机上永远排在候选根之后，所以这一格只能由"把根清空"造出来：清空之后
/// 读取路径会去写临时副本，报错路径必须仍然闭嘴。这正是 `pick_deploy_file` 与
/// `locate_deploy_file` 分成两个入口的全部理由。
#[test]
fn the_error_surface_never_materialises_a_template() {
    let _bucket_guard = lock_deploy_bucket();
    let name = "qianxing.bar-frame.example.json";
    assert!(
        DEPLOY_TEMPLATES.iter().any(|(entry, _)| *entry == name),
        "内置清单里该有 {name}，否则这条判据什么都没测到"
    );
    let written = std::env::temp_dir()
        .join(format!("qianxing-deploy-{DEPLOY_EMBED_SIGNATURE}"))
        .join(name);
    let _ = std::fs::remove_file(&written);
    assert_eq!(
        deploy_relocation_hint(
            Path::new("deploy/qianxing.r31-absent-name.example.json"),
            &[]
        ),
        "",
        "清单外的名字不该有补话"
    );
    assert_eq!(
        deploy_relocation_hint(Path::new(&format!("deploy/{name}")), &[]),
        "",
        "候选根全空时，报错路径不该指向内置层刚落盘的那一份"
    );
    assert!(
        !written.exists(),
        "补话的读法把内置模板写到了磁盘上: {}",
        written.display()
    );
}
