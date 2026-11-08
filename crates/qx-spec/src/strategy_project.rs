//! Versioned project manifest for local and server Strategy workflows.

use serde::{Deserialize, Serialize};

pub const STRATEGY_PROJECT_SCHEMA_VERSION: u32 = 1;
pub const STRATEGY_PROJECT_KIND: &str = "qianxing-strategy-project";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyProjectManifestV1 {
    pub schema_version: u32,
    pub kind: String,
    pub project_id: String,
    pub strategy_file: Option<String>,
    pub input_spec: Option<String>,
    #[serde(default)]
    pub strategy_id: Option<String>,
    #[serde(default)]
    pub strategy_version: Option<String>,
    #[serde(default)]
    pub runtime_config: Option<String>,
    #[serde(default)]
    pub worker_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub data: Option<StrategyProjectDataV1>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StrategyProjectDataV1 {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cache_root: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retries: Option<u8>,
}

impl StrategyProjectDataV1 {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value) in [
            ("source", self.source.as_deref()),
            ("cache_root", self.cache_root.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(format!("data.{name} must be non-empty text or null"));
            }
        }
        if self.retries.is_some_and(|retries| retries > 9) {
            return Err("data.retries must be an integer in 0..9 or null".into());
        }
        Ok(())
    }
}

impl StrategyProjectManifestV1 {
    pub fn from_json(payload: &str) -> Result<Self, String> {
        let value: serde_json::Value = serde_json::from_str(payload)
            .map_err(|error| format!("strategy project manifest JSON is invalid: {error}"))?;
        let object = value
            .as_object()
            .ok_or_else(|| "strategy project manifest must be a JSON object".to_string())?;
        for field in [
            "schema_version",
            "kind",
            "project_id",
            "strategy_file",
            "input_spec",
        ] {
            if !object.contains_key(field) {
                return Err(format!(
                    "strategy project manifest is missing required field {field}"
                ));
            }
        }
        let manifest: Self = serde_json::from_value(value)
            .map_err(|error| format!("strategy project manifest fields are invalid: {error}"))?;
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> Result<(), String> {
        if self.schema_version != STRATEGY_PROJECT_SCHEMA_VERSION {
            return Err(format!(
                "unsupported strategy project schema_version {}; expected {}",
                self.schema_version, STRATEGY_PROJECT_SCHEMA_VERSION
            ));
        }
        if self.kind != STRATEGY_PROJECT_KIND {
            return Err(format!(
                "unsupported strategy project kind {}; expected {}",
                self.kind, STRATEGY_PROJECT_KIND
            ));
        }
        if !valid_project_id(&self.project_id) {
            return Err(
                "project_id must be 1..128 ASCII letters, digits, dot, underscore, or hyphen, and start with a letter or digit".into(),
            );
        }
        for (name, value) in [
            ("strategy_file", self.strategy_file.as_deref()),
            ("input_spec", self.input_spec.as_deref()),
            ("strategy_id", self.strategy_id.as_deref()),
            ("strategy_version", self.strategy_version.as_deref()),
            ("runtime_config", self.runtime_config.as_deref()),
            ("worker_id", self.worker_id.as_deref()),
        ] {
            if value.is_some_and(|value| value.trim().is_empty()) {
                return Err(format!("{name} must be non-empty text or null"));
            }
        }
        if let Some(data) = &self.data {
            data.validate()?;
        }
        Ok(())
    }
}

fn valid_project_id(value: &str) -> bool {
    let bytes = value.as_bytes();
    if bytes.is_empty() || bytes.len() > 128 || !bytes[0].is_ascii_alphanumeric() {
        return false;
    }
    bytes
        .iter()
        .all(|byte| byte.is_ascii_alphanumeric() || matches!(*byte, b'.' | b'_' | b'-'))
}

#[cfg(test)]
mod tests {
    use crate::{StrategyProjectDataV1, StrategyProjectManifestV1};
    use serde_json::json;

    #[test]
    fn strategy_project_manifest_contract() {
        let mut value = json!({
            "schema_version": 1,
            "kind": "qianxing-strategy-project",
            "project_id": "demo",
            "strategy_file": "strategy.py",
            "input_spec": null
        });
        let manifest = StrategyProjectManifestV1::from_json(&value.to_string()).unwrap();
        assert_eq!(manifest.data, None);
        assert!(serde_json::to_value(&manifest)
            .unwrap()
            .get("data")
            .is_none());

        for retries in [0, 3, 9] {
            value["data"] = json!({
                "source": "custom-provider",
                "cache_root": "data/cache",
                "retries": retries
            });
            let manifest = StrategyProjectManifestV1::from_json(&value.to_string()).unwrap();
            assert_eq!(
                manifest.data,
                Some(StrategyProjectDataV1 {
                    source: Some("custom-provider".into()),
                    cache_root: Some("data/cache".into()),
                    retries: Some(retries),
                })
            );
            let serialized = serde_json::to_value(&manifest).unwrap();
            assert_eq!(serialized["data"], value["data"]);
            assert_eq!(
                StrategyProjectManifestV1::from_json(&serialized.to_string()).unwrap(),
                manifest
            );
        }

        for data in [
            json!({}),
            json!({"source": "custom-provider"}),
            json!({"retries": 0}),
        ] {
            value["data"] = data.clone();
            let manifest = StrategyProjectManifestV1::from_json(&value.to_string()).unwrap();
            assert_eq!(serde_json::to_value(&manifest).unwrap()["data"], data);
        }
        value["data"] = json!({"source": null, "cache_root": null, "retries": null});
        let manifest = StrategyProjectManifestV1::from_json(&value.to_string()).unwrap();
        assert_eq!(
            manifest.data,
            Some(StrategyProjectDataV1 {
                source: None,
                cache_root: None,
                retries: None
            })
        );
        value["data"] = json!(null);
        let manifest = StrategyProjectManifestV1::from_json(&value.to_string()).unwrap();
        assert_eq!(manifest.data, None);
        assert!(serde_json::to_value(&manifest)
            .unwrap()
            .get("data")
            .is_none());

        value["data"] = json!({"unknown": true});
        let error = StrategyProjectManifestV1::from_json(&value.to_string()).unwrap_err();
        assert!(error.contains("unknown field"), "{error}");
        for field in ["source", "cache_root"] {
            for text in ["", " \t\n"] {
                value["data"] = json!({});
                value["data"][field] = json!(text);
                let error = StrategyProjectManifestV1::from_json(&value.to_string()).unwrap_err();
                assert!(error.contains(&format!("data.{field}")), "{error}");
            }
        }
        value["data"] = json!({"retries": 10});
        let error = StrategyProjectManifestV1::from_json(&value.to_string()).unwrap_err();
        assert!(error.contains("data.retries"), "{error}");
        for retries in [json!(-1), json!(256), json!(1.5), json!("3"), json!(true)] {
            value["data"] = json!({"retries": retries});
            assert!(StrategyProjectManifestV1::from_json(&value.to_string()).is_err());
        }
    }
}
