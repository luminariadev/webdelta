use serde::{Deserialize, Serialize};

/// Numeric comparison between the previous and current snapshots.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct NumericDelta {
    pub previous: f64,
    pub current: f64,
    pub diff: f64,
    pub percent_change: Option<f64>,
}

/// Computes signed and percentage deltas between two numeric values.
pub fn compute_delta(previous: f64, current: f64) -> NumericDelta {
    let diff = current - previous;
    let percent_change = if previous.abs() > f64::EPSILON {
        Some((diff / previous.abs()) * 100.0)
    } else {
        None
    };
    NumericDelta {
        previous,
        current,
        diff,
        percent_change,
    }
}

const CURRENCY_CODES: &[&str] = &[
    "USD", "EUR", "GBP", "IDR", "RP", "JPY", "SGD", "MYR", "AUD", "CAD", "CHF", "INR", "CNY", "KRW",
];

/// Extracts the first numeric or currency token from arbitrary text.
///
/// Handles US/international (`$1,299.99`), Indonesian/European (`Rp 1.500.000`,
/// `EUR 49,90`), signed numbers (`-12.5`), and percentages (`99.95%`), while
/// skipping version numbers (`v0.1.0`), IPv4 addresses, dates, and alphanumeric
/// identifiers (`h1`, `sha256`).
pub fn parse_numeric(input: &str) -> Option<f64> {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];
        let is_sign = ch == '-' || ch == '+';

        if ch.is_ascii_digit() {
            let end = scan_token_end(&chars, i);
            if has_disallowed_attached_prefix(&chars, i) {
                i = end;
                continue;
            }
            let sign = find_preceding_sign(&chars, i);
            let dot_thousands_hint = has_dot_thousands_currency(&chars, i);
            if let Some(val) = parse_token_slice(&chars[i..end], sign, dot_thousands_hint) {
                return Some(val);
            }
            i = end;
            continue;
        }

        if is_sign && i + 1 < len && chars[i + 1].is_ascii_digit() {
            let digit_start = i + 1;
            let end = scan_token_end(&chars, digit_start);
            if has_disallowed_attached_prefix(&chars, i) {
                i = end;
                continue;
            }
            let dot_thousands_hint = has_dot_thousands_currency(&chars, i);
            if let Some(val) =
                parse_token_slice(&chars[digit_start..end], Some(ch), dot_thousands_hint)
            {
                return Some(val);
            }
            i = end;
            continue;
        }

        i += 1;
    }

    None
}

fn scan_token_end(chars: &[char], start: usize) -> usize {
    let mut end = start;
    while end < chars.len() {
        let c = chars[end];
        if c.is_ascii_digit() || c == '.' || c == ',' {
            end += 1;
        } else {
            break;
        }
    }
    end
}

fn has_disallowed_attached_prefix(chars: &[char], boundary: usize) -> bool {
    if boundary == 0 {
        return false;
    }
    let prev = chars[boundary - 1];
    if !prev.is_ascii_alphabetic() {
        return false;
    }

    let mut start = boundary;
    while start > 0 && chars[start - 1].is_ascii_alphabetic() {
        start -= 1;
    }
    let word: String = chars[start..boundary].iter().collect();
    !CURRENCY_CODES.contains(&word.to_ascii_uppercase().as_str())
}

fn find_preceding_sign(chars: &[char], digit_idx: usize) -> Option<char> {
    let mut idx = digit_idx;
    while idx > 0 {
        idx -= 1;
        let c = chars[idx];
        if c == '-' || c == '+' {
            return Some(c);
        }
        if c.is_whitespace() || matches!(c, '$' | '€' | '£' | '¥' | '₹') || c.is_ascii_alphabetic()
        {
            if idx == 0 {
                break;
            }
            continue;
        }
        break;
    }
    None
}

fn has_dot_thousands_currency(chars: &[char], idx: usize) -> bool {
    let start = idx.saturating_sub(8);
    let prefix: String = chars[start..idx].iter().collect();
    let lower = prefix.to_ascii_lowercase();
    lower.contains("rp") || lower.contains("idr")
}

fn parse_token_slice(slice: &[char], sign: Option<char>, dot_thousands_hint: bool) -> Option<f64> {
    let mut raw: String = slice.iter().collect();
    while raw.ends_with(['.', ',']) {
        raw.pop();
    }
    if raw.is_empty() || !raw.chars().any(|c| c.is_ascii_digit()) {
        return None;
    }

    let normalized = normalize_numeric_token(&raw, dot_thousands_hint)?;
    let full = match sign {
        Some('-') => format!("-{normalized}"),
        _ => normalized,
    };
    full.parse::<f64>().ok().filter(|v| v.is_finite())
}

fn is_valid_thousands_groups(raw: &str, sep: char) -> bool {
    let parts: Vec<&str> = raw.split(sep).collect();
    if parts.len() < 2 {
        return false;
    }
    let first = parts[0];
    if first.is_empty() || first.len() > 3 || first.starts_with('0') {
        return false;
    }
    parts[1..].iter().all(|part| part.len() == 3)
}

fn normalize_numeric_token(raw: &str, dot_thousands_hint: bool) -> Option<String> {
    let dot_count = raw.matches('.').count();
    let comma_count = raw.matches(',').count();

    let clean = match (dot_count, comma_count) {
        (0, 0) => raw.to_string(),
        (d, c) if d > 0 && c > 0 => {
            let last_dot = raw.rfind('.').unwrap();
            let last_comma = raw.rfind(',').unwrap();
            if last_dot > last_comma {
                raw.replace(',', "")
            } else {
                raw.replace('.', "").replace(',', ".")
            }
        }
        (0, 1) => {
            let (before, after) = raw.split_once(',')?;
            if after.len() == 3 && before != "0" && !before.is_empty() {
                raw.replace(',', "")
            } else {
                raw.replace(',', ".")
            }
        }
        (0, c) if c > 1 => {
            if is_valid_thousands_groups(raw, ',') {
                raw.replace(',', "")
            } else {
                return None;
            }
        }
        (1, 0) => {
            if dot_thousands_hint {
                let (before, after) = raw.split_once('.')?;
                if after.len() == 3 && before != "0" && !before.is_empty() {
                    raw.replace('.', "")
                } else {
                    raw.to_string()
                }
            } else {
                raw.to_string()
            }
        }
        (d, 0) if d > 1 => {
            if is_valid_thousands_groups(raw, '.') {
                raw.replace('.', "")
            } else {
                return None;
            }
        }
        _ => raw.to_string(),
    };

    Some(clean)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_parse_numeric_formats() {
        assert_eq!(parse_numeric("$1,299.99"), Some(1299.99));
        assert_eq!(parse_numeric("Price: $1,299.99"), Some(1299.99));
        assert_eq!(parse_numeric("Rp 1.500.000"), Some(1_500_000.0));
        assert_eq!(parse_numeric("Rp1.500.000"), Some(1_500_000.0));
        assert_eq!(parse_numeric("Rp 45.000"), Some(45_000.0));
        assert_eq!(parse_numeric("EUR 49,90"), Some(49.90));
        assert_eq!(parse_numeric("-$45.50"), Some(-45.50));
        assert_eq!(parse_numeric("-12.5"), Some(-12.5));
        assert_eq!(parse_numeric("99.95% uptime"), Some(99.95));
        assert_eq!(parse_numeric("0,125 kg"), Some(0.125));
        assert_eq!(parse_numeric("In Stock"), None);
    }

    #[test]
    fn test_parse_numeric_ignores_versions_ips_dates_and_identifiers() {
        assert_eq!(parse_numeric("v0.1.0"), None);
        assert_eq!(parse_numeric("Release 2.14.3"), None);
        assert_eq!(parse_numeric("127.0.0.1"), None);
        assert_eq!(parse_numeric("2026.11.26"), None);
        assert_eq!(parse_numeric("h1 heading"), None);
        assert_eq!(parse_numeric("sha256"), None);
        assert_eq!(parse_numeric("v0.1.0 costs $49.99"), Some(49.99));
        assert_eq!(parse_numeric("Release 2.3.4 costs $49.99"), Some(49.99));
    }

    #[test]
    fn test_compute_delta() {
        let d = compute_delta(200.0, 150.0);
        assert_eq!(d.diff, -50.0);
        assert_eq!(d.percent_change, Some(-25.0));
    }
}
