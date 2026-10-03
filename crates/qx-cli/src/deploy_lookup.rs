//! `deploy/` 示例配置的查找面（V13 第三十一遍 ①，易用性 F1/F2/F4）。
//!
//! 改前两套默认路径机制并存：`doctor`/`status`/`report`/`config *` 经
//! `repository_deploy_path` 能回到仓库里的 `deploy/`，而 `runtime-check`/`live-check`/
//! `paper-check` 只是把 `"deploy/…"` 字面量拼在当前目录上。同一棵树、同一个无关启动目录，
//! 前一组退 0、后一组退 2 并回 `读取运行时配置失败 deploy/…: 系统找不到指定的路径`
//! （`logs/s734_pass31_default_path_probe.txt`）。本轮收成一条链：默认值只经这一个入口解析，
//! 找过哪些位置在未命中时原样列出。
//!
//! 查找顺序（先命中先用，每个根都按同一层文件名取）：
//!
//! 1. `QX_DEPLOY_DIR` 指向的目录——exe 与仓库分离时的显式出口；
//! 2. 可执行文件同级的 `deploy/`，再往外一层——自包含发行目录的形状；
//! 3. 构建期源码树的 `deploy/`——开发机上 `cargo run` 的形状（这台机器上没有源码树时它不存在，
//!    于是自动退出查找，不会让安装产物指向一台不存在的开发机）；
//! 4. 当前工作目录的 `deploy/`——在仓库根里逐条敲命令的形状；
//! 5. 二进制里的内置模板清单（构建期由 `build.rs` 快照 `deploy/` 顶层的 JSON）——需要时把
//!    **整份清单**落进当前用户的临时目录，按清单内容签名分桶。`cargo install` 后删掉 clone、或把
//!    单颗 exe 拷到别处时，前四格可能一格都不剩，这一层让 `init` 与只读入口仍有模板可读（#266）。
//!    落整份而不是只落被点名的那一个：`fast-backtest` 的 manifest 里 jobs 按**同级文件名**引用
//!    runtime/bars/spec，只落一份会让这条链在下一格读取上断掉。
//!
//! 内置层只接读取路径（`locate_deploy_file`）。报错文案走 `pick_deploy_file`：说出"找不到"
//! 那一格不该顺手往磁盘写东西，也不该把刚写出的临时副本指给人看。
//!
//! 两个口径要分清，本轮的补话覆盖就是按这条线铺的：
//!
//! - 接了查找面的入口（`parse_deploy_path` 挂在只读入口的默认值与手打的显式路径上——含 V13 #274 补上的 backtest / paper-submit-order / reconcile 这类无默认值的必填位置参数；`fast-backtest` 的 manifest 由
//!   读点自己调 `relocate_deploy_path`）里，默认值和手打的同一条路径吃同一条规则：只要它是"示例配置
//!   形状"（`deploy/<文件名>` 两段）且当前目录没有这一份，就按上面的顺序搬过去，并在 stderr 说一句
//!   `[查找 · Lookup]`；绝对路径与当前目录真有的那一份照原样交给下游。
//! - 没挂这个解析器的入口（`scheduler-worker`、`dataset-*`、`strategy backtest`，以及要实盘凭据与运行中
//!   worker 的 `binance-submit-order`——与 paper 侧那条日常入口相反，属精确路径）按当前目录原样解析，
//!   读不到时由 `read_example_json` 点名"别处真有这一份"的位置。
//!
//! 写目标（`--output`、lock 产物）两条都不接，避免把该新建的文件改写进别处。

use std::io::Write;
use std::path::{Component, Path, PathBuf};

include!(concat!(env!("OUT_DIR"), "/deploy_templates.rs"));

/// clap 取值解析器：把命令行上的一条示例配置路径交给查找面（`resolve_deploy_path`）。
///
/// 放在查找面这一侧而不是 `cli_args.rs`，是因为后者正卡在 500 行的单文件门槛上；
/// 职责本身也属于"示例配置怎么找"，不属于"命令表长什么样"。
///
/// 换位置必须说出来：屏幕上那行 `config_fingerprint=` 从此描述的是另一份文件，静默替换等于
/// 让使用者对一份没人点过名的配置签字。写到 stderr 而不是 stdout，`--json` 的机器读者不受影响。
pub(crate) fn parse_deploy_path(value: &str) -> Result<PathBuf, String> {
    Ok(relocate_deploy_path(Path::new(value)))
}

/// 读点用的重定位入口：与 clap 那个解析器同一条规则，只是它挂在读取处而不是参数上。
///
/// 两种挂法给的性质一样（默认值与手打的路径吃同一条链，换位置必说出来），但 `cli_args.rs`
/// 已经贴着 500 行的单文件门槛，把"命令表"继续当"查找面"的挂载点只会让职责往错方向长。
pub(crate) fn relocate_deploy_path(typed: &Path) -> PathBuf {
    let resolved = resolve_deploy_path(typed);
    if resolved.exists() && resolved != collapse_parent_segments(typed) {
        eprintln!(
            "[查找 · Lookup] {} 不在当前目录，改用 {}",
            typed.display(),
            resolved.display()
        );
    }
    resolved
}

/// 显式指定 `deploy/` 目录的环境变量名。集成用例也用它注入合成根，因此不必把 exe 复制到别处
/// 就能整条测到第 1 优先级。
pub(crate) const DEPLOY_DIR_ENV: &str = "QX_DEPLOY_DIR";

/// 示例配置所在目录名，四套候选根共用。
pub(crate) const DEPLOY_DIR_NAME: &str = "deploy";

/// 构建期源码树里的那份 `deploy/`；与 `repository_deploy_path` 迁移前的第一选择同一路径。
fn source_tree_deploy_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join(DEPLOY_DIR_NAME)
}

fn push_unique(roots: &mut Vec<PathBuf>, root: PathBuf) {
    if !roots.iter().any(|seen| seen == &root) {
        roots.push(root);
    }
}

/// 候选 `deploy/` 根，按优先级排好。参数化而非直接读环境，是为了让"顺序"这条性质可单测。
pub(crate) fn deploy_candidate_roots(
    explicit_env: Option<&str>,
    exe_dir: Option<&Path>,
) -> Vec<PathBuf> {
    let mut roots = Vec::new();
    if let Some(root) = explicit_env.filter(|root| !root.is_empty()) {
        push_unique(&mut roots, PathBuf::from(root));
    }
    if let Some(exe_dir) = exe_dir {
        push_unique(&mut roots, exe_dir.join(DEPLOY_DIR_NAME));
        if let Some(parent) = exe_dir.parent() {
            push_unique(&mut roots, parent.join(DEPLOY_DIR_NAME));
        }
    }
    push_unique(&mut roots, source_tree_deploy_dir());
    push_unique(&mut roots, PathBuf::from(DEPLOY_DIR_NAME));
    roots
}

/// 按本机现状算出的候选根（环境变量 + 可执行文件位置）。
pub(crate) fn current_deploy_roots() -> Vec<PathBuf> {
    let explicit = std::env::var(DEPLOY_DIR_ENV).ok();
    let exe_dir = std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|dir| dir.to_path_buf()));
    deploy_candidate_roots(explicit.as_deref(), exe_dir.as_deref())
}

/// 词汇折叠：把 `crates\qx-cli\..\..\deploy\x.json` 这类带 `..` 的路径收成一条能读的写法。
/// 用 `canonicalize` 不行——Windows 上它会带出 `\\?\` 前缀，屏幕上那行反而更难读；这里只做
/// 段级折叠，不跟随符号链接，也不承诺与解析后的真身一致。
pub(crate) fn collapse_parent_segments(path: &Path) -> PathBuf {
    let mut kept: Vec<Component<'_>> = Vec::new();
    for component in path.components() {
        match component {
            Component::CurDir => {}
            Component::ParentDir if matches!(kept.last(), Some(Component::Normal(_))) => {
                kept.pop();
            }
            other => kept.push(other),
        }
    }
    kept.iter().collect()
}

/// 第一个真的持有这份文件的候选根。只认真实目录：报错路径也走这里，所以"找不到"不会顺手写盘。
pub(crate) fn pick_deploy_file(roots: &[PathBuf], file_name: &str) -> Option<PathBuf> {
    roots
        .iter()
        .map(|root| collapse_parent_segments(&root.join(file_name)))
        .find(|path| path.is_file())
}

/// 候选根之外的最后一层：随二进制走的那份模板，需要时落进当前用户的临时目录。
pub(crate) fn locate_deploy_file(roots: &[PathBuf], file_name: &str) -> Option<PathBuf> {
    pick_deploy_file(roots, file_name).or_else(|| embedded_deploy_file(file_name))
}

/// 落盘按内容签名分桶，因此同名目录里的旧副本不可能与新二进制内容不同却互相顶掉。
///
/// 每次进这一层都先补齐整份清单，再回答被点名的那一个：桶可能由上一版二进制留下半份，而
/// manifest 的兄弟引用是按同级文件名读的，缺一格就断一条链。
fn embedded_deploy_file(file_name: &str) -> Option<PathBuf> {
    if !DEPLOY_TEMPLATES.iter().any(|(name, _)| *name == file_name) {
        return None;
    }
    let dir = std::env::temp_dir().join(format!("qianxing-deploy-{DEPLOY_EMBED_SIGNATURE}"));
    std::fs::create_dir_all(&dir).ok()?;
    materialise_templates(&dir);
    dir.join(file_name).is_file().then_some(dir.join(file_name))
}

/// 把清单里还没落盘的那几份补进桶里；先写独占的 .part 再改名，并发下另一进程只会读到写完的那份。
/// 单份失败不连累其余，缺的那格由它自己的读取报错负责。
fn materialise_templates(dir: &Path) {
    for (name, payload) in DEPLOY_TEMPLATES {
        let target = dir.join(name);
        if target.is_file() {
            continue;
        }
        let staging = dir.join(format!("{}.{}.part", name, std::process::id()));
        if write_whole_file(&staging, payload) {
            let _ = std::fs::rename(&staging, &target);
        }
        let _ = std::fs::remove_file(&staging);
    }
}

/// 本机偶发 os error 5（追加锁的同一家族），重试只认整篇写完。
fn write_whole_file(path: &Path, payload: &str) -> bool {
    for attempt in 0..3 {
        match std::fs::File::create_new(path) {
            Ok(mut file) => {
                return file.write_all(payload.as_bytes()).is_ok() && file.flush().is_ok();
            }
            Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => return true,
            Err(error) if error.kind() == std::io::ErrorKind::PermissionDenied && attempt < 2 => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(_) => return false,
        }
    }
    false
}

/// `deploy/<文件名>` 这种示例形状，且当前目录下这一份不存在——只有它才允许被重定位。
fn is_relocatable_example(path: &Path, components: &[Component<'_>]) -> bool {
    path.is_relative()
        && components.len() == 2
        && components.first() == Some(&Component::Normal(std::ffi::OsStr::new(DEPLOY_DIR_NAME)))
}

/// 把一条路径过一遍查找面：命中候选根就用那份，否则原样返回（折叠 `..` 之后）。
pub(crate) fn resolve_deploy_path(path: &Path) -> PathBuf {
    let components: Vec<Component<'_>> = path.components().collect();
    if !(is_relocatable_example(path, &components) && !path.exists()) {
        return collapse_parent_segments(path);
    }
    let Some(Component::Normal(file_name)) = components.get(1) else {
        return collapse_parent_segments(path);
    };
    locate_deploy_file(&current_deploy_roots(), &file_name.to_string_lossy())
        .unwrap_or_else(|| collapse_parent_segments(path))
}

/// 显式路径读不到时的补话：只有「`deploy/<名字>` 这种示例形状、当前目录没有、但别的候选根
/// 真有这一份」才开口。默认值会自动走完查找链，显式路径按当前目录解析——这条不对称是刻意的
/// （显式写下的路径该由使用者说了算），但光失败一行 os error 3 没人看得懂差在哪，所以在这里点名。
pub(crate) fn deploy_relocation_hint(path: &Path, roots: &[PathBuf]) -> String {
    let components: Vec<Component<'_>> = path.components().collect();
    if path.exists() || !is_relocatable_example(path, &components) {
        return String::new();
    }
    let Some(Component::Normal(file_name)) = components.get(1) else {
        return String::new();
    };
    match pick_deploy_file(roots, &file_name.to_string_lossy()) {
        Some(found) => format!(
            "\n  这一份示例在别处存在: {}\n  默认值会自己走到这里；显式给出的路径按当前目录解析，所以要自己给对位置",
            found.display()
        ),
        None => String::new(),
    }
}

/// 示例配置的唯一读取口：把「读不到」与「那一份在别处」交在同一句里。
///
/// 改前每个入口各写一遍 `读取…失败 {path}: {error}`，只有 `runtime-check` 那条链接了补话，于是
/// 同一棵树、同一个无关目录里 `runtime-check` 会说"在别处存在"，而 `fast-backtest` 只留一行
/// os error 3（`logs/s750_pass31_standalone_fast_backtest.txt`）。收成一处之后，新增入口不会漏。
///
/// `label` 是屏幕上「读取…失败」之间那一段**原文**，连它自己的空格一起给：这几句文案已经被用例
/// 与产物钉住（`src/tests/backtest_input_provenance.rs` 判 `读取策略回测 BarFrame 失败`），
/// 收口时不许顺手统一标点。
pub(crate) fn read_example_json(path: &Path, label: &str) -> Result<String, String> {
    std::fs::read_to_string(path).map_err(|error| {
        format!(
            "读取{label}失败 {}: {error}{}",
            path.display(),
            deploy_relocation_hint(path, &current_deploy_roots())
        )
    })
}

/// 未命中说明：把「找过哪里」与「怎么办」交在同一屏。改前的错误只印一个位置
/// （`找不到初始化样例文件: deploy/…`），使用者据此以为仓库坏了，而真因常常是 exe 与
/// `deploy/` 分离——那份文件其实就在这台机器上，只是不在当前目录那一层。
pub(crate) fn deploy_miss_note(file_name: &str, roots: &[PathBuf]) -> String {
    let mut lines = vec![format!("按查找顺序找过这些目录，都没有 {file_name}：")];
    for (index, root) in roots.iter().enumerate() {
        lines.push(format!(
            "  {}. {}",
            index + 1,
            collapse_parent_segments(root).display()
        ));
    }
    lines.push(
        "  出路三选一：把仓库的 `deploy/` 放到可执行文件同级或上一级；或设置 \
         {DEPLOY_DIR_ENV}=<deploy 目录>；或显式给出该文件的完整路径。"
            .replace("{DEPLOY_DIR_ENV}", DEPLOY_DIR_ENV),
    );
    lines.push(format!(
        "  二进制的内置模板清单（{} 份）里也没有这个名字。",
        DEPLOY_TEMPLATES.len()
    ));
    lines.join("\n")
}
