use qx_core::SCALE;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct RatioParts {
    numerator: String,
    denominator: String,
}

#[derive(Debug, Clone, Copy)]
struct ParsedRatio {
    numerator: i128,
    denominator: i128,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CorporateActionRatiosInput {
    bonus: RatioParts,
    transfer: RatioParts,
    share_ratio: Option<RatioParts>,
    rights_issue: RatioParts,
    rights_issue_ratio: Option<RatioParts>,
    conversion: RatioParts,
    conversion_ratio: Option<RatioParts>,
}

#[derive(Debug, Serialize)]
struct CorporateActionRatiosOutput {
    share_ratio_num: String,
    share_ratio_den: String,
    rights_issue_ratio_num: String,
    rights_issue_ratio_den: String,
    conversion_ratio_num: String,
    conversion_ratio_den: String,
}

/// Combine per-share bonus-share and capital-transfer additions into one
/// post-action share ratio, preserving the legacy sequential fixed-point floor.
fn combine_share_ratio_parts(
    bonus_raw: i128,
    transfer_raw: i128,
) -> Result<(i128, i128), &'static str> {
    if bonus_raw < 0 || transfer_raw < 0 {
        return Err("share ratio components must be non-negative");
    }
    let mut numerator = SCALE;
    for part in [bonus_raw, transfer_raw] {
        if part == 0 {
            continue;
        }
        let factor = SCALE
            .checked_add(part)
            .ok_or("share ratio component is out of range")?;
        numerator = numerator
            .checked_mul(factor)
            .ok_or("combined share ratio is out of range")?
            / SCALE;
    }
    Ok((numerator, SCALE))
}

/// Resolve normalized provider ratios against already-normalized contract pairs.
///
/// Provider aliases and numeric parsing stay in the adapter. This function owns
/// the domain precedence: bonus/transfer additions combine when present; an
/// explicit contract pair is used only when those additions are absent; rights
/// and conversion contract pairs replace an empty provider ratio.
pub fn resolve_corporate_action_ratios_json(payload: &str) -> Result<String, String> {
    let input: CorporateActionRatiosInput =
        serde_json::from_str(payload).map_err(|error| format!("公司行为比例输入无效: {error}"))?;
    let bonus = parse_ratio("bonus", &input.bonus)?;
    let transfer = parse_ratio("transfer", &input.transfer)?;
    let rights_issue = parse_ratio("rights_issue", &input.rights_issue)?;
    let conversion = parse_ratio("conversion", &input.conversion)?;
    let share_ratio = input
        .share_ratio
        .as_ref()
        .map(|pair| parse_ratio("share_ratio", pair))
        .transpose()?;
    let rights_issue_ratio = input
        .rights_issue_ratio
        .as_ref()
        .map(|pair| parse_ratio("rights_issue_ratio", pair))
        .transpose()?;
    let conversion_ratio = input
        .conversion_ratio
        .as_ref()
        .map(|pair| parse_ratio("conversion_ratio", pair))
        .transpose()?;
    for (name, pair) in [
        ("bonus", bonus),
        ("transfer", transfer),
        ("rights_issue", rights_issue),
        ("conversion", conversion),
    ] {
        validate_provider_ratio(name, pair)?;
    }
    for (name, pair) in [
        ("share_ratio", share_ratio),
        ("rights_issue_ratio", rights_issue_ratio),
        ("conversion_ratio", conversion_ratio),
    ] {
        if let Some(pair) = pair {
            validate_explicit_ratio(name, pair)?;
        }
    }

    let share_ratio = if bonus.numerator != 0 || transfer.numerator != 0 {
        combine_share_ratio_parts(bonus.numerator, transfer.numerator)?
    } else {
        share_ratio
            .map(|pair| (pair.numerator, pair.denominator))
            .unwrap_or((SCALE, SCALE))
    };
    let rights_issue_ratio = if rights_issue.numerator == 0 {
        rights_issue_ratio
            .map(|pair| (pair.numerator, pair.denominator))
            .unwrap_or((rights_issue.numerator, rights_issue.denominator))
    } else {
        (rights_issue.numerator, rights_issue.denominator)
    };
    let conversion_ratio = if conversion.numerator == 0 {
        conversion_ratio
            .map(|pair| (pair.numerator, pair.denominator))
            .unwrap_or((conversion.numerator, conversion.denominator))
    } else {
        (conversion.numerator, conversion.denominator)
    };

    serde_json::to_string(&CorporateActionRatiosOutput {
        share_ratio_num: share_ratio.0.to_string(),
        share_ratio_den: share_ratio.1.to_string(),
        rights_issue_ratio_num: rights_issue_ratio.0.to_string(),
        rights_issue_ratio_den: rights_issue_ratio.1.to_string(),
        conversion_ratio_num: conversion_ratio.0.to_string(),
        conversion_ratio_den: conversion_ratio.1.to_string(),
    })
    .map_err(|error| format!("公司行为比例结果无法编码: {error}"))
}

fn validate_provider_ratio(name: &str, pair: ParsedRatio) -> Result<(), String> {
    if pair.numerator < 0 || pair.denominator <= 0 {
        return Err(format!(
            "{name} ratio must be non-negative with a positive denominator"
        ));
    }
    Ok(())
}

fn parse_ratio(name: &str, pair: &RatioParts) -> Result<ParsedRatio, String> {
    let numerator = pair
        .numerator
        .parse::<i128>()
        .map_err(|_| format!("{name} numerator is out of range"))?;
    let denominator = pair
        .denominator
        .parse::<i128>()
        .map_err(|_| format!("{name} denominator is out of range"))?;
    Ok(ParsedRatio {
        numerator,
        denominator,
    })
}

fn validate_explicit_ratio(name: &str, pair: ParsedRatio) -> Result<(), String> {
    if pair.numerator < 0 || pair.denominator <= 0 {
        return Err(format!(
            "{name} must be non-negative with a positive denominator"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{combine_share_ratio_parts, resolve_corporate_action_ratios_json};
    use qx_core::SCALE;

    #[test]
    fn combines_bonus_and_transfer_with_the_legacy_sequential_floor() {
        assert_eq!(combine_share_ratio_parts(0, 0), Ok((SCALE, SCALE)));
        assert_eq!(
            combine_share_ratio_parts(SCALE / 10, SCALE / 5),
            Ok((1_320_000_000, SCALE))
        );
        assert_eq!(
            combine_share_ratio_parts(-1, 0),
            Err("share ratio components must be non-negative")
        );
        assert_eq!(
            combine_share_ratio_parts(i128::MAX, 0),
            Err("share ratio component is out of range")
        );
    }

    #[test]
    fn resolves_provider_and_contract_ratio_precedence() {
        let payload = format!(
            r#"{{"bonus":{{"numerator":"{}","denominator":"{}"}},"transfer":{{"numerator":"{}","denominator":"{}"}},"share_ratio":{{"numerator":"9","denominator":"4"}},"rights_issue":{{"numerator":"0","denominator":"{}"}},"rights_issue_ratio":{{"numerator":"2","denominator":"5"}},"conversion":{{"numerator":"300000000","denominator":"{}"}},"conversion_ratio":{{"numerator":"7","denominator":"8"}}}}"#,
            SCALE / 10,
            SCALE,
            SCALE / 5,
            SCALE,
            SCALE,
            SCALE,
        );
        let ratios: serde_json::Value =
            serde_json::from_str(&resolve_corporate_action_ratios_json(&payload).unwrap()).unwrap();
        assert_eq!(ratios["share_ratio_num"], "1320000000");
        assert_eq!(ratios["share_ratio_den"], SCALE.to_string());
        assert_eq!(ratios["rights_issue_ratio_num"], "2");
        assert_eq!(ratios["rights_issue_ratio_den"], "5");
        assert_eq!(ratios["conversion_ratio_num"], "300000000");
        assert_eq!(ratios["conversion_ratio_den"], SCALE.to_string());
    }

    #[test]
    fn uses_explicit_share_pair_only_when_bonus_and_transfer_are_empty() {
        let payload = format!(
            r#"{{"bonus":{{"numerator":"0","denominator":"{}"}},"transfer":{{"numerator":"0","denominator":"{}"}},"share_ratio":{{"numerator":"3","denominator":"2"}},"rights_issue":{{"numerator":"0","denominator":"{}"}},"rights_issue_ratio":null,"conversion":{{"numerator":"0","denominator":"{}"}},"conversion_ratio":null}}"#,
            SCALE, SCALE, SCALE, SCALE
        );
        let ratios: serde_json::Value =
            serde_json::from_str(&resolve_corporate_action_ratios_json(&payload).unwrap()).unwrap();
        assert_eq!(ratios["share_ratio_num"], "3");
        assert_eq!(ratios["share_ratio_den"], "2");
        assert_eq!(ratios["rights_issue_ratio_num"], "0");
        assert_eq!(ratios["conversion_ratio_num"], "0");
    }
}
