use qx_strategy::{
    capabilities::StrategyCapability, sha256_hex, DynamicCAbiLoadPolicy, DynamicCAbiStrategy,
};
use std::path::Path;
use std::process::Command;

fn build_legacy_plugin(directory: &Path, create_marker: &Path) -> std::path::PathBuf {
    let source = directory.join("legacy_plugin.rs");
    let library = directory.join(format!("legacy_plugin.{}", std::env::consts::DLL_EXTENSION));
    let source_text = format!(
        r#"
use std::ffi::{{c_char, c_void}};

#[repr(C)]
struct VTable {{
    abi_version: u32,
    create: Option<unsafe extern "C" fn(*const c_char) -> *mut c_void>,
    on_init: Option<unsafe extern "C" fn(*mut c_void, *const c_void, *mut c_char, usize) -> i32>,
    on_event: Option<unsafe extern "C" fn(*mut c_void, *const c_void, *const c_void, *mut c_void, *mut c_char, usize) -> i32>,
    on_order_update: Option<unsafe extern "C" fn(*mut c_void, *const c_char, *mut c_void, *mut c_char, usize) -> i32>,
    free_decision: Option<unsafe extern "C" fn(*mut c_void, *mut c_void)>,
    destroy: Option<unsafe extern "C" fn(*mut c_void)>,
}}

unsafe extern "C" fn create(_: *const c_char) -> *mut c_void {{
    let _ = std::fs::write({marker:?}, b"created");
    1_usize as *mut c_void
}}
unsafe extern "C" fn on_init(_: *mut c_void, _: *const c_void, _: *mut c_char, _: usize) -> i32 {{ 0 }}
unsafe extern "C" fn on_event(_: *mut c_void, _: *const c_void, _: *const c_void, _: *mut c_void, _: *mut c_char, _: usize) -> i32 {{ 0 }}
unsafe extern "C" fn free_decision(_: *mut c_void, _: *mut c_void) {{}}
unsafe extern "C" fn destroy(_: *mut c_void) {{}}

static VTABLE: VTable = VTable {{
    abi_version: 2,
    create: Some(create),
    on_init: Some(on_init),
    on_event: Some(on_event),
    on_order_update: None,
    free_decision: Some(free_decision),
    destroy: Some(destroy),
}};

#[no_mangle]
pub extern "C" fn qx_strategy_get_vtable() -> *const VTable {{ &VTABLE }}
"#,
        marker = create_marker.to_string_lossy()
    );
    std::fs::write(&source, source_text).expect("write minimal legacy C ABI plugin");
    let compiler = std::env::var_os("RUSTC").unwrap_or_else(|| "rustc".into());
    let output = Command::new(compiler)
        .args(["--edition=2021", "--crate-type", "cdylib"])
        .arg(&source)
        .arg("-o")
        .arg(&library)
        .output()
        .expect("run rustc to build the legacy ABI fixture");
    assert!(
        output.status.success(),
        "legacy C ABI fixture did not build: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    library
}

fn verified_policy(library: &Path) -> DynamicCAbiLoadPolicy {
    let bytes = std::fs::read(library).expect("read compiled plugin bytes");
    DynamicCAbiLoadPolicy::new(sha256_hex(&bytes))
        .with_max_library_bytes(u64::try_from(bytes.len()).expect("library size fits u64"))
}

fn isolated_directory() -> std::path::PathBuf {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("system clock is after Unix epoch")
        .as_nanos();
    let directory =
        std::env::temp_dir().join(format!("qx-legacy-c-abi-{}-{nonce}", std::process::id()));
    std::fs::create_dir(&directory).expect("create isolated legacy plugin directory");
    directory
}

#[test]
fn legacy_plugin_without_optional_requirement_export_loads_empty_requirements_and_fails_closed_before_create(
) {
    let directory = isolated_directory();
    let marker = directory.join("plugin-create-called");
    let library = build_legacy_plugin(&directory, &marker);
    let policy = verified_policy(&library);

    let strategy = unsafe {
        DynamicCAbiStrategy::load_verified_with_required_capabilities(
            &library,
            "{}",
            &policy,
            &[],
            false,
        )
    }
    .expect("legacy plugin must remain compatible with an empty capability requirement");
    assert!(
        marker.is_file(),
        "empty requirement path reaches plugin create"
    );
    drop(strategy);

    std::fs::remove_file(&marker).expect("clear create marker before unsupported load");
    let rejected = unsafe {
        DynamicCAbiStrategy::load_verified_with_required_capabilities(
            &library,
            "{}",
            &policy,
            &[StrategyCapability::FillUpdates],
            false,
        )
    }
    .err()
    .expect("legacy plugin must reject unsupported FillUpdates");
    assert!(rejected.contains("does not support"), "{rejected}");
    assert!(
        !marker.exists(),
        "capability rejection must happen before the plugin create callback"
    );
    std::fs::remove_dir_all(directory).expect("remove isolated legacy plugin directory");
}
