use crate::outbound::b64_any;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

pub struct FetchFailure {
    pub url: String,
    pub reason: String,
}

pub struct FetchReport {
    pub texts: Vec<String>,
    pub failures: Vec<FetchFailure>,
}

const UA: &str = "Mozilla/5.0 (Windows NT 10.0; Win64; x64) AppleWebKit/537.36 (KHTML, like Gecko) Chrome/121.0.0.0 Safari/537.36";

fn maybe_decode(text: &str) -> String {
    if text.contains("://") {
        return text.to_string();
    }
    let compact: String = text.chars().filter(|c| !c.is_whitespace()).collect();
    if compact.len() > 64
        && compact
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '=' | '-' | '_'))
    {
        if let Some(bytes) = b64_any(&compact) {
            let decoded = String::from_utf8_lossy(&bytes);
            if decoded.contains("://") {
                return decoded.into_owned();
            }
        }
    }
    text.to_string()
}

async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<String, String> {
    let resp = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .map_err(|e| e.to_string())?;
    let status = resp.status();
    if !status.is_success() {
        return Err(format!("HTTP {status}"));
    }
    let bytes = resp
        .bytes()
        .await
        .map_err(|e| format!("ошибка чтения ответа: {e}"))?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Ok(maybe_decode(&text))
}

pub async fn fetch_all(urls: &[String], concurrency: usize, timeout_s: f64) -> FetchReport {
    if urls.is_empty() {
        return FetchReport {
            texts: Vec::new(),
            failures: Vec::new(),
        };
    }
    let client = reqwest::Client::builder()
        .user_agent(UA)
        .danger_accept_invalid_certs(true)
        .build()
        .unwrap_or_else(|_| reqwest::Client::new());
    let timeout = Duration::from_secs_f64(timeout_s.max(1.0));

    let pb = ProgressBar::new(urls.len() as u64).with_style(
        ProgressStyle::with_template(
            "{spinner:.green} загрузка источников {pos}/{len} [{elapsed_precise}<{eta_precise}]",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );

    let results: Vec<(String, Result<String, String>)> = stream::iter(urls.iter().cloned())
        .map(|url| {
            let client = client.clone();
            let pb = pb.clone();
            async move {
                let out = fetch_one(&client, &url, timeout).await;
                pb.inc(1);
                (url, out)
            }
        })
        .buffer_unordered(concurrency.max(1))
        .collect()
        .await;

    pb.finish_and_clear();
    let mut report = FetchReport {
        texts: Vec::new(),
        failures: Vec::new(),
    };
    for (url, result) in results {
        match result {
            Ok(text) => report.texts.push(text),
            Err(reason) => report.failures.push(FetchFailure { url, reason }),
        }
    }
    report
}

#[cfg(test)]
mod tests {
    use super::{fetch_all, fetch_one, maybe_decode};
    use base64::Engine;
    use std::io::{Read, Write};
    use std::net::{Shutdown, TcpListener};
    use std::thread;
    use std::time::Duration;

    fn serve_once(response: &'static [u8]) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 1024];
            let _ = stream.read(&mut request);
            let _ = stream.write_all(response);
            let _ = stream.shutdown(Shutdown::Both);
        });
        format!("http://{address}/test")
    }

    #[test]
    fn maybe_decode_keeps_plain_links_and_non_encoded_text() {
        assert_eq!(maybe_decode("vless://example"), "vless://example");
        assert_eq!(maybe_decode("this is plain text"), "this is plain text");
        assert_eq!(maybe_decode(&"A".repeat(80)), "A".repeat(80));
        assert_eq!(maybe_decode(&"A".repeat(65)), "A".repeat(65));
    }

    #[test]
    fn maybe_decode_decodes_base64_subscription() {
        let original = format!("vless://{}", "x".repeat(80));
        let encoded = base64::engine::general_purpose::STANDARD.encode(&original);
        assert_eq!(maybe_decode(&encoded), original);
    }

    #[tokio::test]
    async fn fetch_all_returns_successful_response_text() {
        let url = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world",
        );
        let report = fetch_all(&[url], 1, 2.0).await;
        assert_eq!(report.texts, ["hello world"]);
        assert!(report.failures.is_empty());
    }

    #[tokio::test]
    async fn fetch_all_collects_failed_urls_with_their_reasons() {
        let url = serve_once(
            b"HTTP/1.1 502 Bad Gateway\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let report = fetch_all(std::slice::from_ref(&url), 1, 2.0).await;

        assert!(report.texts.is_empty());
        assert_eq!(report.failures.len(), 1);
        assert_eq!(report.failures[0].url, url);
        assert_eq!(report.failures[0].reason, "HTTP 502 Bad Gateway");
    }

    #[tokio::test]
    async fn fetch_one_reports_connection_errors() {
        let client = reqwest::Client::new();
        let error = fetch_one(
            &client,
            "http://127.0.0.1:0/unreachable",
            Duration::from_secs(1),
        )
        .await
        .unwrap_err();

        assert!(!error.is_empty());
    }

    #[tokio::test]
    async fn fetch_one_reports_http_status_errors() {
        let url = serve_once(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
        );
        let client = reqwest::Client::new();
        let error = fetch_one(&client, &url, Duration::from_secs(2))
            .await
            .unwrap_err();
        assert_eq!(error, "HTTP 503 Service Unavailable");
    }

    #[tokio::test]
    async fn fetch_one_reports_response_body_read_errors() {
        let url =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\nshort");
        let client = reqwest::Client::new();
        let error = fetch_one(&client, &url, Duration::from_secs(2))
            .await
            .unwrap_err();
        assert!(error.starts_with("ошибка чтения ответа:"), "{error}");
    }

    #[tokio::test]
    async fn fetch_all_handles_an_empty_url_list_without_network_access() {
        let report = fetch_all(&[], 0, 0.0).await;
        assert!(report.texts.is_empty());
        assert!(report.failures.is_empty());
    }
}
