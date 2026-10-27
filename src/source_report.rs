//! Data model for a source-by-source quality report.
//!
//! This module is intentionally independent of fetching and proxy validation. Callers
//! can create one [`SourceQuality`] per configured subscription and update its counts
//! as each pipeline stage completes.

use serde::{Deserialize, Serialize};

/// The furthest pipeline state reached by a source.
///
/// Counts remain independent of this status: a partially processed source can still
/// report its fetched, parsed, duplicate, or TCP-live totals.
#[derive(Clone, Copy, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceStatus {
    /// The source has been registered but fetching has not finished.
    #[default]
    Pending,
    /// Fetching failed before response text was available.
    FetchFailed,
    /// Response text was fetched successfully.
    Fetched,
    /// Proxy links from the response have been parsed.
    Parsed,
    /// TCP reachability checks have completed.
    TcpChecked,
    /// Speed checks have completed.
    SpeedChecked,
}

/// Per-source item totals at each quality stage.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceCounts {
    /// Successful response bodies fetched for this source (normally zero or one).
    pub fetched: u64,
    /// Valid, supported proxy links parsed from the source, before deduplication.
    pub valid: u64,
    /// Valid links discarded because they duplicated an earlier link.
    pub duplicate: u64,
    /// Unique proxies that passed the TCP liveness check.
    pub tcp_live: u64,
    /// TCP-live proxies that met the configured speed threshold.
    pub speed_qualified: u64,
}

/// Quality information for one configured source.
///
/// `url` is sanitized at construction and contains no userinfo, query, fragment, or
/// non-root path. The report model never stores a response body or fetch error text.
#[derive(Clone, Debug, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceQuality {
    /// Sanitized source URL safe for inclusion in a report by default.
    url: String,
    pub status: SourceStatus,
    pub counts: SourceCounts,
}

impl SourceQuality {
    /// Create a source row, sanitizing its URL before retaining it.
    pub fn new(url: &str) -> Self {
        Self {
            url: redact_url(url),
            status: SourceStatus::Pending,
            counts: SourceCounts::default(),
        }
    }

    /// Return the sanitized URL retained by this row.
    #[cfg(test)]
    pub fn url(&self) -> &str {
        &self.url
    }
}

/// A collection of per-source quality rows.
#[derive(Clone, Debug, Default, Deserialize, Eq, PartialEq, Serialize)]
pub struct SourceQualityReport {
    pub sources: Vec<SourceQuality>,
}

impl SourceQualityReport {
    /// Create an empty report.
    pub fn new() -> Self {
        Self::default()
    }

    /// Add a source row and return it for updates as pipeline stages finish.
    pub fn add_source(&mut self, url: &str) -> &mut SourceQuality {
        self.sources.push(SourceQuality::new(url));
        self.sources.last_mut().expect("just pushed a source row")
    }

    /// Serialize to compact JSON with source rows sorted by sanitized URL.
    ///
    /// Sorting a cloned view makes output deterministic regardless of completion
    /// order, without changing the report's in-memory order.
    #[cfg(test)]
    pub fn to_json(&self) -> Result<String, serde_json::Error> {
        let mut sorted = self.clone();
        sorted.sources.sort_by(|a, b| {
            a.url
                .cmp(&b.url)
                .then_with(|| a.status.cmp(&b.status))
                .then_with(|| counts_key(&a.counts).cmp(&counts_key(&b.counts)))
        });
        serde_json::to_string(&sorted)
    }

    /// Serialize to pretty JSON with stable field and source ordering.
    pub fn to_pretty_json(&self) -> Result<String, serde_json::Error> {
        let mut sorted = self.clone();
        sorted.sources.sort_by(|a, b| {
            a.url
                .cmp(&b.url)
                .then_with(|| a.status.cmp(&b.status))
                .then_with(|| counts_key(&a.counts).cmp(&counts_key(&b.counts)))
        });
        serde_json::to_string_pretty(&sorted)
    }
}

impl Ord for SourceStatus {
    fn cmp(&self, other: &Self) -> std::cmp::Ordering {
        (*self as u8).cmp(&(*other as u8))
    }
}

impl PartialOrd for SourceStatus {
    fn partial_cmp(&self, other: &Self) -> Option<std::cmp::Ordering> {
        Some(self.cmp(other))
    }
}

fn counts_key(counts: &SourceCounts) -> (u64, u64, u64, u64, u64) {
    (
        counts.fetched,
        counts.valid,
        counts.duplicate,
        counts.tcp_live,
        counts.speed_qualified,
    )
}

/// Remove credentials and potentially sensitive URL components.
///
/// The scheme, host, and explicit port remain useful for identifying a feed. Paths
/// are replaced because providers sometimes place access tokens there. Invalid input
/// is replaced wholesale so malformed strings cannot leak credentials into JSON.
pub fn redact_url(input: &str) -> String {
    let Ok(mut url) = url::Url::parse(input) else {
        return "[redacted invalid URL]".to_owned();
    };

    if !matches!(url.scheme(), "http" | "https") || url.host_str().is_none() {
        return "[redacted unsupported URL]".to_owned();
    }

    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    if url.path() != "/" {
        url.set_path("/[redacted]");
    }
    url.to_string()
}

#[cfg(test)]
mod tests {
    use super::{redact_url, SourceQualityReport, SourceStatus};
    use serde_json::json;

    #[test]
    fn new_rows_have_pending_status_and_zero_counts() {
        let row = super::SourceQuality::new("https://feed.example/subscriptions");

        assert_eq!(row.url(), "https://feed.example/[redacted]");
        assert_eq!(row.status, SourceStatus::Pending);
        assert_eq!(row.counts.fetched, 0);
        assert_eq!(row.counts.valid, 0);
        assert_eq!(row.counts.duplicate, 0);
        assert_eq!(row.counts.tcp_live, 0);
        assert_eq!(row.counts.speed_qualified, 0);
    }

    #[test]
    fn url_redaction_removes_userinfo_query_fragment_and_path_tokens() {
        let redacted = redact_url(
            "https://alice:secret@feed.example:8443/private/token?key=query-secret#fragment-secret",
        );

        assert_eq!(redacted, "https://feed.example:8443/[redacted]");
        for secret in [
            "alice",
            "secret",
            "token",
            "query-secret",
            "fragment-secret",
        ] {
            assert!(!redacted.contains(secret));
        }
    }

    #[test]
    fn url_redaction_preserves_safe_root_urls_and_ipv6_hosts() {
        assert_eq!(redact_url("https://feed.example/"), "https://feed.example/");
        assert_eq!(
            redact_url("http://[::1]:8080/?token=secret"),
            "http://[::1]:8080/"
        );
    }

    #[test]
    fn url_redaction_replaces_malformed_and_non_http_urls() {
        assert_eq!(
            redact_url("not a url user:password"),
            "[redacted invalid URL]"
        );
        assert_eq!(
            redact_url("ftp://user:password@feed.example/file"),
            "[redacted unsupported URL]"
        );
        assert_eq!(
            redact_url("vless://uuid-secret@proxy.example:443"),
            "[redacted unsupported URL]"
        );
    }

    #[test]
    fn report_json_contains_expected_fields_and_counts_without_body_data() {
        let mut report = SourceQualityReport::new();
        let row = report.add_source("https://user:pass@feed.example/list?token=abc");
        row.status = SourceStatus::SpeedChecked;
        row.counts.fetched = 1;
        row.counts.valid = 12;
        row.counts.duplicate = 3;
        row.counts.tcp_live = 7;
        row.counts.speed_qualified = 4;

        let value: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
        assert_eq!(
            value,
            json!({
                "sources": [{
                    "url": "https://feed.example/[redacted]",
                    "status": "speed_checked",
                    "counts": {
                        "fetched": 1,
                        "valid": 12,
                        "duplicate": 3,
                        "tcp_live": 7,
                        "speed_qualified": 4
                    }
                }]
            })
        );
        let serialized = report.to_json().unwrap();
        assert!(!serialized.contains("pass"));
        assert!(!serialized.contains("abc"));
        assert!(!serialized.contains("response body"));
    }

    #[test]
    fn json_output_is_deterministic_and_sorted_without_reordering_report() {
        let mut report = SourceQualityReport::new();
        report.add_source("https://z.example/feed").counts.valid = 1;
        report.add_source("https://a.example/feed").counts.valid = 2;

        let json = report.to_json().unwrap();
        assert_eq!(json, report.to_json().unwrap());
        assert!(json.find("a.example").unwrap() < json.find("z.example").unwrap());
        assert_eq!(report.sources[0].url(), "https://z.example/[redacted]");

        let pretty = report.to_pretty_json().unwrap();
        assert!(pretty.contains("\n  \"sources\": ["));
        assert!(pretty.find("a.example").unwrap() < pretty.find("z.example").unwrap());
    }

    #[test]
    fn equal_redacted_urls_are_sorted_by_status_then_counts() {
        let mut report = SourceQualityReport::new();
        let higher = report.add_source("https://feed.example/second");
        higher.status = SourceStatus::SpeedChecked;
        higher.counts.valid = 9;
        let parsed = report.add_source("https://feed.example/first");
        parsed.status = SourceStatus::Fetched;
        parsed.counts.valid = 100;
        let lower = report.add_source("https://feed.example/third");
        lower.status = SourceStatus::SpeedChecked;
        lower.counts.valid = 2;

        let value: serde_json::Value = serde_json::from_str(&report.to_json().unwrap()).unwrap();
        let rows = value["sources"].as_array().unwrap();

        assert_eq!(rows[0]["status"], "fetched");
        assert_eq!(rows[1]["status"], "speed_checked");
        assert_eq!(rows[1]["counts"]["valid"], 2);
        assert_eq!(rows[2]["counts"]["valid"], 9);
        assert_eq!(report.sources[0].counts.valid, 9);
        assert_eq!(report.sources[1].status, SourceStatus::Fetched);
    }

    #[test]
    fn pretty_json_breaks_equal_url_and_status_ties_by_counts() {
        let mut report = SourceQualityReport::new();
        let larger = report.add_source("https://feed.example/first");
        larger.status = SourceStatus::Fetched;
        larger.counts.valid = 9;
        let smaller = report.add_source("https://feed.example/second");
        smaller.status = SourceStatus::Fetched;
        smaller.counts.valid = 2;

        let pretty = report.to_pretty_json().unwrap();
        let value: serde_json::Value = serde_json::from_str(&pretty).unwrap();
        let rows = value["sources"].as_array().unwrap();

        assert_eq!(rows[0]["counts"]["valid"], 2);
        assert_eq!(rows[1]["counts"]["valid"], 9);
        assert_eq!(report.sources[0].counts.valid, 9);
    }

    #[test]
    fn empty_report_serializes_to_stable_empty_collection() {
        let report = SourceQualityReport::new();
        assert_eq!(report.to_json().unwrap(), r#"{"sources":[]}"#);
        assert_eq!(report.to_pretty_json().unwrap(), "{\n  \"sources\": []\n}");
    }

    #[test]
    fn status_serialization_names_are_stable_snake_case() {
        let statuses = [
            (SourceStatus::Pending, "pending"),
            (SourceStatus::FetchFailed, "fetch_failed"),
            (SourceStatus::Fetched, "fetched"),
            (SourceStatus::Parsed, "parsed"),
            (SourceStatus::TcpChecked, "tcp_checked"),
            (SourceStatus::SpeedChecked, "speed_checked"),
        ];

        for (status, expected) in statuses {
            assert_eq!(
                serde_json::to_string(&status).unwrap(),
                format!("\"{expected}\"")
            );
        }
    }

    #[test]
    fn status_ordering_is_total_and_equal_values_compare_equal() {
        let statuses = [
            SourceStatus::Pending,
            SourceStatus::FetchFailed,
            SourceStatus::Fetched,
            SourceStatus::Parsed,
            SourceStatus::TcpChecked,
            SourceStatus::SpeedChecked,
        ];

        for (index, status) in statuses.iter().enumerate() {
            assert_eq!(status.cmp(status), std::cmp::Ordering::Equal);
            assert_eq!(status.partial_cmp(status), Some(std::cmp::Ordering::Equal));
            for later in &statuses[index + 1..] {
                assert_eq!(status.cmp(later), std::cmp::Ordering::Less);
                assert_eq!(later.cmp(status), std::cmp::Ordering::Greater);
            }
        }
    }
}
