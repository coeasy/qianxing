//! A 股时间轴解析：上海时区日期/时刻/ISO-8601 文本到 epoch 毫秒的纯函数。
//!
//! 从 `ashare.rs` 纯搬家拆出：`DAY_MS` / `SHANGHAI_OFFSET_MS` 仍由父模块持有，
//! 这里只搬走解析实现。

use super::*;

/// 解析 `YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS[.fff][Z|±HH:MM]`。
///
/// 无时区后缀时按 Asia/Shanghai(+08:00) 解释，与
/// [`date_to_shanghai_midnight_ms`] 的日线时间轴保持一致；A 股公告时间都带
/// `+08:00`，因此两种写法会落在同一毫秒上。
pub(crate) fn timestamp_text_to_ms(text: &str) -> Result<u64, String> {
    if text.len() < 10 {
        return Err("时间必须是 YYYY-MM-DD 或 ISO-8601 字符串".into());
    }
    let (date, rest) = text.split_at(10);
    // 日期午夜（+08:00）对应的 UTC 毫秒。
    let base_ms = date_to_shanghai_midnight_ms(date)?;
    let mut rest = rest;
    if !rest.is_empty() {
        let separator = rest.as_bytes()[0];
        if !matches!(separator, b'T' | b't' | b' ') {
            return Err("日期与时间之间必须使用 T 或空格分隔".into());
        }
        rest = &rest[1..];
    }
    let mut offset_ms: i64 = i64::try_from(SHANGHAI_OFFSET_MS).expect("上海偏移可表示为 i64");
    // 时区后缀：时间部分自身不含 Z/+/-，因此首个出现处即偏移起点。
    let clock_text = match rest.find(['Z', 'z', '+', '-']) {
        Some(index) => {
            let (clock, zone) = rest.split_at(index);
            let sign = zone.as_bytes()[0];
            let zone = &zone[1..];
            if matches!(sign, b'Z' | b'z') {
                if !zone.is_empty() {
                    return Err("时区 Z 之后不能带其它字符".into());
                }
                offset_ms = 0;
            } else {
                let (zone_hour, zone_minute) = match zone.split_once(':') {
                    Some((hour, minute)) => (hour, minute),
                    None if zone.len() == 4 => zone.split_at(2),
                    None => return Err("时区偏移必须是 ±HH:MM".into()),
                };
                let hour = zone_hour
                    .parse::<u64>()
                    .map_err(|_| "时区小时部分非法".to_string())?;
                let minute = zone_minute
                    .parse::<u64>()
                    .map_err(|_| "时区分钟部分非法".to_string())?;
                if hour > 26 || minute >= 60 {
                    return Err("时区偏移超出范围".into());
                }
                let mut value = i64::try_from(hour * 3_600 + minute * 60)
                    .expect("合法时区偏移可表示为 i64")
                    * 1_000;
                if sign == b'-' {
                    value = -value;
                }
                offset_ms = value;
            }
            clock
        }
        None => rest,
    };
    let (clock_text, fraction) = match clock_text.split_once('.') {
        Some((clock, fraction)) => (clock, fraction),
        None => (clock_text, ""),
    };
    let mut clock_ms = if clock_text.is_empty() {
        0
    } else {
        time_of_day_ms(clock_text)?
    };
    if !fraction.is_empty() {
        if !fraction.bytes().all(|byte| byte.is_ascii_digit()) {
            return Err("秒的小数部分必须是数字".into());
        }
        let digits = &fraction[..fraction.len().min(3)];
        let scaled = digits
            .parse::<u64>()
            .map_err(|_| "秒的小数部分非法".to_string())?
            * 10u64.pow(3 - digits.len() as u32);
        clock_ms += scaled;
    }
    let days_ms = i64::try_from(base_ms)
        .map_err(|_| "日期超出范围".to_string())?
        .checked_add(SHANGHAI_OFFSET_MS as i64)
        .ok_or_else(|| "日期时间戳溢出".to_string())?;
    let clock_ms = i64::try_from(clock_ms).expect("一天的毫秒数可表示为 i64");
    let timestamp = days_ms
        .checked_add(clock_ms)
        .and_then(|value| value.checked_sub(offset_ms))
        .ok_or_else(|| "时间戳溢出".to_string())?;
    u64::try_from(timestamp).map_err(|_| "时间必须不早于 Unix epoch".into())
}

pub(crate) fn date_to_shanghai_midnight_ms(value: &str) -> Result<u64, String> {
    let date = value
        .get(..10)
        .filter(|date| {
            date.as_bytes().get(4) == Some(&b'-') && date.as_bytes().get(7) == Some(&b'-')
        })
        .ok_or_else(|| "必须是 YYYY-MM-DD".to_string())?;
    let mut parts = date.split('-');
    let year = parts
        .next()
        .ok_or_else(|| "缺少年份".to_string())?
        .parse::<i64>()
        .map_err(|_| "年份非法".to_string())?;
    let month = parts
        .next()
        .ok_or_else(|| "缺少月份".to_string())?
        .parse::<i64>()
        .map_err(|_| "月份非法".to_string())?;
    let day = parts
        .next()
        .ok_or_else(|| "缺少日期".to_string())?
        .parse::<i64>()
        .map_err(|_| "日期非法".to_string())?;
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let days_in_month = match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => 31,
        4 | 6 | 9 | 11 => 30,
        2 if leap => 29,
        2 => 28,
        _ => 0,
    };
    if !(1..=12).contains(&month) || !(1..=days_in_month).contains(&day) {
        return Err("日期超出范围".into());
    }
    let adjusted_year = year - i64::from(month <= 2);
    let era = if adjusted_year >= 0 {
        adjusted_year / 400
    } else {
        (adjusted_year - 399) / 400
    };
    let year_of_era = adjusted_year - era * 400;
    let month_prime = month + if month > 2 { -3 } else { 9 };
    let day_of_year = (153 * month_prime + 2) / 5 + day - 1;
    let day_of_era = year_of_era * 365 + year_of_era / 4 - year_of_era / 100 + day_of_year;
    let days_since_epoch = era * 146_097 + day_of_era - 719_468;
    let timestamp = days_since_epoch
        .checked_mul(86_400_000)
        .and_then(|value| value.checked_sub(8 * 3_600_000))
        .ok_or_else(|| "日期时间戳溢出".to_string())?;
    u64::try_from(timestamp).map_err(|_| "日期必须不早于 Unix epoch".into())
}

pub(crate) fn time_of_day_ms(value: &str) -> Result<u64, String> {
    let mut parts = value.split(':');
    let hour = parts
        .next()
        .ok_or_else(|| "缺少小时".to_string())?
        .parse::<u64>()
        .map_err(|_| "小时非法".to_string())?;
    let minute = parts
        .next()
        .ok_or_else(|| "缺少分钟".to_string())?
        .parse::<u64>()
        .map_err(|_| "分钟非法".to_string())?;
    let second = parts
        .next()
        .map(|value| value.parse::<u64>().map_err(|_| "秒非法".to_string()))
        .transpose()?
        .unwrap_or(0);
    if hour >= 24 || minute >= 60 || second >= 60 {
        return Err("时间超出范围".into());
    }
    Ok((hour * 3_600 + minute * 60 + second) * 1_000)
}
