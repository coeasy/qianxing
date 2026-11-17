//! Host-owned policy for local and sandboxed Python strategy execution.

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct SandboxExecutionPolicy {
    provider: StrategySandboxProvider,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum StrategySandboxProvider {
    Local,
    E2b,
    LlmSandbox,
    Microsandbox,
    OpenSandbox,
}

impl SandboxExecutionPolicy {
    pub(crate) fn from_env() -> Result<Self, String> {
        let provider = std::env::var("QX_PYTHON_STRATEGY_EXECUTION_MODE").ok();
        let required = std::env::var("QX_PYTHON_STRATEGY_SANDBOX_REQUIRED").ok();
        Self::parse(provider.as_deref(), required.as_deref())
    }

    fn parse(provider: Option<&str>, sandbox_required: Option<&str>) -> Result<Self, String> {
        let provider = match provider.unwrap_or("local") {
            "local" => StrategySandboxProvider::Local,
            "e2b" => StrategySandboxProvider::E2b,
            "llm-sandbox" | "llm_sandbox" => StrategySandboxProvider::LlmSandbox,
            "microsandbox" => StrategySandboxProvider::Microsandbox,
            "opensandbox" => StrategySandboxProvider::OpenSandbox,
            value => {
                return Err(format!(
                    "unsupported QX_PYTHON_STRATEGY_EXECUTION_MODE={value:?}; expected local, e2b, llm-sandbox, microsandbox, or opensandbox"
                ));
            }
        };
        let sandbox_required = parse_bool("QX_PYTHON_STRATEGY_SANDBOX_REQUIRED", sandbox_required)?;
        if sandbox_required && provider == StrategySandboxProvider::Local {
            return Err(
                "QX_PYTHON_STRATEGY_SANDBOX_REQUIRED is enabled, but the strategy execution provider is local or unset; configure e2b, llm-sandbox, microsandbox, or opensandbox"
                    .into(),
            );
        }
        Ok(Self { provider })
    }

    pub(crate) fn provider(&self) -> &'static str {
        match self.provider {
            StrategySandboxProvider::Local => "local",
            StrategySandboxProvider::E2b => "e2b",
            StrategySandboxProvider::LlmSandbox => "llm-sandbox",
            StrategySandboxProvider::Microsandbox => "microsandbox",
            StrategySandboxProvider::OpenSandbox => "opensandbox",
        }
    }

    pub(crate) fn sandbox_enabled(&self) -> bool {
        self.provider != StrategySandboxProvider::Local
    }
}

fn parse_bool(variable: &str, value: Option<&str>) -> Result<bool, String> {
    match value.map(str::trim).map(str::to_ascii_lowercase).as_deref() {
        None | Some("false") | Some("0") | Some("no") => Ok(false),
        Some("true") | Some("1") | Some("yes") => Ok(true),
        Some(value) => Err(format!(
            "{variable} must be one of true/false, 1/0, or yes/no; got {value:?}"
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{SandboxExecutionPolicy, StrategySandboxProvider};

    #[test]
    fn local_is_the_default_unless_sandbox_is_required() {
        let policy = SandboxExecutionPolicy::parse(None, None).unwrap();
        assert_eq!(policy.provider, StrategySandboxProvider::Local);
        assert!(!policy.sandbox_enabled());

        let error = SandboxExecutionPolicy::parse(None, Some("true")).unwrap_err();
        assert!(error.contains("provider is local or unset"));
    }

    #[test]
    fn sandbox_required_rejects_explicit_local_provider() {
        let error = SandboxExecutionPolicy::parse(Some("local"), Some("1")).unwrap_err();
        assert!(error.contains("provider is local or unset"));
    }

    #[test]
    fn sandbox_required_accepts_explicit_remote_provider() {
        for provider in ["e2b", "llm-sandbox", "microsandbox", "opensandbox"] {
            let policy = SandboxExecutionPolicy::parse(Some(provider), Some("YES")).unwrap();
            assert_eq!(policy.provider(), provider);
            assert!(policy.sandbox_enabled());
        }
    }

    #[test]
    fn invalid_provider_and_requirement_values_fail_closed() {
        assert!(SandboxExecutionPolicy::parse(Some("unknown"), None).is_err());
        assert!(SandboxExecutionPolicy::parse(Some("e2b"), Some("sometimes")).is_err());
    }
}
