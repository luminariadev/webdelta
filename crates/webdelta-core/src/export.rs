use std::fmt::Write as _;
use std::str::FromStr;

use crate::config::WatchConfig;
use crate::error::{Error, Result};
use crate::store::SnapshotRecord;

/// Output formats supported when exporting snapshot history.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportFormat {
    Json,
    Csv,
    Rss,
}

impl ExportFormat {
    /// Returns the HTTP `Content-Type` header value for this export format.
    pub fn content_type(&self) -> &'static str {
        match self {
            Self::Json => "application/json; charset=utf-8",
            Self::Csv => "text/csv; charset=utf-8",
            Self::Rss => "application/rss+xml; charset=utf-8",
        }
    }

    /// Returns the file extension for downloads.
    pub fn extension(&self) -> &'static str {
        match self {
            Self::Json => "json",
            Self::Csv => "csv",
            Self::Rss => "xml",
        }
    }
}

impl FromStr for ExportFormat {
    type Err = Error;

    fn from_str(raw: &str) -> Result<Self> {
        match raw.trim().to_ascii_lowercase().as_str() {
            "json" => Ok(Self::Json),
            "csv" => Ok(Self::Csv),
            "rss" | "xml" => Ok(Self::Rss),
            other => Err(Error::Validation(format!(
                "unsupported export format '{other}': expected json, csv, or rss"
            ))),
        }
    }
}

/// Serializes snapshot history into JSON, RFC 4180 CSV, or RSS 2.0 XML.
pub fn export_history(
    watch: Option<&WatchConfig>,
    watch_id: &str,
    records: &[SnapshotRecord],
    format: ExportFormat,
) -> Result<String> {
    match format {
        ExportFormat::Json => serde_json::to_string_pretty(records)
            .map_err(|err| Error::Storage(format!("failed to serialize JSON export: {err}"))),
        ExportFormat::Csv => Ok(export_csv(records)),
        ExportFormat::Rss => Ok(export_rss(watch, watch_id, records)),
    }
}

fn export_csv(records: &[SnapshotRecord]) -> String {
    let mut out = String::from(
        "checked_at,status_code,elapsed_ms,numeric_value,content_hash,extracted_text\n",
    );
    for rec in records {
        let checked = rec.checked_at.to_rfc3339();
        let numeric = rec.numeric_value.map(|n| n.to_string()).unwrap_or_default();
        let _ = writeln!(
            out,
            "{},{},{},{},{},{}",
            csv_escape(&checked),
            rec.status_code,
            rec.elapsed_ms,
            csv_escape(&numeric),
            csv_escape(&rec.content_hash),
            csv_escape(&rec.extracted_text)
        );
    }
    out
}

fn csv_escape(field: &str) -> String {
    if field.contains([',', '"', '\n', '\r']) {
        let escaped = field.replace('"', "\"\"");
        format!("\"{escaped}\"")
    } else {
        field.to_string()
    }
}

fn export_rss(watch: Option<&WatchConfig>, watch_id: &str, records: &[SnapshotRecord]) -> String {
    let title = watch.map(|w| w.display_name()).unwrap_or(watch_id);
    let link = watch
        .map(|w| w.url.as_str())
        .unwrap_or("https://github.com/luminariadev/webdelta");

    let mut out = String::from(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<rss version=\"2.0\">\n  <channel>\n",
    );
    let _ = writeln!(out, "    <title>{}</title>", xml_escape(title));
    let _ = writeln!(out, "    <link>{}</link>", xml_escape(link));
    let _ = writeln!(
        out,
        "    <description>{}</description>",
        xml_escape(&format!("Snapshot history for {watch_id}"))
    );

    for rec in records {
        let hash_short = &rec.content_hash[..rec.content_hash.len().min(8)];
        let item_title = match rec.numeric_value {
            Some(num) => format!(
                "{title} (HTTP {}, value {num}, {hash_short})",
                rec.status_code
            ),
            None => format!("{title} (HTTP {}, {hash_short})", rec.status_code),
        };
        let guid = format!(
            "{}:{}:{}",
            watch_id,
            rec.content_hash,
            rec.checked_at.timestamp_millis()
        );
        let _ = writeln!(out, "    <item>");
        let _ = writeln!(out, "      <title>{}</title>", xml_escape(&item_title));
        let _ = writeln!(out, "      <link>{}</link>", xml_escape(link));
        let _ = writeln!(
            out,
            "      <guid isPermaLink=\"false\">{}</guid>",
            xml_escape(&guid)
        );
        let _ = writeln!(
            out,
            "      <pubDate>{}</pubDate>",
            xml_escape(&rec.checked_at.to_rfc2822())
        );
        let _ = writeln!(
            out,
            "      <description>{}</description>",
            xml_escape(&rec.extracted_text)
        );
        let _ = writeln!(out, "    </item>");
    }

    out.push_str("  </channel>\n</rss>\n");
    out
}

fn xml_escape(input: &str) -> String {
    let mut escaped = String::with_capacity(input.len());
    for ch in input.chars() {
        match ch {
            '&' => escaped.push_str("&amp;"),
            '<' => escaped.push_str("&lt;"),
            '>' => escaped.push_str("&gt;"),
            '"' => escaped.push_str("&quot;"),
            '\'' => escaped.push_str("&apos;"),
            other => escaped.push(other),
        }
    }
    escaped
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::DateTime;

    fn sample_records() -> Vec<SnapshotRecord> {
        vec![SnapshotRecord {
            watch_id: "demo".to_string(),
            checked_at: DateTime::from_timestamp_millis(1_700_000_000_000).unwrap(),
            status_code: 200,
            etag: None,
            last_modified: None,
            content_hash: "abcdef1234567890".to_string(),
            extracted_text: "Price \"Pro\", Tier 1\nLine 2 & <tag>".to_string(),
            numeric_value: Some(149.5),
            elapsed_ms: 15,
        }]
    }

    #[test]
    fn test_export_formats_and_escaping() {
        let records = sample_records();

        let json_out = export_history(None, "demo", &records, ExportFormat::Json).unwrap();
        let parsed: serde_json::Value = serde_json::from_str(&json_out).unwrap();
        assert_eq!(parsed.as_array().unwrap().len(), 1);

        let csv_out = export_history(None, "demo", &records, ExportFormat::Csv).unwrap();
        assert!(csv_out.starts_with(
            "checked_at,status_code,elapsed_ms,numeric_value,content_hash,extracted_text\n"
        ));
        assert!(csv_out.contains("\"Price \"\"Pro\"\", Tier 1\nLine 2 & <tag>\""));

        let rss_out = export_history(None, "demo", &records, ExportFormat::Rss).unwrap();
        assert!(rss_out.contains("<rss version=\"2.0\">"));
        assert!(rss_out.contains("Price &quot;Pro&quot;, Tier 1\nLine 2 &amp; &lt;tag&gt;"));
    }
}
