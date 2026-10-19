//! Native reconciliation for already-normalized A-share corporate actions.

use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::{PyBytes, PyModule};

#[pyfunction]
fn sha256_hex_bytes(payload: &Bound<'_, PyBytes>) -> String {
    sha256_hex_bytes_impl(payload.as_bytes())
}

fn sha256_hex_bytes_impl(payload: &[u8]) -> String {
    qx_data::sha256_hex_bytes(payload)
}

#[pyfunction]
fn decimal_to_raw(value: &str) -> PyResult<String> {
    qx_core::parse_decimal_half_even(&value.replace(',', ""))
        .map(|raw| raw.to_string())
        .map_err(PyValueError::new_err)
}

#[pyfunction]
fn raw_scale() -> i64 {
    qx_core::SCALE as i64
}

#[pyfunction]
fn validate_corporate_actions_json(payload: &str) -> PyResult<()> {
    qx_xingban::ashare::validate_corporate_action_json(payload).map_err(PyValueError::new_err)
}

#[pyfunction]
fn resolve_corporate_action_ratios(payload: &str) -> PyResult<String> {
    qx_xingban::ashare::resolve_corporate_action_ratios_json(payload).map_err(PyValueError::new_err)
}

#[pyfunction]
fn calendar_component_fingerprint(payload: &str) -> PyResult<(String, u64)> {
    qx_data::calendar_component_fingerprint_from_json(payload).map_err(PyValueError::new_err)
}

#[pyfunction]
fn validate_ashare_calendar_json(payload: &str) -> PyResult<()> {
    qx_xingban::ashare::validate_ashare_calendar_json(payload).map_err(PyValueError::new_err)
}

#[pyfunction]
fn reconcile_corporate_actions(payload: &str) -> PyResult<String> {
    qx_xingban::ashare::reconcile_corporate_actions_json(payload).map_err(PyValueError::new_err)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(sha256_hex_bytes, module)?)?;
    module.add_function(wrap_pyfunction!(decimal_to_raw, module)?)?;
    module.add_function(wrap_pyfunction!(raw_scale, module)?)?;
    module.add_function(wrap_pyfunction!(validate_corporate_actions_json, module)?)?;
    module.add_function(wrap_pyfunction!(resolve_corporate_action_ratios, module)?)?;
    module.add_function(wrap_pyfunction!(calendar_component_fingerprint, module)?)?;
    module.add_function(wrap_pyfunction!(validate_ashare_calendar_json, module)?)?;
    module.add_function(wrap_pyfunction!(reconcile_corporate_actions, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        decimal_to_raw, reconcile_corporate_actions, resolve_corporate_action_ratios,
        sha256_hex_bytes_impl,
    };

    #[test]
    fn sha256_bridge_uses_the_shared_data_digest() {
        assert_eq!(
            sha256_hex_bytes_impl(b"abc"),
            "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
        );
    }

    #[test]
    fn reconciliation_matches_python_merge_contract_for_fill_and_conflict() {
        let payload = r#"[[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"dividend","cash_dividend_raw":0,"share_ratio_num":1,"share_ratio_den":1,"source":"akshare","raw_payload":{"a":1}}],[{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"dividend","cash_dividend_raw":100000000,"share_ratio_num":1,"share_ratio_den":1,"source":"baostock","raw_payload":{"b":2}},{"instrument":"000001.SZSE","ex_date":"2024-06-03","action_type":"dividend","cash_dividend_raw":200000000,"share_ratio_num":1,"share_ratio_den":1,"source":"tdx","raw_payload":{}}]]"#;
        let merged = reconcile_corporate_actions(payload).unwrap();
        let result: serde_json::Value = serde_json::from_str(&merged).unwrap();
        assert_eq!(result["actions"].as_array().unwrap().len(), 1);
        assert_eq!(result["actions"][0]["cash_dividend_raw"], 100000000);
        assert_eq!(result["actions"][0]["source"], "akshare+baostock+tdx");
        assert_eq!(
            result["actions"][0]["raw_payload"],
            serde_json::json!({"a":1,"b":2})
        );
        assert_eq!(result["conflicts"].as_array().unwrap().len(), 1);
        assert_eq!(result["conflicts"][0]["left"], 100000000);
        assert_eq!(result["conflicts"][0]["right"], 200000000);
    }

    #[test]
    fn decimal_conversion_uses_scale_and_rounds_half_even() {
        assert_eq!(decimal_to_raw("0.0000000005").unwrap(), "0");
        assert_eq!(decimal_to_raw("0.0000000015").unwrap(), "2");
        assert_eq!(decimal_to_raw("1,234.5").unwrap(), "1234500000000");
        assert_eq!(decimal_to_raw("-2.25").unwrap(), "-2250000000");
        assert!(decimal_to_raw("NaN").is_err());
    }

    #[test]
    fn corporate_action_ratio_bridge_uses_the_xingban_domain_rule() {
        let payload = r#"{"bonus":{"numerator":"100000000","denominator":"1000000000"},"transfer":{"numerator":"200000000","denominator":"1000000000"},"share_ratio":null,"rights_issue":{"numerator":"0","denominator":"1000000000"},"rights_issue_ratio":{"numerator":"2","denominator":"5"},"conversion":{"numerator":"0","denominator":"1000000000"},"conversion_ratio":null}"#;
        let output = resolve_corporate_action_ratios(payload).unwrap();
        let output: serde_json::Value = serde_json::from_str(&output).unwrap();
        assert_eq!(output["share_ratio_num"], "1320000000");
        assert_eq!(output["rights_issue_ratio_num"], "2");
    }
}
