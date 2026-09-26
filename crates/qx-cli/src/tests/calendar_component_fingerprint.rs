// V11 R17：日历组件指纹的两侧 canonical 字节共用一份夹具。
//
// Python `AshareTradingCalendar.component_fingerprint` 把摘要登记进 DatasetBundle，Rust
// `dataset_component_file_fingerprint` 在回测启动前对同一份文件重算。此前两份实现各写一遍
// 格式串、全仓没有一条用例比过这两个字节；这里的夹具与摘要文件由 `python/tests/fixtures/`
// 里那对 Python 产物提供，两侧各自算数、比同一个值：任何一侧改了 canonical 字节，就有一侧红。
use super::*;

const CALENDAR_FIXTURE_DIR: &str =
    concat!(env!("CARGO_MANIFEST_DIR"), "/../../python/tests/fixtures");

fn calendar_fixture(name: &str) -> PathBuf {
    Path::new(CALENDAR_FIXTURE_DIR).join(name)
}

/// 夹具文档按 LF 读回：工作树是 CRLF，而 Rust 侧比对的是"换序后的同一份内容"。
fn calendar_fixture_text(name: &str) -> String {
    let text = std::fs::read_to_string(calendar_fixture(name))
        .unwrap_or_else(|error| panic!("跨语言日历夹具 {name} 必须存在: {error}"));
    text.replace("\r\n", "\n")
}

fn calendar_fixture_json(name: &str) -> serde_json::Value {
    serde_json::from_str(&calendar_fixture_text(name))
        .unwrap_or_else(|error| panic!("跨语言日历夹具 {name} 必须是合法 JSON: {error}"))
}

/// 摘要由 Python 写侧算出后落盘，仓库里只此一份：两侧都读它，谁都不抄它。
fn calendar_pinned_digest(name: &str) -> String {
    let digest = calendar_fixture_text(name).trim().to_owned();
    assert!(
        digest.len() == 64
            && digest
                .chars()
                .all(|c| c.is_ascii_hexdigit() && !c.is_uppercase()),
        "{name} 必须是算出来的 64 位小写 sha256，不是手抄的占位: {digest}"
    );
    digest
}

fn temp_calendar_dir(tag: &str) -> PathBuf {
    let root = std::env::temp_dir().join(format!(
        "qianxing-cli-calendar-{}-{}-{}",
        tag,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    std::fs::create_dir_all(&root).unwrap();
    root
}

fn calendar_fingerprint_of(path: &Path) -> Result<(String, u64), String> {
    dataset_commands::dataset_component_file_fingerprint(path, "calendar")
}

/// 两份夹具、各一份摘要：CLI 重算出来的那一格必须等于 Python 登记进 bundle 的那一格。
/// 旧格式那条同时钉住"sessions 缺席读成没有时段"——两侧读侧此前一宽一严。
#[test]
fn calendar_component_fingerprints_match_the_shared_python_digest() {
    for (document, digest_file, trading_days) in [
        (
            "calendar-component-v1.json",
            "calendar-component-v1.fingerprint",
            3,
        ),
        (
            "calendar-component-legacy.json",
            "calendar-component-legacy.fingerprint",
            2,
        ),
    ] {
        let (fingerprint, row_count) = calendar_fingerprint_of(&calendar_fixture(document))
            .unwrap_or_else(|error| {
                panic!("{document} 是 Python 已经登记进 bundle 的日历组件，CLI 必须能重算: {error}")
            });
        assert_eq!(
            fingerprint,
            calendar_pinned_digest(digest_file),
            "{document} 的 canonical 字节与 Python `component_fingerprint` 分叉了：\
             两侧各写一遍的那份格式串又漂了一个字节"
        );
        assert_eq!(
            row_count, trading_days,
            "{document} 的行数必须是 trading_days 的个数——Python 登记的 row_count 就是它"
        );
    }
}

/// 指纹吃的是那三个规范字段的规范字节，不是文件的字节：换键序不能动摘要，动内容必须动摘要。
#[test]
fn calendar_digest_follows_the_canonical_fields_not_the_document_layout() {
    let value = calendar_fixture_json("calendar-component-v1.json");
    let object = value.as_object().expect("日历文档必须是对象");
    let expected = calendar_pinned_digest("calendar-component-v1.fingerprint");
    let root = temp_calendar_dir("layout");

    let reversed: Vec<&String> = object.keys().rev().collect();
    let shuffled = format!(
        "{{{}}}\n",
        reversed
            .iter()
            .map(|name| format!(
                "{}:{}",
                serde_json::to_string(name).unwrap(),
                serde_json::to_string(&object[name.as_str()]).unwrap()
            ))
            .collect::<Vec<String>>()
            .join(",")
    );
    assert_ne!(
        shuffled,
        calendar_fixture_text("calendar-component-v1.json"),
        "键序夹具没有真的换序，这条用例就成了空跑"
    );
    let shuffled_path = root.join("shuffled.json");
    std::fs::write(&shuffled_path, shuffled.as_bytes()).unwrap();
    assert_eq!(
        calendar_fingerprint_of(&shuffled_path).unwrap().0,
        expected,
        "顶层键序不该改变指纹：指纹变了说明这侧哈希的是文件字节而不是规范字节"
    );

    // 三个规范字段逐个改动，摘要必须跟着动；少了这一组，"忘记 sessions"的实现也能全绿。
    for (field, replacement) in [
        ("calendar_id", serde_json::json!("cn-shanghai-other")),
        (
            "trading_days",
            serde_json::json!(["2024-01-02", "2024-01-03", "2024-02-19", "2024-02-20"]),
        ),
        ("sessions", serde_json::json!([])),
    ] {
        let mutated = {
            let mut mutated = value.clone();
            mutated[field] = replacement;
            mutated
        };
        let path = root.join(format!("{field}.json"));
        std::fs::write(&path, format!("{mutated}\n").as_bytes()).unwrap();
        assert_ne!(
            calendar_fingerprint_of(&path).unwrap().0,
            expected,
            "{field} 改了摘要却没变，说明这一列根本没进 canonical 字节"
        );
    }

    // 字段在但形状不对仍然拒：缺席才是"没有时段"，写坏不是。
    let broken = {
        let mut broken = value.clone();
        broken["sessions"] = serde_json::json!("09:30");
        broken
    };
    let broken_path = root.join("broken-sessions.json");
    std::fs::write(&broken_path, format!("{broken}\n").as_bytes()).unwrap();
    assert!(
        calendar_fingerprint_of(&broken_path).is_err(),
        "sessions 是非数组时必须拒收，不能按空时段折算出一个合法指纹"
    );
    let _ = std::fs::remove_dir_all(root);
}

/// 夹具得是回测链真能装载的那份文档，而不只是指纹器认识的形状。
#[test]
fn calendar_fixtures_load_through_the_backtest_rules_registry() {
    for (document, trading_days) in [
        ("calendar-component-v1.json", 3_usize),
        ("calendar-component-legacy.json", 2_usize),
    ] {
        let mut rules = AshareRuleConfig::default();
        assert_eq!(
            rules
                .apply_calendar_json(&calendar_fixture_text(document))
                .unwrap_or_else(|error| {
                    panic!(
                        "{document} 必须能被 A 股规则注册表装载（回测绑定走的同一条路）: {error}"
                    )
                }),
            trading_days,
            "{document} 装出来的交易日数与夹具不一致"
        );
    }
}
