//! 接口文档的「API 认证边界」段、配置面的回环闸门、`ApiService` 的策略装载三处必须一致
//! （V13 R2 第二十三遍 #244）。
//!
//! 立案时的实测（`C:/temp/qx_pass23/s555_before_validate.txt`）：把仓库那份明文模板的
//! `api.bind` 换成 `10.20.30.40:8443` 后 `config validate` 仍回 `[PASS]` —— 旧的判定式只把
//! `environment` 与字面量 `production` 相比，而仓库 18 份明文模板用的是 paper/sandbox/testnet。
//! 同一时刻明文面**不装**访问策略（`api.operators` 为空 ⇒ `ApiService::new` ⇒ `policy: None`），
//! `POST /control/commands` 的档位直接取请求体里的 `permission`。两条合起来是一个无鉴权的
//! 下单入口挂在可路由地址上，而文档当时只承诺「除三个只读端点外都要求已认证 operator」。
//!
//! 本文件把这三处钉在一起：文档里那句报错原文必须就是 `validate()` 吐出的那句（从散文里抠
//! `「…」` 引用，不在这里抄第二份字面量）；每一份明文 runtime 模板的 bind 都得是回环；
//! 同一条自报档位的请求在"没装策略"与"装了策略"两种服务上分别拿到 202 与 403。

use super::*;
use std::net::SocketAddr;

/// 文档与源码共用的那句拒绝语的字头；文档里它以 `「…」` 的形式引用，本文件从散文抠出来比对。
const GUARD_PHRASE_HINT: &str = "回环";

fn deploy_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("..")
        .join("..")
        .join("deploy")
}

fn readme_text() -> String {
    let raw = workspace_source("deploy/README.md");
    // 这份文档是纯 CRLF（#183 的口径），按行取锚点前先折行尾。
    raw.replace("\r\n", "\n")
}

/// 从散文里抠出含 `hint` 的那段 `「…」` 引用；没有则返回空串，让调用方把它当缺失判红。
fn quoted_phrase_containing(hint: &str) -> String {
    let text = readme_text();
    let mut rest = text.as_str();
    while let Some(open) = rest.find('「') {
        let after_open = &rest[open + '「'.len_utf8()..];
        let Some(close) = after_open.find('」') else {
            break;
        };
        let phrase = &after_open[..close];
        if phrase.contains(hint) {
            return phrase.to_string();
        }
        rest = &after_open[close + '」'.len_utf8()..];
    }
    String::new()
}

/// 按 `config validate` 的同一读法装载模板，只替换 `api.bind`，再走生产的那条判定式。
fn validate_template_with_bind(name: &str, bind: &str) -> Result<(), String> {
    let mut config = read_runtime_config(&deploy_root().join(name))?;
    config.api.bind = bind.to_string();
    config.validate()
}

fn is_loopback(bind: &str) -> bool {
    bind.parse::<SocketAddr>()
        .is_ok_and(|address| address.ip().is_loopback())
}

/// 文档那句拒绝语必须是 `validate()` 真的吐出的那句：把 `「…」` 里的原文拿去咬实现。
#[test]
fn the_doc_quotes_the_same_loopback_guard_the_config_plane_enforces() {
    let phrase = quoted_phrase_containing(GUARD_PHRASE_HINT);
    assert!(
        !phrase.is_empty(),
        "接口文档的认证边界段里没有 `「…{GUARD_PHRASE_HINT}…」` 这句引用：判据失去对照物"
    );

    let ok = validate_template_with_bind("qianxing.runtime.example.json", "127.0.0.1:19090");
    assert!(
        matches!(ok, Ok(())),
        "仓库自带的明文模板本该过闸门，实际: {ok:?}"
    );

    let error = validate_template_with_bind("qianxing.runtime.example.json", "10.20.30.40:8443")
        .expect_err("可路由的明文 bind 必须被 validate() 拒绝");
    assert!(
        error.contains(&phrase),
        "文档引用的那句拒绝语与实现报出的不是一句话。文档: 「{phrase}」／实现: {error}"
    );

    // 闸门只管明文：同一份配置换成 mTLS（证书三件套 + 一条 Operator 映射）后仍须能绑可路由地址。
    let doc = readme_text();
    for needle in [
        "api.operators",
        "明文 API",
        "等于调用方自报",
        "transport: \"mtls\"",
        "config validate",
    ] {
        assert!(
            doc.contains(needle),
            "接口文档不再写出认证边界的这一格: {needle}"
        );
    }
    assert!(
        doc.contains("api_transport_auth_boundary_doc.rs"),
        "接口文档点名了这条边界却不再点名核对它的用例：读者按文档找不到判据"
    );
    assert!(
        doc.contains("topology_validation.rs"),
        "接口文档没有点出这条闸门的源码位置，判定式与文档之间断了源码那一头"
    );
}

/// 仓库自带的明文 runtime 模板必须天然过这条闸门：份数与"每份都绑回环"都要钉住。
#[test]
fn every_plaintext_runtime_template_in_the_repo_binds_loopback() {
    let mut plaintext = Vec::new();
    let mut mtls = Vec::new();
    for entry in std::fs::read_dir(deploy_root()).expect("deploy 目录可读") {
        let path = entry.expect("目录项可读").path();
        let name = path
            .file_name()
            .expect("顶层文件有文件名")
            .to_string_lossy()
            .into_owned();
        if !name.starts_with("qianxing.runtime.") || !name.ends_with(".json") {
            continue;
        }
        let config = read_runtime_config(&path)
            .unwrap_or_else(|error| panic!("{name} 按生产读法装载失败: {error}"));
        match config.api.transport {
            ApiTransport::Plaintext => {
                assert!(
                    is_loopback(&config.api.bind),
                    "{name} 是明文 transport 却绑了非回环地址 {}：这份模板会把 #244 的入口\
                     带回读者的默认部署里",
                    config.api.bind
                );
                assert!(
                    config.api.operators.is_empty(),
                    "{name} 明文 transport 却声明了 Operator 证书映射，config validate 会直接拒绝它"
                );
                plaintext.push(name);
            }
            ApiTransport::Mtls => {
                assert!(
                    !config.api.operators.is_empty() && config.api.tls.is_some(),
                    "{name} 是 mTLS 模板却缺 Operator 映射或证书路径"
                );
                mtls.push(name);
            }
        }
    }
    assert_eq!(
        plaintext.len(),
        18,
        "明文 runtime 模板的份数与接口文档写的 18 份不等: {plaintext:?}"
    );
    assert_eq!(
        mtls,
        vec!["qianxing.runtime.production.example.json".to_string()],
        "mTLS 模板的名单变了：文档里「唯一那份生产模板绑 0.0.0.0:8443」这句话需要一起改"
    );
}

/// 策略装不装决定同一条自报档位的请求是 202 还是 403 —— 文档第 2 条的那一半事实。
#[test]
fn without_a_policy_the_command_tier_comes_from_the_request_body() {
    let body = std::fs::read_to_string(deploy_root().join("qianxing.submit-order.example.json"))
        .expect("提交样例可读");
    let value: serde_json::Value = serde_json::from_str(&body).expect("提交样例是合法 JSON");
    assert_eq!(
        value.get("permission").and_then(serde_json::Value::as_str),
        Some("Trading"),
        "样例载荷的档位不再是 Trading：本用例的对照物要一起改"
    );

    // 明文部署的形状：没有 operators ⇒ 没有 policy ⇒ 请求体里的 permission 就是授予档位。
    let open = ApiService::new(ApiState::default());
    let accepted = open.handle("POST", "/control/commands", &body, 7);
    assert_eq!(
        accepted.status, 202,
        "未装策略的服务应把自报档位当授予档位受理（文档第 2 条），实际 {} {}",
        accepted.status, accepted.body
    );

    // mTLS 部署的形状：装了策略而无证书身份，同一条请求只能拿到 403。
    let guarded = ApiService::with_policy(
        ApiState::default(),
        ApiPolicy::new().grant("ops", Permission::Admin),
    );
    let refused = guarded.handle("POST", "/control/commands", &body, 7);
    assert_eq!(
        refused.status, 403,
        "装了策略却拿不到 operator 身份时必须 403，实际: {} {}",
        refused.status, refused.body
    );
    assert!(
        refused.body.contains("authenticated_operator_required"),
        "403 的码名与接口文档那一格不一致，客户端按码名分支会读错: {}",
        refused.body
    );
}
