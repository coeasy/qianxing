use std::collections::BTreeMap;

pub fn aggregate_factor_exposure(values: &[BTreeMap<String, i32>]) -> BTreeMap<String, i32> {
    let mut result: BTreeMap<String, i32> = BTreeMap::new();
    for value in values {
        for (key, exposure) in value {
            let current = result.entry(key.clone()).or_insert(0);
            *current = (*current).saturating_add(*exposure);
        }
    }
    result
}
