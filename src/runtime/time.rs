use crate::runtime::values::Value;
use chrono::{DateTime, NaiveDate, TimeZone, Utc};
use std::collections::HashMap;

pub fn parse_time_reference(raw: &str, now: DateTime<Utc>) -> Result<DateTime<Utc>, String> {
    if let Some(parsed) = parse_time_phrase(raw, now)? {
        return Ok(parsed);
    }

    if let Ok(seconds) = raw.parse::<i64>() {
        return Utc
            .timestamp_opt(seconds, 0)
            .single()
            .ok_or_else(|| format!("Invalid epoch seconds '{}'", raw));
    }

    if let Ok(dt) = DateTime::parse_from_rfc3339(raw) {
        return Ok(dt.with_timezone(&Utc));
    }

    if let Ok(date) = NaiveDate::parse_from_str(raw, "%Y-%m-%d") {
        let Some(naive) = date.and_hms_opt(0, 0, 0) else {
            return Err(format!("Invalid date '{}'", raw));
        };
        return Ok(Utc.from_utc_datetime(&naive));
    }

    Err(format!(
        "Expected RFC3339 timestamp, YYYY-MM-DD date, or epoch seconds, got '{}'",
        raw
    ))
}

pub fn duration_summary(seconds: i64) -> Value {
    let abs_seconds = seconds.abs();
    let mut map = HashMap::new();
    map.insert("seconds".into(), Value::Number(seconds as f64));
    map.insert("minutes".into(), Value::Number(seconds as f64 / 60.0));
    map.insert("hours".into(), Value::Number(seconds as f64 / 3_600.0));
    map.insert("days".into(), Value::Number(seconds as f64 / 86_400.0));
    map.insert("human".into(), Value::String(human_duration(abs_seconds)));
    Value::Object(map)
}

fn parse_time_phrase(raw: &str, now: DateTime<Utc>) -> Result<Option<DateTime<Utc>>, String> {
    let phrase = raw.trim().to_ascii_lowercase();

    match phrase.as_str() {
        "now" => return Ok(Some(now)),
        "today" => return Ok(Some(start_of_utc_day(now, 0)?)),
        "tomorrow" => return Ok(Some(start_of_utc_day(now, 1)?)),
        "yesterday" => return Ok(Some(start_of_utc_day(now, -1)?)),
        _ => {}
    }

    let parts: Vec<&str> = phrase.split_whitespace().collect();
    if parts.len() == 3 && parts[0] == "in" {
        let amount = parse_phrase_amount(parts[1], raw)?;
        let duration = phrase_duration(amount, parts[2], raw)?;
        return Ok(Some(now + duration));
    }

    if parts.len() == 3 && parts[2] == "ago" {
        let amount = parse_phrase_amount(parts[0], raw)?;
        let duration = phrase_duration(amount, parts[1], raw)?;
        return Ok(Some(now - duration));
    }

    Ok(None)
}

fn start_of_utc_day(now: DateTime<Utc>, day_offset: i64) -> Result<DateTime<Utc>, String> {
    let date = now.date_naive() + chrono::Duration::days(day_offset);
    let Some(naive) = date.and_hms_opt(0, 0, 0) else {
        return Err("Invalid day boundary".into());
    };
    Ok(Utc.from_utc_datetime(&naive))
}

fn parse_phrase_amount(raw_amount: &str, phrase: &str) -> Result<i64, String> {
    raw_amount
        .parse::<i64>()
        .map_err(|_| format!("Invalid time phrase amount in '{}'", phrase))
}

fn phrase_duration(amount: i64, raw_unit: &str, phrase: &str) -> Result<chrono::Duration, String> {
    let normalized = raw_unit.to_ascii_lowercase();
    let unit = normalized.as_str();
    match unit {
        "ms" | "millisecond" | "milliseconds" => Ok(chrono::Duration::milliseconds(amount)),
        "s" | "sec" | "second" => Ok(chrono::Duration::seconds(amount)),
        "m" | "min" | "minute" => Ok(chrono::Duration::minutes(amount)),
        "h" | "hr" | "hour" => Ok(chrono::Duration::hours(amount)),
        "d" | "day" => Ok(chrono::Duration::days(amount)),
        "w" | "week" => Ok(chrono::Duration::weeks(amount)),
        _ if unit.ends_with('s') => phrase_duration(amount, &unit[..unit.len() - 1], phrase),
        _ => Err(format!(
            "Unsupported time phrase unit '{}' in '{}'",
            raw_unit, phrase
        )),
    }
}

fn human_duration(seconds: i64) -> String {
    let units = [
        ("year", 31_536_000),
        ("day", 86_400),
        ("hour", 3_600),
        ("minute", 60),
        ("second", 1),
    ];

    for (name, unit_seconds) in units {
        if seconds >= unit_seconds {
            let amount = seconds / unit_seconds;
            let suffix = if amount == 1 { "" } else { "s" };
            return format!("{} {}{}", amount, name, suffix);
        }
    }

    "0 seconds".into()
}
