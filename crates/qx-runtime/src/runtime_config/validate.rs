//! 运行时配置读写、指纹锁定与注释键剥离入口。

use super::*;

impl RuntimeConfig {
    pub fn from_json(payload: &str) -> Result<Self, String> {
        let payload = strip_config_comments(payload)?;
        let config: Self = serde_json::from_str(&payload)
            .map_err(|error| format!("运行时配置 JSON 无效: {error}"))?;
        config.validate()?;
        Ok(config)
    }

    pub fn to_json(&self) -> Result<String, String> {
        self.validate()?;
        serde_json::to_string_pretty(self).map_err(|error| format!("运行时配置编码失败: {error}"))
    }

    /// 计算规范化运行时配置指纹；指纹字段自身被清空后再编码，保证发布值
    /// 可以稳定地写回同一份 JSON，并覆盖策略、worker、凭据引用、存储和 API
    /// 等全部运行时参数。
    pub fn fingerprint(&self) -> Result<String, String> {
        let mut canonical = self.clone();
        canonical.config_fingerprint = None;
        let payload = serde_json::to_vec(&canonical)
            .map_err(|error| format!("运行时配置指纹编码失败: {error}"))?;
        Ok(qx_strategy::sha256_hex(&payload))
    }

    pub fn verify_fingerprint(&self) -> Result<(), String> {
        let Some(expected) = self.config_fingerprint.as_deref() else {
            return Ok(());
        };
        if expected.trim().is_empty() {
            return Err("config_fingerprint 不能为空字符串".into());
        }
        let actual = self.fingerprint()?;
        if expected != actual {
            return Err(format!(
                "运行时配置指纹不匹配: expected={expected} actual={actual}"
            ));
        }
        Ok(())
    }

    pub fn strategy_for_worker(&self, worker_id: &str) -> Result<StrategyRuntimeConfig, String> {
        if self.strategies.is_empty() {
            return Ok(self.strategy.clone());
        }
        self.strategies
            .iter()
            .find(|strategy| strategy.id.as_deref() == Some(worker_id))
            .cloned()
            .ok_or_else(|| format!("没有为 Strategy worker {worker_id} 配置策略实例"))
    }
}

/// 运行时配置允许以 `_` 开头的注释键承载运维说明；它们在结构体反序列化前被
/// 递归剥离，因此既不会进入 `config_fingerprint`，也不会因为开启
/// `deny_unknown_fields` 而报错。除此之外的未知键一律失败——拼错一个风控字段
/// 不能静默退化成“该字段没有配置”。
fn strip_config_comments(payload: &str) -> Result<String, String> {
    fn prune(value: &mut serde_json::Value) {
        match value {
            serde_json::Value::Object(entries) => {
                entries.retain(|key, _| !key.starts_with('_'));
                for (_, child) in entries.iter_mut() {
                    prune(child);
                }
            }
            serde_json::Value::Array(items) => {
                for item in items {
                    prune(item);
                }
            }
            _ => {}
        }
    }
    let mut value: serde_json::Value =
        serde_json::from_str(payload).map_err(|error| format!("运行时配置 JSON 无效: {error}"))?;
    prune(&mut value);
    serde_json::to_string(&value).map_err(|error| format!("运行时配置 JSON 规范化失败: {error}"))
}
