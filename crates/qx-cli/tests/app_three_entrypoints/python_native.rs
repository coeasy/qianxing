use serde_json::Value;
use std::path::Path;
use std::process::Command;

/// Run the Python leg against the newest native extension, not a stale venv copy.
pub(super) fn via_python(
    root: &Path,
    function: &str,
    argument: &str,
) -> Option<Result<Value, String>> {
    let python = std::env::var("QX_PYTHON").unwrap_or_else(|_| {
        if cfg!(windows) {
            root.join("python/.venv/Scripts/python.exe")
                .to_string_lossy()
                .into_owned()
        } else {
            root.join("python/.venv/bin/python")
                .to_string_lossy()
                .into_owned()
        }
    });
    if Command::new(&python).arg("--version").output().is_err() {
        return None;
    }

    let mut candidates = Vec::new();
    for relative in ["target/release", "target/debug", "python/qianxing_bridge"] {
        if let Ok(entries) = std::fs::read_dir(root.join(relative)) {
            candidates.extend(entries.flatten().filter_map(|entry| {
                let path = entry.path();
                let is_extension = matches!(
                    path.extension().and_then(|value| value.to_str()),
                    Some("pyd" | "dll" | "so" | "dylib")
                );
                (is_extension
                    && path.file_stem().and_then(|value| value.to_str())
                        == Some("_qianxing_native"))
                .then_some(path)
            }));
        }
    }
    candidates.sort_by_key(|path| {
        std::fs::metadata(path)
            .and_then(|metadata| metadata.modified())
            .unwrap_or(std::time::UNIX_EPOCH)
    });
    let newest = candidates.pop()?;
    let import_dir = std::env::temp_dir().join(format!("qx-app-g1-native-{}", std::process::id()));
    std::fs::create_dir_all(&import_dir).ok()?;
    let suffix = if cfg!(windows) { "pyd" } else { "so" };
    std::fs::copy(
        newest,
        import_dir.join(format!("_qianxing_native.{suffix}")),
    )
    .ok()?;

    let script = format!(
        "import json, sys\n\
         sys.path.insert(0, {python_dir:?})\n\
         sys.path.insert(0, {native_dir:?})\n\
         try:\n\
         \x20   from qianxing_bridge import app, native\n\
         except Exception as error:\n\
         \x20   print('QX_APP_PYTHON_UNAVAILABLE: ' + str(error), file=sys.stderr)\n\
         \x20   sys.exit(3)\n\
         if not native.app_available():\n\
         \x20   print('QX_APP_PYTHON_UNAVAILABLE: native app facade unavailable', file=sys.stderr)\n\
         \x20   sys.exit(3)\n\
         try:\n\
         \x20   result = app.{function}({argument:?})\n\
         except Exception as error:\n\
         \x20   payload = app.app_error_payload(error)\n\
         \x20   print(json.dumps({{'qx_app_error': payload if payload is not None else str(error)}}))\n\
         \x20   sys.exit(4)\n\
         print(json.dumps(result))\n",
        native_dir = import_dir.to_string_lossy(),
        python_dir = root.join("python").to_string_lossy(),
    );
    let output = Command::new(&python)
        .arg("-c")
        .arg(script)
        .current_dir(root.join("python"))
        .output()
        .expect("启动解释器失败");
    let stdout = String::from_utf8_lossy(&output.stdout);
    match output.status.code() {
        Some(0) => Some(Ok(
            serde_json::from_str(stdout.trim()).expect("Python stdout JSON")
        )),
        Some(3) => None,
        Some(4) => {
            let value: Value = serde_json::from_str(stdout.trim()).expect("Python error JSON");
            Some(Err(value["qx_app_error"].to_string()))
        }
        other => panic!(
            "Python 腿异常退出 {other:?}：stderr={}",
            String::from_utf8_lossy(&output.stderr)
        ),
    }
}
