//! Provider-specific Python worker selection and host-owned environment preparation.

use std::collections::BTreeMap;

fn strategy_session_ttl(variable: &str, timeout_ms: u64) -> Result<u32, String> {
    let session_ttl = std::env::var(variable)
        .unwrap_or_else(|_| "3600".to_string())
        .parse::<u32>()
        .map_err(|_| format!("{variable} must be an integer"))?;
    let minimum_session_ttl = ((timeout_ms.saturating_add(999) / 1000) + 30)
        .max(60)
        .min(u32::MAX as u64) as u32;
    if !(minimum_session_ttl..=86_400).contains(&session_ttl) {
        return Err(format!(
            "{variable} must be from {minimum_session_ttl} through 86400"
        ));
    }
    Ok(session_ttl)
}

fn inherit_provider_sdk_environment(env: &mut BTreeMap<String, String>) {
    // Provider SDKs may rely on platform path and certificate settings. These stay
    // inside the local bridge process and are never copied into the remote command.
    for name in ["PATH", "SYSTEMROOT", "WINDIR", "TEMP", "TMP"] {
        if let Ok(value) = std::env::var(name) {
            env.insert(name.to_string(), value);
        }
    }
}

pub(super) fn configure(
    execution_mode: &str,
    timeout_ms: u64,
    env: &mut BTreeMap<String, String>,
    host_provider_env: &mut BTreeMap<String, String>,
) -> Result<&'static str, String> {
    Ok(match execution_mode {
        "local" => "qianxing_strategy.worker",
        "e2b" => {
            let template = std::env::var("QX_E2B_TEMPLATE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "E2B mode requires host setting QX_E2B_TEMPLATE".to_string())?;
            let api_key = std::env::var("E2B_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| "E2B mode requires host setting E2B_API_KEY".to_string())?;
            let session_ttl = strategy_session_ttl("QX_E2B_SESSION_TTL_SECONDS", timeout_ms)?;
            env.insert("QX_E2B_TEMPLATE".into(), template);
            host_provider_env.insert("E2B_API_KEY".into(), api_key);
            env.insert("QX_E2B_SESSION_TTL_SECONDS".into(), session_ttl.to_string());
            env.insert(
                "QX_STRATEGY_PYTHON_TIMEOUT_MS".into(),
                timeout_ms.to_string(),
            );
            inherit_provider_sdk_environment(env);
            "qianxing_strategy.e2b_worker"
        }
        "llm-sandbox" => {
            let image = std::env::var("QX_LLMSANDBOX_IMAGE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    "llm-sandbox mode requires host setting QX_LLMSANDBOX_IMAGE".to_string()
                })?;
            let docker_host = std::env::var("QX_LLMSANDBOX_DOCKER_HOST")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    "llm-sandbox mode requires TLS remote Docker setting QX_LLMSANDBOX_DOCKER_HOST"
                        .to_string()
                })?;
            let session_ttl =
                strategy_session_ttl("QX_LLMSANDBOX_SESSION_TTL_SECONDS", timeout_ms)?;
            for (name, value) in [
                ("QX_LLMSANDBOX_IMAGE", image),
                ("QX_LLMSANDBOX_SESSION_TTL_SECONDS", session_ttl.to_string()),
                (
                    "QX_LLMSANDBOX_EXPECTED_CPU_COUNT",
                    std::env::var("QX_LLMSANDBOX_EXPECTED_CPU_COUNT")
                        .unwrap_or_else(|_| "2".into()),
                ),
                (
                    "QX_LLMSANDBOX_EXPECTED_MEMORY_MB",
                    std::env::var("QX_LLMSANDBOX_EXPECTED_MEMORY_MB")
                        .unwrap_or_else(|_| "2048".into()),
                ),
                ("QX_STRATEGY_PYTHON_TIMEOUT_MS", timeout_ms.to_string()),
            ] {
                env.insert(name.into(), value);
            }
            host_provider_env.insert("QX_LLMSANDBOX_DOCKER_HOST".into(), docker_host);
            for name in [
                "QX_LLMSANDBOX_DOCKER_CA_CERT",
                "QX_LLMSANDBOX_DOCKER_CLIENT_CERT",
                "QX_LLMSANDBOX_DOCKER_CLIENT_KEY",
            ] {
                if let Ok(value) = std::env::var(name) {
                    if !value.trim().is_empty() {
                        host_provider_env.insert(name.into(), value);
                    }
                }
            }
            inherit_provider_sdk_environment(env);
            "qianxing_strategy.llm_sandbox_worker"
        }
        "microsandbox" => {
            let image = std::env::var("QX_MICROSANDBOX_IMAGE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    "Microsandbox mode requires host setting QX_MICROSANDBOX_IMAGE".to_string()
                })?;
            let backend =
                std::env::var("QX_MICROSANDBOX_BACKEND").unwrap_or_else(|_| "local".to_string());
            if !matches!(backend.as_str(), "cloud" | "local") {
                return Err("QX_MICROSANDBOX_BACKEND must be cloud or local".to_string());
            }
            let profile = std::env::var("QX_MICROSANDBOX_PROFILE")
                .ok()
                .filter(|value| !value.trim().is_empty());
            let api_key = std::env::var("MICROSANDBOX_API_KEY")
                .ok()
                .filter(|value| !value.trim().is_empty());
            if backend == "cloud" && profile.is_none() && api_key.is_none() {
                return Err(
                            "Microsandbox cloud mode requires host setting MICROSANDBOX_API_KEY or QX_MICROSANDBOX_PROFILE".to_string(),
                        );
            }
            let session_ttl =
                strategy_session_ttl("QX_MICROSANDBOX_SESSION_TTL_SECONDS", timeout_ms)?;
            env.insert("QX_MICROSANDBOX_IMAGE".into(), image);
            env.insert("QX_MICROSANDBOX_BACKEND".into(), backend.clone());
            env.insert(
                "QX_MICROSANDBOX_SESSION_TTL_SECONDS".into(),
                session_ttl.to_string(),
            );
            env.insert(
                "QX_STRATEGY_PYTHON_TIMEOUT_MS".into(),
                timeout_ms.to_string(),
            );
            if let Some(profile) = &profile {
                env.insert("QX_MICROSANDBOX_PROFILE".into(), profile.clone());
                for name in ["HOME", "USERPROFILE"] {
                    if let Ok(value) = std::env::var(name) {
                        env.insert(name.into(), value);
                    }
                }
            }
            if let Ok(url) = std::env::var("QX_MICROSANDBOX_URL") {
                if !url.trim().is_empty() {
                    env.insert("QX_MICROSANDBOX_URL".into(), url);
                }
            }
            for name in ["QX_MICROSANDBOX_CPUS", "QX_MICROSANDBOX_MEMORY_MIB"] {
                if let Ok(value) = std::env::var(name) {
                    env.insert(name.into(), value);
                }
            }
            if backend == "cloud" && profile.is_none() {
                if let Some(api_key) = api_key {
                    host_provider_env.insert("MICROSANDBOX_API_KEY".into(), api_key);
                }
            }
            inherit_provider_sdk_environment(env);
            "qianxing_strategy.microsandbox_worker"
        }
        "opensandbox" => {
            let domain = std::env::var("OPEN_SANDBOX_DOMAIN")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    "OpenSandbox mode requires host setting OPEN_SANDBOX_DOMAIN".to_string()
                })?;
            let image = std::env::var("QX_OPENSANDBOX_IMAGE")
                .ok()
                .filter(|value| !value.trim().is_empty())
                .ok_or_else(|| {
                    "OpenSandbox mode requires host setting QX_OPENSANDBOX_IMAGE".to_string()
                })?;
            let session_ttl =
                strategy_session_ttl("QX_OPENSANDBOX_SESSION_TTL_SECONDS", timeout_ms)?;
            env.insert("OPEN_SANDBOX_DOMAIN".into(), domain);
            env.insert("QX_OPENSANDBOX_IMAGE".into(), image);
            env.insert(
                "QX_OPENSANDBOX_SESSION_TTL_SECONDS".into(),
                session_ttl.to_string(),
            );
            env.insert(
                "QX_STRATEGY_PYTHON_TIMEOUT_MS".into(),
                timeout_ms.to_string(),
            );
            if let Ok(api_key) = std::env::var("OPEN_SANDBOX_API_KEY") {
                if !api_key.trim().is_empty() {
                    host_provider_env.insert("OPEN_SANDBOX_API_KEY".into(), api_key);
                }
            }
            for name in ["QX_OPENSANDBOX_CPUS", "QX_OPENSANDBOX_MEMORY_MIB"] {
                if let Ok(value) = std::env::var(name) {
                    env.insert(name.into(), value);
                }
            }
            inherit_provider_sdk_environment(env);
            "qianxing_strategy.opensandbox_worker"
        }
        _ => unreachable!("execution mode was validated above"),
    })
}
