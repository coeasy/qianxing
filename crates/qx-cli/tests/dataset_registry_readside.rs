//! 数据集登记记录的读侧（V11 F2）：`dataset-ingest` 写下的那份身份必须有人回读。
//!
//! 缺陷原形：ingest 往 `datasets.manifest.json` 写登记记录，`dataset-bundle` 与策略回测链
//! 则各自按眼前的输入文件重算身份，两侧从来没有比过——同一个 `(dataset_id, version)` 可以由
//! 两个工具写成两份内容，而整条链上没有任何一句话说得出"这两件事不是同一件事"。
//! 走真实子进程是因为登记记录落在磁盘上：同进程调用复制不到"上一次运行写下的那条记录"这一半。

use std::ffi::OsStr;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const DATASET_ID: &str = "demo-bars";
const DATASET_VERSION: &str = "v1";

fn deploy(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(|crates| crates.parent())
        .expect("仓库根目录")
        .join("deploy")
        .join(name)
}

fn temp_dir(label: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-dataset-readside-{label}-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).expect("创建用例临时目录失败");
    root
}

fn run<A: AsRef<OsStr>>(args: &[A]) -> (i32, String, String) {
    let output = Command::new(env!("CARGO_BIN_EXE_qx-cli"))
        .args(args)
        .output()
        .expect("启动 qx-cli 失败");
    (
        output.status.code().unwrap_or(-1),
        String::from_utf8_lossy(&output.stdout).to_string(),
        String::from_utf8_lossy(&output.stderr).to_string(),
    )
}

fn data_root(root: &Path) -> PathBuf {
    root.join("data")
}

fn datasets_manifest(root: &Path) -> PathBuf {
    data_root(root).join("datasets.manifest.json")
}

/// 用真实命令把夹具摄进临时数据根，再从磁盘读回那条登记记录（不取内存里的对象）。
fn ingest(root: &Path) -> serde_json::Value {
    let (code, stdout, stderr) = run(&[
        "dataset-ingest".into(),
        deploy("qianxing.bar-frame.example.json")
            .to_string_lossy()
            .into_owned(),
        DATASET_ID.into(),
        DATASET_VERSION.into(),
        data_root(root).to_string_lossy().into_owned(),
    ]);
    assert_eq!(code, 0, "摄取夹具要成功: {stderr}");
    assert!(
        stdout.contains("fingerprint="),
        "ingest 要把登记的指纹印出来: {stdout}"
    );
    let path = datasets_manifest(root);
    let payload = std::fs::read_to_string(&path)
        .unwrap_or_else(|error| panic!("读取登记记录失败 {}: {error}", path.display()));
    let document: serde_json::Value = serde_json::from_str(&payload)
        .unwrap_or_else(|error| panic!("登记记录不是合法 JSON {}: {error}", path.display()));
    document["manifests"]
        .as_array()
        .expect("登记记录里的 manifests 是数组")
        .iter()
        .find(|record| record["dataset_id"] == DATASET_ID && record["version"] == DATASET_VERSION)
        .cloned()
        .unwrap_or_else(|| panic!("{DATASET_ID}@{DATASET_VERSION} 没有被登记: {document}"))
}

/// 只声明数据集身份的 Bundle（不传 bar-frame）：内容核对走不到，读侧必须自己把记录读回来。
fn bundle_file(root: &Path, name: &str, dataset_id: &str, fingerprint: &str) -> PathBuf {
    let path = root.join(format!("{name}.bundle.json"));
    let bundle = serde_json::json!({
        "bundle_id": "demo",
        "version": "v1",
        "source": "fixture",
        "schema_version": 1,
        "components": {
            "bars": {
                "kind": "bars",
                "dataset_id": dataset_id,
                "version": DATASET_VERSION,
                "source": "fixture",
                "fingerprint": fingerprint,
                "schema_version": 1,
                "start_timestamp": 1,
                "end_timestamp": 2,
                "row_count": 2
            }
        }
    });
    std::fs::write(&path, serde_json::to_vec(&bundle).unwrap()).unwrap();
    path
}

fn save_bundle(root: &Path, bundle: &Path) -> (i32, String, String) {
    run(&[
        "dataset-bundle".into(),
        bundle.to_string_lossy().into_owned(),
        data_root(root).to_string_lossy().into_owned(),
    ])
}

/// 登记与声明是同一份内容时，登记记录要真的被读回来并核上。
#[test]
fn an_ingested_record_is_read_back_by_a_bundle_that_declares_it() {
    let root = temp_dir("match");
    let record = ingest(&root);
    let declared = record["fingerprint"].as_str().unwrap().to_string();
    let (code, stdout, stderr) =
        save_bundle(&root, &bundle_file(&root, "match", DATASET_ID, &declared));
    assert_eq!(code, 0, "声明与登记一致时应当保存成功: {stderr}");
    assert!(
        stdout.contains("registry_checked=1/1"),
        "登记的那一份必须被读回来核对，而不是只算自己的: {stdout}"
    );
    assert!(
        !stdout.contains("没有登记记录"),
        "有记录在场时不该报未核对: {stdout}"
    );
}

/// 同一个 `(dataset_id, version)` 两份内容：当场失败，而不是各写各的。
#[test]
fn a_declaration_that_disagrees_with_the_record_fails_closed() {
    let root = temp_dir("mismatch");
    let record = ingest(&root);
    let recorded = record["fingerprint"].as_str().unwrap().to_string();
    assert_ne!(recorded, "deadbeefdeadbeef", "夹具得真登记出一个指纹");
    let (code, stdout, stderr) = save_bundle(
        &root,
        &bundle_file(&root, "mismatch", DATASET_ID, "deadbeefdeadbeef"),
    );
    assert_eq!(code, 2, "同一版本号两份内容必须拒收");
    assert!(
        stderr.contains("与数据集登记记录不符") && stderr.contains(DATASET_ID),
        "报错要点名是哪个组件、哪个数据集: {stderr}"
    );
    assert!(
        stderr.contains(&recorded) && stderr.contains("deadbeefdeadbeef"),
        "两个指纹都要写出来，读者才知道要修哪一侧: {stderr}"
    );
    assert!(
        !stdout.contains("[Data · Bundle] bundle="),
        "失败的一律不该留下已保存的读数: {stdout}"
    );
}

/// 没登记过的组件说成"未核对"：没人读过这条记录，不等于核对过了。
#[test]
fn a_component_with_no_record_is_reported_unchecked() {
    let root = temp_dir("unrecorded");
    let record = ingest(&root);
    let declared = record["fingerprint"].as_str().unwrap().to_string();
    let (code, stdout, stderr) = save_bundle(
        &root,
        &bundle_file(&root, "unrecorded", "never-ingested", &declared),
    );
    assert_eq!(code, 0, "没有登记记录不等于内容有问题: {stderr}");
    assert!(
        stdout.contains("registry_checked=0/1"),
        "一份都没核对时不能报成 1/1: {stdout}"
    );
    let line = stdout
        .lines()
        .find(|line| line.contains("没有登记记录"))
        .unwrap_or_default()
        .to_string();
    assert!(
        line.contains("bars:never-ingested@v1"),
        "未核对的组件要点名到身份: {line}"
    );
}
