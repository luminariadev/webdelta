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

/// Extracts the first numeric or currency token from arbitrary text.
///
/// Handles US/international (`$1,299.99`), Indonesian/European (`Rp 1.500.000`,
/// `EUR 49,90`), signed numbers (`-12.5`), and percentages (`99.95%`).
pub fn parse_numeric(input: &str) -> Option<f64> {
    let chars: Vec<char> = input.chars().collect();
    let len = chars.len();
    let mut i = 0;

    while i < len {
        let ch = chars[i];
        let is_sign = ch == '-' || ch == '+';

        if ch.is_ascii_digit() {
            let sign = find_preceding_sign(&chars, i);
            let dot_thousands_hint = has_dot_thousands_currency(&chars, i);
            return parse_token_at(&chars, i, sign, dot_thousands_hint);
        }

        if is_sign && i + 1 < len && chars[i + 1].is_ascii_digit() {
            let dot_thousands_hint = has_dot_thousands_currency(&chars, i);
            return parse_token_at(&chars, i + 1, Some(ch), dot_thousands_hint);
        }

        i += 1;
    }

    None
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

fn parse_token_at(
    chars: &[char],
    start: usize,
    sign: Option<char>,
    dot_thousands_hint: bool,
) -> Option<f64> {
    let mut end = start;
    while end < chars.len() {
        let c = chars[end];
        if c.is_ascii_digit() || c == '.' || c == ',' {
            end += 1;
        } else {
            break;
        }
    }

    let mut raw: String = chars[start..end].iter().collect();
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
        (0, c) if c > 1 => raw.replace(',', ""),
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
        (d, 0) if d > 1 => raw.replace('.', ""),
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
        assert_eq!(parse_numeric("Rp 1.500.000"), Some(1_500_000.0));
        assert_eq!(parse_numeric("Rp 45.000"), Some(45_000.0));
        assert_eq!(parse_numeric("EUR 49,90"), Some(49.90));
        assert_eq!(parse_numeric("-$45.50"), Some(-45.50));
        assert_eq!(parse_numeric("-12.5"), Some(-12.5));
        assert_eq!(parse_numeric("99.95% uptime"), Some(99.95));
        assert_eq!(parse_numeric("0,125 kg"), Some(0.125));
        assert_eq!(parse_numeric("In Stock"), None);
    }

    #[test]
    fn test_compute_delta() {
        let d = compute_delta(200.0, 150.0);
        assert_eq!(d.diff, -50.0);
        assert_eq!(d.percent_change, Some(-25.0));
    }
}
