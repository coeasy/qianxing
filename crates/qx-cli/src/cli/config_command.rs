//! Runtime config command dispatch and validation output.

use super::*;

pub(super) fn dispatch(action: Option<ConfigCommand>) {
    let validate_path = match &action {
        Some(ConfigCommand::Validate { path }) => path.clone(),
        _ => None,
    };
    let result = match action {
        Some(ConfigCommand::Explain { path, json }) => {
            run_config_explain(&path.unwrap_or_else(default_runtime_path), json)
        }
        Some(ConfigCommand::Validate { .. }) | None => {
            match read_runtime_config(&validate_path.clone().unwrap_or_else(default_runtime_path)) {
                Ok(config) => {
                    let path = validate_path.unwrap_or_else(default_runtime_path);
                    let (failures, warnings) = validate_runtime_references(&path, &config);
                    for warning in warnings {
                        println!("[WARN] {warning}");
                    }
                    if failures.is_empty() {
                        println!("[PASS] config validate 通过: {}", path.display());
                        Ok(())
                    } else {
                        for failure in &failures {
                            eprintln!("[FAIL] {failure}");
                        }
                        Err(format!("配置引用校验失败，共 {} 项", failures.len()))
                    }
                }
                Err(error) => Err(error),
            }
        }
        Some(ConfigCommand::Fingerprint { path }) => {
            run_config_fingerprint(&path.unwrap_or_else(default_runtime_path))
        }
        Some(ConfigCommand::Lock {
            path,
            output,
            force,
        }) => {
            let path = path.unwrap_or_else(default_runtime_path);
            let output = output.unwrap_or_else(|| {
                let stem = path
                    .file_stem()
                    .and_then(|value| value.to_str())
                    .unwrap_or("qianxing.runtime");
                path.with_file_name(format!("{stem}.locked.json"))
            });
            run_config_lock(&path, &output, force)
        }
    };
    if let Err(error) = result {
        eprintln!("[qx-cli · CLI] 配置命令失败: {error}");
        std::process::exit(2);
    }
}
