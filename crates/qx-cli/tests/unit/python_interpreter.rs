use crate::python_interpreter_for;

#[test]
fn configured_python_interpreter_has_priority_on_every_platform() {
    assert_eq!(
        python_interpreter_for(Some("/opt/qx/python"), false),
        ("/opt/qx/python".into(), "来自 QX_PYTHON")
    );
}

#[test]
fn empty_python_override_uses_platform_default() {
    assert_eq!(
        python_interpreter_for(Some("  "), true),
        ("python".into(), "QX_PYTHON 未设置，回落 PATH python")
    );
    assert_eq!(
        python_interpreter_for(None, false),
        ("python3".into(), "QX_PYTHON 未设置，回落 PATH python3")
    );
}
