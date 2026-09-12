use std::collections::BTreeMap;

pub fn aggregate_factor_exposure(values: &[BTreeMap<String, i32>]) -> BTreeMap<String, i32> {
    let mut result = BTreeMap::new();
    for value in values {
        for (key, exposure) in value {
            *result.entry(key.clone()).or_insert(0) += *exposure;
        }
    }
    result
}
