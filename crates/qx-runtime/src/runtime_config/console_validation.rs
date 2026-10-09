//! 控制台面（`api.console`）的边界校验。
//!
//! 从 `topology_validation.rs` 拆出来：那个文件贴着 500 行的单文件门槛，而控制台面这四格
//! 校验彼此独立、理由也各自成段（回环地址 / 身份在册 / 令牌来源 / 会话 TTL），单独成模块
//! 比挤进 API 传输面那一串里更名副其实。

use super::*;

/// 控制台面不做 mTLS，所以边界只能靠这四格站住；任何一格配错都在启动之前拒绝。
///
/// 四格各挡一种"配了但等于没配"：可路由地址 = 一个无认证的代理入口；空身份 / 空令牌环境
/// 变量名 = 会话层既没有可声称的身份、也没有换会话的凭据；零 TTL = 签一个立刻过期的会话；
/// 有策略时身份不在册 = 每个受保护端点都 403，而配置本身看起来完全正常。
pub(crate) fn console_boundary(
    console: &ConsoleRuntimeConfig,
    operators: &BTreeMap<String, OperatorConfig>,
) -> Result<(), String> {
    let address = console.bind.parse::<SocketAddr>();
    let bind = address.map_err(|error| format!("控制台 bind 非法: {error}"))?;
    // 这一层没有 TLS、没有客户端证书、没有运维审批：绑到可路由地址就是把它交给任何能连上的人。
    if !bind.ip().is_loopback() {
        return Err(format!("控制台只能绑回环地址，实得 {}", console.bind));
    }
    if console.static_dir.trim().is_empty()
        || console.operator.trim().is_empty()
        || console.bootstrap_token_env.trim().is_empty()
    {
        return Err("控制台配置必须含 static_dir、operator 与 bootstrap_token_env".into());
    }
    if console.session_ttl_seconds == Some(0) {
        return Err("控制台 session_ttl_seconds 必须为正数".into());
    }
    // 有策略时身份必须在册，否则控制台替每个请求声称一个策略不认识的身份，受保护端点全 403。
    let known = operators.contains_key(&console.operator);
    if !operators.is_empty() && !known {
        return Err(format!(
            "控制台身份 {} 不在 api.operators",
            console.operator
        ));
    }
    Ok(())
}
