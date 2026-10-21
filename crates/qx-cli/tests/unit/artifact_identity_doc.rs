//! 交付文档必须按**载荷**认发布产物，不许把整档 sha 当身份证（V13 R2 第六遍，#159 收口）。
//!
//! 立案缘由是两轮实测：同一份 wheel 载荷重打包，17 个条目的 CRC 全同而整档 sha 变了（zip 时间戳）；
//! 同一条 `cargo build --release` 跑两次，exe 也是同尺寸不同 sha。也就是说整档 sha 既不能证明
//! "是这一份产物"，也不能证明"不是"——把它写进交付文档，读者按它核对会得出错误结论，而门禁与用例
//! 谁都不会抱怨。README 本轮已把活口径换成尺寸 / 条目数 / 条目 CRC / 内嵌扩展 md5 / 二进制内文案
//! 字节计数，这条用例负责让那句解释和那套口径不能再被悄悄改回去。
//!
//! 取数只看两份**交付**文档（`README.md`、`deploy/README.md`）。`docs/` 与 `CHANGELOG.md` 的执行记录
//! 里那些 `2603b5c2…` 是当时现场的抄录，属于历史记录而不是口径，所以不进这条判据。

use super::*;

/// 交付文档：读者按它们装东西、核对装到的东西是不是当轮那一份。
const DELIVERY_DOCS: [&str; 2] = ["README.md", "deploy/README.md"];

/// 这条判据要求 README 至少保住的口径骨架。删掉任意一条都会让"为什么不报 sha"失去解释，
/// 读者就会退回按整档 sha 核对。
const PAYLOAD_IDIOM: [&str; 7] = [
    "为什么这里不报整档 sha256",
    "产物身份只按",
    "条目 CRC",
    "`_qianxing_native.pyd` 的 md5",
    "`target/release/qx-cli.exe` 的字节数",
    "crates/qx-cli/tests/unit/artifact_identity_doc.rs",
    // #179：计数只能证明"当轮改动真的进了发布物"，不能证明反方向。删掉这句，读者就会拿
    // "数到 0"当成"这个构建没有这个能力"的结论，而链接器池化/死代码消除随时能造出这种 0。
    "字面量计数是单向证据",
];

/// 文本里最长的连续十六进制串长度：64 位那一段就是 sha256 的线上形态，32 位是本轮认可的 md5。
fn longest_hex_run(text: &str) -> usize {
    let mut best = 0_usize;
    let mut current = 0_usize;
    for ch in text.chars() {
        if ch.is_ascii_hexdigit() && !ch.is_ascii_uppercase() {
            current += 1;
            best = best.max(current);
        } else {
            current = 0;
        }
    }
    best
}

#[test]
fn delivery_docs_identify_release_artifacts_by_payload_not_archive_sha() {
    for doc in DELIVERY_DOCS {
        let text = workspace_source(doc);
        let longest = longest_hex_run(&text);
        assert!(
            longest < 64,
            "{doc} 里出现 {longest} 位连续十六进制串——那是整档 sha 的形态。发布产物的整档 sha \
             每次重打包都会变（zip 时间戳 / 重链接），把它写进交付文档等于给读者一把量不出东西的尺子；\
             要改就改成尺寸 / 条目数 / 条目 CRC / 内嵌扩展 md5（#159）。"
        );
        assert!(
            !text.contains("sha256="),
            "{doc} 用 `sha256=` 引用产物身份：这一形态来自打包器的打印，同一载荷重打包就会换值。\
             协议字段里的 sha256（数据集指纹那类）不受这条限制，它不是产物身份证。"
        );
    }

    let readme = workspace_source(DELIVERY_DOCS[0]);
    for idiom in PAYLOAD_IDIOM {
        assert!(
            readme.contains(idiom),
            "README.md 丢了载荷口径骨架的一句：{idiom:?}——它撑住的是「产物身份按载荷报」这条口径，\
             以及文档与本用例的互相点名"
        );
    }

    // 双向点名：本用例也要把被核对的两份文档写在源码里，否则换一份文档就能绕开这条判据。
    let judge = workspace_source("crates/qx-cli/tests/unit/artifact_identity_doc.rs");
    for doc in DELIVERY_DOCS {
        assert!(
            judge.contains(doc),
            "判据侧不再点名 {doc}：把交付文档换成另一份没人核对的文件，这条判据就空转了"
        );
    }
    assert!(
        judge.contains("条目 CRC") && judge.contains("md5"),
        "判据侧不再写出口径要求（条目 CRC / md5），只剩一条「不许有 64 位十六进制」的负向检查"
    );
}

/// Both public wheel builders delegate extension compilation and ABI tagging to PEP 517 maturin.
const PACKAGING_STEPS: [(&str, &str, [&str; 3]); 2] = [
    (
        "tools/build_python_wheel.ps1",
        "powershell",
        [
            "-m pip --version",
            "-m maturin --version",
            "-m pip wheel --no-deps --no-build-isolation",
        ],
    ),
    (
        "tools/build_python_wheel.sh",
        "bash",
        [
            "-m pip --version",
            "-m maturin --version",
            "-m pip wheel --no-deps --no-build-isolation",
        ],
    ),
];

/// README 里那句"打包入口只有这两个脚本"的说明，与本判据互相点名。
const STAGING_PROMISE: &str = "maturin 从当前源码和 qx-python Cargo manifest 构建原生扩展";

#[test]
fn wheel_packaging_entry_uses_the_maturin_backend() {
    for (script, interpreter, steps) in PACKAGING_STEPS {
        let text = workspace_source(script);
        let mut previous = usize::MAX;
        for step in steps {
            let hits = text.matches(step).count();
            assert_eq!(
                hits, 1,
                "{script} 里的锚点 {step:?} 出现 {hits} 次，需要恰好 1 次：这条判据是按位置比先后的，\
                 同一个锚点多一处就量不出顺序"
            );
            let at = text.find(step).unwrap();
            if previous != usize::MAX {
                assert!(
                    at > previous,
                    "{script} 的 {step:?} 不再排在上一锚点之后：{interpreter} 必须先确认 pip 可用，再进入 PEP 517 wheel 构建"
                );
            }
            previous = at;
        }
    }

    let readme = workspace_source("README.md");
    assert!(
        readme.contains(STAGING_PROMISE),
        "README 不再说明「{STAGING_PROMISE}」：稳定 ABI wheel 的扩展来源和构建器边界没有被说清楚"
    );

    let project = workspace_source("python/pyproject.toml");
    for contract in [
        "build-backend = \"maturin\"",
        "manifest-path = \"../crates/qx-python/Cargo.toml\"",
        "python-source = \".\"",
        "module-name = \"qianxing_bridge._qianxing_native\"",
    ] {
        assert!(
            project.contains(contract),
            "Python wheel contract lacks {contract:?}"
        );
    }

    // 双向点名：判据也要真的写进被核对的两份脚本与那份文档，换一份没人核对的文件就等于删判据。
    let judge = workspace_source("crates/qx-cli/tests/unit/artifact_identity_doc.rs");
    for script in [
        "tools/build_python_wheel.ps1",
        "tools/build_python_wheel.sh",
    ] {
        assert!(
            judge.contains(script),
            "判据侧不再点名 {script}：把 wheel 打包入口换成另一份没人核对的脚本，这条判据就空转了"
        );
    }
    assert!(
        judge.contains(STAGING_PROMISE) && judge.contains("README.md"),
        "判据侧不再点名 README 与那句 stage 承诺，就没人守住「文档说的入口 = 脚本做的入口」"
    );
}
