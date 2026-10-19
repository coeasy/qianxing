use pyo3::exceptions::PyValueError;
use pyo3::prelude::*;
use pyo3::types::PyModule;

#[pyfunction]
fn decimal_to_raw_exact(value: &str, allow_negative: bool) -> PyResult<String> {
    let raw = qx_core::parse_decimal_exact(value).map_err(PyValueError::new_err)?;
    if raw < 0 && !allow_negative {
        return Err(PyValueError::new_err("decimal value must be non-negative"));
    }
    Ok(raw.to_string())
}

#[pyfunction]
fn raw_to_decimal(value: &str, allow_negative: bool) -> PyResult<String> {
    let raw = value
        .parse::<i128>()
        .map_err(|_| PyValueError::new_err("raw decimal value is out of range"))?;
    if raw < 0 && !allow_negative {
        return Err(PyValueError::new_err(
            "raw decimal value must be non-negative",
        ));
    }
    Ok(qx_core::Money::from_raw(raw).to_string())
}

#[pyfunction]
fn decimal_to_scaled_integer(value: &str, multiplier: i64) -> PyResult<i64> {
    qx_core::parse_decimal_scaled_integer(value, multiplier).map_err(PyValueError::new_err)
}

#[pyfunction]
fn decimal_to_integer_exact(value: &str) -> PyResult<String> {
    qx_core::parse_decimal_integer_exact(value)
        .map(|integer| integer.to_string())
        .map_err(PyValueError::new_err)
}

pub(crate) fn register(module: &Bound<'_, PyModule>) -> PyResult<()> {
    module.add_function(wrap_pyfunction!(decimal_to_raw_exact, module)?)?;
    module.add_function(wrap_pyfunction!(raw_to_decimal, module)?)?;
    module.add_function(wrap_pyfunction!(decimal_to_scaled_integer, module)?)?;
    module.add_function(wrap_pyfunction!(decimal_to_integer_exact, module)?)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_boundary_preserves_exact_scale_and_sign_policy() {
        assert_eq!(decimal_to_raw_exact("1e-3", false).unwrap(), "1000000");
        assert!(decimal_to_raw_exact("1.0000000001", false).is_err());
        assert!(decimal_to_raw_exact("-1", false).is_err());
        assert_eq!(decimal_to_raw_exact("-1.25", true).unwrap(), "-1250000000");
        assert_eq!(raw_to_decimal("1000000001", false).unwrap(), "1.000000001");
        assert_eq!(raw_to_decimal("-1250000000", true).unwrap(), "-1.25");
        assert!(raw_to_decimal("-1", false).is_err());
        assert_eq!(decimal_to_scaled_integer("0.0001", 10_000).unwrap(), 1);
        assert_eq!(decimal_to_scaled_integer("0.5", 100).unwrap(), 50);
        assert!(decimal_to_scaled_integer("0.00001", 10_000).is_err());
        assert_eq!(decimal_to_integer_exact("123.0").unwrap(), "123");
        assert!(decimal_to_integer_exact("123.1").is_err());
    }
}
