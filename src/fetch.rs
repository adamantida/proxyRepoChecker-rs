use crate::outbound::b64_any;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use std::time::Duration;

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
) -> Option<String> {
    let resp = client.get(url).timeout(timeout).send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let bytes = resp.bytes().await.ok()?;
    let text = String::from_utf8_lossy(&bytes).into_owned();
    Some(maybe_decode(&text))
}

pub async fn fetch_all(urls: &[String], concurrency: usize, timeout_s: f64) -> Vec<String> {
    if urls.is_empty() {
        return Vec::new();
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

    let results: Vec<String> = stream::iter(urls.iter().cloned())
        .map(|url| {
            let client = client.clone();
            let pb = pb.clone();
            async move {
                let out = fetch_one(&client, &url, timeout).await;
                pb.inc(1);
                out
            }
        })
        .buffer_unordered(concurrency.max(1))
        .filter_map(|x| async move { x })
        .collect()
        .await;

    pb.finish_and_clear();
    results
}
