use crate::check::socks_client;
use rand::seq::SliceRandom;
use std::time::Duration;

pub const DEFAULT_TARGETS: &[&str] = &[
    "https://speed.cloudflare.com/__down?bytes=10000000",
    "https://proof.ovh.net/files/100Mb.dat",
    "http://speedtest.tele2.net/100MB.zip",
    "https://speed.hetzner.de/100MB.bin",
    "https://mirror.leaseweb.com/speedtest/100mb.bin",
    "https://yandex.ru/internet/api/v0/measure/download?size=10000000",
];

async fn try_download(
    client: &reqwest::Client,
    url: &str,
    _connect_timeout: Duration,
    timeout: Duration,
    max_bytes: u64,
) -> Option<(u64, f64)> {
    let resp = client
        .get(url)
        .timeout(timeout)
        .send()
        .await
        .ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let start = std::time::Instant::now();
    let mut resp = resp;
    let mut total: u64 = 0;
    loop {
        if start.elapsed() > timeout || total >= max_bytes {
            break;
        }
        let remaining = timeout.saturating_sub(start.elapsed());
        if remaining.is_zero() {
            break;
        }
        match tokio::time::timeout(remaining, resp.chunk()).await {
            Ok(Ok(Some(chunk))) => total += chunk.len() as u64,
            Ok(Ok(None)) => break,
            Ok(Err(_)) => break,
            Err(_) => break,
        }
    }
    if total == 0 {
        return None;
    }
    let secs = start.elapsed().as_secs_f64().max(0.1);
    Some((total, secs))
}

/// Замер скорости через socks. Возвращает KB/s (0.0 = мусор/недоступно).
pub async fn measure(
    port: u16,
    primary: Option<&str>,
    conn_timeout_s: f64,
    timeout_s: f64,
    max_mb: f64,
) -> f64 {
    let conn_timeout = Duration::from_secs_f64(conn_timeout_s.max(1.0));
    let timeout = Duration::from_secs_f64(timeout_s.max(1.0));
    let max_bytes = ((max_mb.max(0.5)) * 1024.0 * 1024.0) as u64;
    let client = match socks_client(port, conn_timeout) {
        Ok(c) => c,
        Err(_) => return 0.0,
    };

    let mut pool: Vec<String> = Vec::new();
    if let Some(u) = primary {
        if !u.is_empty() {
            pool.push(u.to_string());
        }
    }
    let mut targets: Vec<String> = DEFAULT_TARGETS.iter().map(|s| s.to_string()).collect();
    if primary.is_none() {
        let mut rng = rand::thread_rng();
        targets.shuffle(&mut rng);
    }
    pool.extend(targets);

    for url in &pool {
        if let Some((total, secs)) =
            try_download(&client, url, conn_timeout, timeout, max_bytes).await
        {
            if total < 1024 {
                continue;
            }
            let kbs = (total as f64 / 1024.0) / secs;
            if kbs <= 0.0 {
                continue;
            }
            return kbs;
        }
    }
    0.0
}
