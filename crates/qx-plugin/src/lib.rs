//! # qx-plugin — 卯眼 / 榫头
//!
//! 插件体系：**插件只向扩展点贡献，不修改内核**。
//! - 卯眼（Mortise）= 扩展点 Extension Point
//! - 榫头（Tenon）  = 插件贡献 Contribution
//!
//! 三条硬规则：
//! 1. **禁止隐式"最后加载者胜"**：独占扩展点冲突必须显式用 `replaces` 解决。
//! 2. **依赖决定顺序，不是书写顺序**：由 `requires` 拓扑求解。
//! 3. **"插件一次、运行静态"**：本模块只负责启动期装配，不提供运行期热替换。
//!
//! 扩展点名单只保留内核真的会分派的两格（`Matcher`/`FeeModel`，见 `qx-cli selfcheck`
//! 的装配）；其余名字此前既没有贡献者也没有分派者，留在字典里会让"声明了扩展点"
//! 被误读成"接上了插件"（V12 §16）。同理，`Profile/Bundle/Patch` 组合器与五阶段
//! `bootstrap_plan` 在仓内没有任何装配读者，已连同其声明一并删除——启动顺序目前
//! 由 `Registry::resolve_order` 的依赖拓扑单点决定。

use qx_core::Fnv1a;
use ring::signature::{Ed25519KeyPair, KeyPair, UnparsedPublicKey, ED25519};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, BTreeSet};

pub const POINT_MATCHER: &str = "Matcher";
pub const POINT_FEE_MODEL: &str = "FeeModel";

#[derive(Clone, Copy, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub enum Cardinality {
    /// 独占：只允许一个生效贡献者。
    Exclusive,
    /// 多贡献者：按顺序组成规则链。
    Multi,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Provides {
    pub point: String,
    pub cardinality: Cardinality,
    /// 同扩展点内排序依据；**不能替代显式 replaces**。
    pub priority: i32,
}

#[derive(Clone, PartialEq, Eq, Debug, Serialize, Deserialize)]
pub struct Manifest {
    pub id: String,
    pub version: String,
    pub kind: String,
    pub provides: Vec<Provides>,
    pub requires: Vec<String>,
    /// 显式声明替换了谁——解决独占冲突的唯一合法方式。
    pub replaces: Vec<String>,
    pub capabilities: Vec<String>,
    pub permissions: Vec<String>,
    pub healthcheck_timeout_ms: u64,
    pub shutdown_timeout_ms: u64,
    /// 启动期配置的 JSON Schema；插件不得以未校验字典接收配置。
    pub config_schema: String,
    /// 不含 `manifest_hash` 与 `signature` 字段的稳定 FNV-1a 摘要。
    pub manifest_hash: u64,
    pub signature: Option<String>,
}

#[derive(Clone, PartialEq, Eq, Debug)]
pub enum PluginError {
    InvalidManifest(String),
    Conflict(String),
    MissingDependency { plugin: String, dep: String },
    Cycle,
}

impl std::fmt::Display for PluginError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            PluginError::InvalidManifest(s) => write!(f, "manifest 非法: {}", s),
            PluginError::Conflict(s) => write!(f, "扩展点冲突: {}", s),
            PluginError::MissingDependency { plugin, dep } => {
                write!(f, "插件 {} 缺少依赖 {}", plugin, dep)
            }
            PluginError::Cycle => write!(f, "依赖图存在环"),
        }
    }
}

impl Manifest {
    pub fn validate(&self) -> Result<(), PluginError> {
        if self.id.trim().is_empty() {
            return Err(PluginError::InvalidManifest("id 为空".into()));
        }
        if self.version.trim().is_empty() {
            return Err(PluginError::InvalidManifest("version 为空".into()));
        }
        if self.kind.trim().is_empty() {
            return Err(PluginError::InvalidManifest("kind 为空".into()));
        }
        if self.requires.contains(&self.id) {
            return Err(PluginError::InvalidManifest("不能依赖自身".into()));
        }
        if self.replaces.contains(&self.id) {
            return Err(PluginError::InvalidManifest("不能替换自身".into()));
        }
        if self.healthcheck_timeout_ms == 0 || self.shutdown_timeout_ms == 0 {
            return Err(PluginError::InvalidManifest("生命周期超时必须大于0".into()));
        }
        if self.config_schema.trim().is_empty() {
            return Err(PluginError::InvalidManifest(
                "config_schema 不能为空".into(),
            ));
        }
        serde_json::from_str::<serde_json::Value>(&self.config_schema).map_err(|error| {
            PluginError::InvalidManifest(format!("config_schema 不是合法 JSON: {error}"))
        })?;
        if self.manifest_hash == 0 || self.manifest_hash != self.canonical_hash() {
            return Err(PluginError::InvalidManifest(
                "manifest_hash 与规范化 manifest 不一致".into(),
            ));
        }
        if let Some(signature) = &self.signature {
            if let Some(expected) = signature.strip_prefix("fnv1a:") {
                if expected != format!("{:016x}", self.canonical_hash()) {
                    return Err(PluginError::InvalidManifest(
                        "插件 FNV 签名与 manifest 摘要不一致".into(),
                    ));
                }
            } else if let Some(value) = signature.strip_prefix("ed25519:") {
                let mut parts = value.split(':');
                let public_key = decode_hex(parts.next().unwrap_or_default())?;
                let signature = decode_hex(parts.next().unwrap_or_default())?;
                if parts.next().is_some() || public_key.len() != 32 || signature.len() != 64 {
                    return Err(PluginError::InvalidManifest("Ed25519 签名格式非法".into()));
                }
                UnparsedPublicKey::new(&ED25519, public_key)
                    .verify(signature_message(self).as_bytes(), &signature)
                    .map_err(|_| PluginError::InvalidManifest("Ed25519 签名校验失败".into()))?;
            } else {
                return Err(PluginError::InvalidManifest("不支持的插件签名类型".into()));
            }
        }
        Ok(())
    }

    /// 计算不包含可变校验字段的稳定摘要。
    pub fn canonical_hash(&self) -> u64 {
        let mut hash = Fnv1a::new();
        hash.write_text(&self.id);
        hash.write_text(&self.version);
        hash.write_text(&self.kind);
        hash.write_u64(self.provides.len() as u64);
        for provide in &self.provides {
            hash.write_text(&provide.point);
            hash.write_u64(match provide.cardinality {
                Cardinality::Exclusive => 0,
                Cardinality::Multi => 1,
            });
            hash.write_i128(i128::from(provide.priority));
        }
        hash.write_u64(self.requires.len() as u64);
        for value in &self.requires {
            hash.write_text(value);
        }
        hash.write_u64(self.replaces.len() as u64);
        for value in &self.replaces {
            hash.write_text(value);
        }
        hash.write_u64(self.capabilities.len() as u64);
        for value in &self.capabilities {
            hash.write_text(value);
        }
        hash.write_u64(self.permissions.len() as u64);
        for value in &self.permissions {
            hash.write_text(value);
        }
        hash.write_u64(self.healthcheck_timeout_ms);
        hash.write_u64(self.shutdown_timeout_ms);
        hash.write_text(&self.config_schema);
        hash.finish()
    }

    /// 为启动期 manifest 生成稳定 hash；签名仍可由受信任发布流程追加。
    pub fn seal(mut self) -> Self {
        self.manifest_hash = self.canonical_hash();
        self
    }

    /// 使用内置摘要签名，适合本地开发和 golden 测试；生产环境应替换为公钥签名。
    pub fn sign(mut self) -> Self {
        self.manifest_hash = self.canonical_hash();
        self.signature = Some(format!("fnv1a:{:016x}", self.manifest_hash));
        self
    }

    /// 使用 Ed25519 私钥种子生成可离线验证的发布签名。
    /// 私钥只接受调用方传入的 32 字节种子，不会写入 manifest。
    pub fn sign_ed25519(mut self, seed: &[u8; 32]) -> Result<Self, PluginError> {
        self.manifest_hash = self.canonical_hash();
        let key_pair = Ed25519KeyPair::from_seed_unchecked(seed)
            .map_err(|_| PluginError::InvalidManifest("Ed25519 私钥种子非法".into()))?;
        self.signature = Some(format!(
            "ed25519:{}:{}",
            encode_hex(key_pair.public_key().as_ref()),
            encode_hex(key_pair.sign(signature_message(&self).as_bytes()).as_ref())
        ));
        Ok(self)
    }

    pub fn to_json(&self) -> Result<String, PluginError> {
        self.validate()?;
        serde_json::to_string(self).map_err(|error| {
            PluginError::InvalidManifest(format!("manifest JSON 序列化失败: {error}"))
        })
    }

    pub fn from_json(input: &str) -> Result<Self, PluginError> {
        let manifest: Self = serde_json::from_str(input).map_err(|error| {
            PluginError::InvalidManifest(format!("manifest JSON 无法解析: {error}"))
        })?;
        manifest.validate()?;
        Ok(manifest)
    }
}

fn signature_message(manifest: &Manifest) -> String {
    format!("qx-plugin-manifest-v1:{:016x}", manifest.canonical_hash())
}

fn encode_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn decode_hex(value: &str) -> Result<Vec<u8>, PluginError> {
    if !value.len().is_multiple_of(2) {
        return Err(PluginError::InvalidManifest("签名十六进制长度非法".into()));
    }
    (0..value.len())
        .step_by(2)
        .map(|index| {
            u8::from_str_radix(&value[index..index + 2], 16)
                .map_err(|_| PluginError::InvalidManifest("签名不是合法十六进制".into()))
        })
        .collect()
}

#[derive(Default)]
pub struct Registry {
    entries: BTreeMap<String, Manifest>,
}

impl Registry {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register(&mut self, m: Manifest) -> Result<(), PluginError> {
        m.validate()?;
        if self.entries.contains_key(&m.id) {
            return Err(PluginError::InvalidManifest(format!(
                "插件 id 重复: {}",
                m.id
            )));
        }
        self.entries.insert(m.id.clone(), m);
        Ok(())
    }

    pub fn get(&self, id: &str) -> Option<&Manifest> {
        self.entries.get(id)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// 独占扩展点冲突检测：被 `replaces` 显式替换的不算冲突。
    pub fn conflicts(&self) -> Vec<String> {
        let mut by_point: BTreeMap<String, Vec<&Manifest>> = BTreeMap::new();
        for m in self.entries.values() {
            for p in &m.provides {
                if p.cardinality == Cardinality::Exclusive {
                    by_point.entry(p.point.clone()).or_default().push(m);
                }
            }
        }

        let mut out = Vec::new();
        for (point, providers) in by_point {
            if providers.len() <= 1 {
                continue;
            }
            let replaced: BTreeSet<String> = providers
                .iter()
                .flat_map(|m| m.replaces.iter().cloned())
                .collect();
            let effective: Vec<&str> = providers
                .iter()
                .filter(|m| !replaced.contains(&m.id))
                .map(|m| m.id.as_str())
                .collect();
            if effective.len() > 1 {
                out.push(format!("{} <- {:?}", point, effective));
            }
        }
        out
    }

    /// 依赖拓扑排序（Kahn）。同层按 id 字典序——**顺序必须确定**。
    pub fn resolve_order(&self) -> Result<Vec<String>, PluginError> {
        let ids: Vec<String> = self.entries.keys().cloned().collect();
        let mut indeg: BTreeMap<String, usize> = ids.iter().map(|i| (i.clone(), 0)).collect();
        let mut adj: BTreeMap<String, Vec<String>> =
            ids.iter().map(|i| (i.clone(), Vec::new())).collect();

        for (id, m) in &self.entries {
            for dep in &m.requires {
                if !self.entries.contains_key(dep) {
                    return Err(PluginError::MissingDependency {
                        plugin: id.clone(),
                        dep: dep.clone(),
                    });
                }
                adj.get_mut(dep).unwrap().push(id.clone());
                *indeg.get_mut(id).unwrap() += 1;
            }
        }

        let mut ready: BTreeSet<String> = indeg
            .iter()
            .filter(|(_, &d)| d == 0)
            .map(|(k, _)| k.clone())
            .collect();
        let mut out = Vec::new();

        while let Some(n) = ready.iter().next().cloned() {
            ready.remove(&n);
            out.push(n.clone());
            if let Some(nexts) = adj.get(&n) {
                for nx in nexts {
                    let d = indeg.get_mut(nx).unwrap();
                    *d -= 1;
                    if *d == 0 {
                        ready.insert(nx.clone());
                    }
                }
            }
        }

        if out.len() != ids.len() {
            return Err(PluginError::Cycle);
        }
        Ok(out)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn m(id: &str, requires: Vec<&str>) -> Manifest {
        Manifest {
            id: id.into(),
            version: "0.1.0".into(),
            kind: "domain-mod".into(),
            provides: vec![Provides {
                point: POINT_FEE_MODEL.into(),
                cardinality: Cardinality::Exclusive,
                priority: 100,
            }],
            requires: requires.into_iter().map(|s| s.to_string()).collect(),
            replaces: vec![],
            capabilities: vec![],
            permissions: vec![],
            healthcheck_timeout_ms: 1000,
            shutdown_timeout_ms: 1000,
            config_schema: "{}".into(),
            manifest_hash: 0,
            signature: None,
        }
        .seal()
    }

    #[test]
    fn dependency_decides_order_not_writing_order() {
        let mut r = Registry::new();
        // 故意先注册被依赖方之外的插件
        r.register(m("b", vec!["a"])).unwrap();
        r.register(m("a", vec![])).unwrap();
        let order = r.resolve_order().unwrap();
        assert_eq!(order, vec!["a".to_string(), "b".to_string()]);
    }

    #[test]
    fn missing_dependency_is_error_not_silence() {
        let mut r = Registry::new();
        r.register(m("b", vec!["nope"])).unwrap();
        assert!(matches!(
            r.resolve_order(),
            Err(PluginError::MissingDependency { .. })
        ));
    }

    #[test]
    fn exclusive_conflict_detected() {
        let mut r = Registry::new();
        r.register(m("x", vec![])).unwrap();
        r.register(m("y", vec![])).unwrap();
        let c = r.conflicts();
        assert_eq!(c.len(), 1);
    }

    #[test]
    fn explicit_replaces_resolves_conflict() {
        let mut r = Registry::new();
        r.register(m("x", vec![])).unwrap();
        let mut y = m("y", vec![]);
        y.replaces = vec!["x".into()];
        r.register(y.seal()).unwrap();
        assert!(r.conflicts().is_empty());
    }

    #[test]
    fn duplicate_id_rejected() {
        let mut r = Registry::new();
        r.register(m("x", vec![])).unwrap();
        assert!(r.register(m("x", vec![])).is_err());
    }

    #[test]
    fn manifest_hash_signature_and_json_round_trip_are_verified() {
        let manifest = m("signed", vec![]).sign();
        let json = manifest.to_json().unwrap();
        assert_eq!(Manifest::from_json(&json).unwrap(), manifest);
        let mut tampered = manifest.clone();
        tampered.config_schema = "{\"type\":\"object\"}".into();
        assert!(tampered.validate().is_err());
        let mut bad_signature = manifest;
        bad_signature.signature = Some("fnv1a:0000000000000000".into());
        assert!(bad_signature.validate().is_err());
    }

    #[test]
    fn ed25519_manifest_signature_is_verified_and_tamper_evident() {
        let seed = [7_u8; 32];
        let manifest = m("ed25519", vec![]).sign_ed25519(&seed).unwrap();
        let json = manifest.to_json().unwrap();
        assert_eq!(Manifest::from_json(&json).unwrap(), manifest);
        let mut tampered = manifest;
        tampered.permissions.push("network".into());
        assert!(tampered.validate().is_err());
    }
}
