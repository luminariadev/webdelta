use std::collections::BTreeMap;

use regex::Regex;
use scraper::{ElementRef, Html, Node, Selector};
use serde_json::Value;
use serde_json_path::JsonPath;

use crate::config::{ExtractorConfig, NormalizeConfig};
use crate::error::{Error, Result};

const SKIPPED_TAGS: &[&str] = &["script", "style", "noscript", "template"];
const BLOCK_TAGS: &[&str] = &[
    "address",
    "article",
    "aside",
    "blockquote",
    "br",
    "dd",
    "div",
    "dl",
    "dt",
    "fieldset",
    "figcaption",
    "figure",
    "footer",
    "form",
    "h1",
    "h2",
    "h3",
    "h4",
    "h5",
    "h6",
    "header",
    "hr",
    "li",
    "main",
    "nav",
    "ol",
    "p",
    "pre",
    "section",
    "table",
    "td",
    "th",
    "tr",
    "ul",
];

/// Extracts, filters, and normalizes content from a fetched HTTP response.
pub fn extract_content(
    body: &str,
    status: u16,
    headers: &BTreeMap<String, String>,
    extractor: &ExtractorConfig,
    ignore_regex: &[String],
    normalize: &NormalizeConfig,
) -> Result<String> {
    let raw = match extractor {
        ExtractorConfig::Text => extract_text(body),
        ExtractorConfig::Css {
            selector,
            attribute,
            all,
        } => extract_css(body, selector, attribute.as_deref(), *all)?,
        ExtractorConfig::JsonPath { path } => extract_json_path(body, path)?,
        ExtractorConfig::Regex { pattern, group } => extract_regex(body, pattern, *group)?,
        ExtractorConfig::Status { header } => extract_status(status, headers, header.as_deref()),
    };

    let filtered = apply_ignore_regex(raw, ignore_regex)?;
    Ok(normalize_text(&filtered, normalize))
}

fn extract_text(body: &str) -> String {
    if !looks_like_html(body) {
        return body.to_string();
    }
    let doc = Html::parse_document(body);
    let mut out = String::new();
    collect_node_text(doc.root_element(), &mut out);
    out
}

fn extract_css(
    body: &str,
    selector_str: &str,
    attribute: Option<&str>,
    all: bool,
) -> Result<String> {
    let selector = Selector::parse(selector_str)
        .map_err(|err| Error::Extract(format!("invalid CSS selector '{selector_str}': {err:?}")))?;
    let doc = Html::parse_document(body);
    let mut results = Vec::new();

    for element in doc.select(&selector) {
        let piece = match attribute {
            Some(attr) => element
                .value()
                .attr(attr)
                .map(str::to_string)
                .ok_or_else(|| {
                    Error::Extract(format!(
                        "attribute '{attr}' not present on element matched by '{selector_str}'"
                    ))
                })?,
            None => {
                let mut buf = String::new();
                collect_node_text(element, &mut buf);
                buf
            }
        };
        if !piece.trim().is_empty() || attribute.is_some() {
            results.push(piece);
        }
        if !all {
            break;
        }
    }

    if results.is_empty() {
        return Err(Error::Extract(format!(
            "CSS selector '{selector_str}' matched no elements"
        )));
    }

    Ok(results.join("\n"))
}

fn extract_json_path(body: &str, path_str: &str) -> Result<String> {
    let value: Value = serde_json::from_str(body)
        .map_err(|err| Error::Extract(format!("response body is not valid JSON: {err}")))?;
    let path = JsonPath::parse(path_str)
        .map_err(|err| Error::Extract(format!("invalid JSONPath '{path_str}': {err}")))?;
    let nodes = path.query(&value).all();
    if nodes.is_empty() {
        return Err(Error::Extract(format!(
            "JSONPath '{path_str}' matched no nodes"
        )));
    }

    let formatted: Vec<String> = nodes.into_iter().map(format_json_value).collect();
    Ok(formatted.join("\n"))
}

fn format_json_value(val: &Value) -> String {
    match val {
        Value::Null => "null".to_string(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => s.clone(),
        Value::Array(_) | Value::Object(_) => {
            serde_json::to_string_pretty(val).unwrap_or_else(|_| val.to_string())
        }
    }
}

fn extract_regex(body: &str, pattern: &str, group: Option<usize>) -> Result<String> {
    let re = Regex::new(pattern)
        .map_err(|err| Error::Extract(format!("invalid regex '{pattern}': {err}")))?;
    let caps = re
        .captures(body)
        .ok_or_else(|| Error::Extract(format!("regex '{pattern}' did not match response body")))?;
    let idx = group.unwrap_or(0);
    let matched = caps.get(idx).ok_or_else(|| {
        Error::Extract(format!(
            "regex '{pattern}' matched, but capture group {idx} is missing"
        ))
    })?;
    Ok(matched.as_str().to_string())
}

fn extract_status(status: u16, headers: &BTreeMap<String, String>, header: Option<&str>) -> String {
    match header {
        Some(name) => {
            let key = name.to_ascii_lowercase();
            match headers.get(&key) {
                Some(val) => format!("{status} {key}: {val}"),
                None => format!("{status} {key}: <missing>"),
            }
        }
        None => status.to_string(),
    }
}

fn apply_ignore_regex(mut text: String, patterns: &[String]) -> Result<String> {
    for pattern in patterns {
        let re = Regex::new(pattern)
            .map_err(|err| Error::Extract(format!("invalid ignore_regex '{pattern}': {err}")))?;
        text = re.replace_all(&text, "").into_owned();
    }
    Ok(text)
}

/// Applies normalization rules line by line while preserving meaningful line breaks.
pub fn normalize_text(input: &str, normalize: &NormalizeConfig) -> String {
    let mut lines: Vec<String> = input
        .replace("\r\n", "\n")
        .split('\n')
        .map(|line| {
            let mut s = if normalize.collapse_whitespace {
                collapse_inline_spaces(line)
            } else {
                line.to_string()
            };
            if normalize.trim {
                s = s.trim().to_string();
            }
            if normalize.lowercase {
                s = s.to_lowercase();
            }
            s
        })
        .collect();

    if normalize.trim || normalize.collapse_whitespace {
        lines.retain(|line| !line.is_empty());
    }

    let joined = lines.join("\n");
    if normalize.trim {
        joined.trim().to_string()
    } else {
        joined
    }
}

fn collapse_inline_spaces(line: &str) -> String {
    let mut out = String::with_capacity(line.len());
    let mut prev_space = false;
    for ch in line.chars() {
        if ch.is_whitespace() {
            if !prev_space {
                out.push(' ');
                prev_space = true;
            }
        } else {
            out.push(ch);
            prev_space = false;
        }
    }
    out
}

fn looks_like_html(body: &str) -> bool {
    let sample = body.trim_start();
    let lower = sample
        .chars()
        .take(512)
        .collect::<String>()
        .to_ascii_lowercase();
    lower.starts_with("<!doctype html")
        || lower.starts_with("<html")
        || lower.contains("<body")
        || lower.contains("<div")
        || lower.contains("<head")
        || lower.contains("<p>")
        || lower.contains("<h1")
}

fn collect_node_text(element: ElementRef<'_>, out: &mut String) {
    for child in element.children() {
        match child.value() {
            Node::Text(text) => {
                if !text.is_empty() {
                    if !out.is_empty() && !out.ends_with([' ', '\n', '\t']) {
                        out.push(' ');
                    }
                    out.push_str(text);
                }
            }
            Node::Element(el) => {
                let tag = el.name().to_ascii_lowercase();
                if SKIPPED_TAGS.contains(&tag.as_str()) {
                    continue;
                }
                let is_block = BLOCK_TAGS.contains(&tag.as_str());
                if is_block && !out.is_empty() && !out.ends_with('\n') {
                    out.push('\n');
                }
                if let Some(child_el) = ElementRef::wrap(child) {
                    collect_node_text(child_el, out);
                }
                if is_block && !out.ends_with('\n') {
                    out.push('\n');
                }
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_css_extractor_skips_scripts_and_supports_all() {
        let html = r#"
        <html>
          <head><title>Store</title><script>var secret = 1;</script></head>
          <body>
            <div class="price">$199.00</div>
            <ul class="items"><li>Alpha</li><li>Beta</li></ul>
            <a class="cta" href="/buy">Buy</a>
          </body>
        </html>
        "#;
        let headers = BTreeMap::new();
        let norm = NormalizeConfig::default();

        let price = extract_content(
            html,
            200,
            &headers,
            &ExtractorConfig::Css {
                selector: ".price".to_string(),
                attribute: None,
                all: false,
            },
            &[],
            &norm,
        )
        .unwrap();
        assert_eq!(price, "$199.00");

        let items = extract_content(
            html,
            200,
            &headers,
            &ExtractorConfig::Css {
                selector: ".items li".to_string(),
                attribute: None,
                all: true,
            },
            &[],
            &norm,
        )
        .unwrap();
        assert_eq!(items, "Alpha\nBeta");

        let href = extract_content(
            html,
            200,
            &headers,
            &ExtractorConfig::Css {
                selector: "a.cta".to_string(),
                attribute: Some("href".to_string()),
                all: false,
            },
            &[],
            &norm,
        )
        .unwrap();
        assert_eq!(href, "/buy");

        let text =
            extract_content(html, 200, &headers, &ExtractorConfig::Text, &[], &norm).unwrap();
        assert!(!text.contains("var secret"));
        assert!(text.contains("$199.00"));
    }

    #[test]
    fn test_jsonpath_regex_status_and_ignore_filter() {
        let json = r#"{"product": {"name": "Keyboard", "price": 149.50}, "updated": "2026-11-26"}"#;
        let headers = BTreeMap::from([("x-version".to_string(), "v2".to_string())]);
        let norm = NormalizeConfig::default();

        let price = extract_content(
            json,
            200,
            &headers,
            &ExtractorConfig::JsonPath {
                path: "$.product.price".to_string(),
            },
            &[],
            &norm,
        )
        .unwrap();
        assert_eq!(price, "149.5");

        let regex_res = extract_content(
            "Build 421 (2026-11-26T10:00:00Z) OK",
            200,
            &headers,
            &ExtractorConfig::Regex {
                pattern: r"Build (\d+).*".to_string(),
                group: Some(1),
            },
            &[],
            &norm,
        )
        .unwrap();
        assert_eq!(regex_res, "421");

        let filtered = extract_content(
            "Server ready at 2026-11-26 12:00:00 UTC",
            200,
            &headers,
            &ExtractorConfig::Text,
            &[r"\d{4}-\d{2}-\d{2}\s+\d{2}:\d{2}:\d{2}\s+UTC".to_string()],
            &norm,
        )
        .unwrap();
        assert_eq!(filtered, "Server ready at");

        let status = extract_content(
            "",
            200,
            &headers,
            &ExtractorConfig::Status {
                header: Some("X-Version".to_string()),
            },
            &[],
            &norm,
        )
        .unwrap();
        assert_eq!(status, "200 x-version: v2");
    }
}
