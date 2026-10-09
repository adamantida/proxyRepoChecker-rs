use crate::parse::Proxy;
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::net::SocketAddr;
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct TcpStats {
    pub total: usize,
    pub alive: usize,
    pub dead: usize,
    pub max_ms_seen: u64,
}

async fn resolve(host: &str, port: u16, cache: &Arc<tokio::sync::Mutex<HashMap<String, Vec<SocketAddr>>>>) -> Option<Vec<SocketAddr>> {
    if let Ok(addr) = host.parse::<std::net::IpAddr>() {
        return Some(vec![SocketAddr::new(addr, port)]);
    }
    {
        let c = cache.lock().await;
        if let Some(addrs) = c.get(host) {
            if addrs.is_empty() {
                return None;
            }
            return Some(addrs.clone());
        }
    }
    let addrs: Vec<SocketAddr> = match tokio::net::lookup_host((host, port)).await {
        Ok(it) => it.collect(),
        Err(_) => Vec::new(),
    };
    let mut c = cache.lock().await;
    c.insert(host.to_string(), addrs.clone());
    if addrs.is_empty() {
        None
    } else {
        Some(addrs)
    }
}

async fn probe(
    proxy: &Proxy,
    timeout: Duration,
    retries: u32,
    max_ms: u64,
    cache: &Arc<tokio::sync::Mutex<HashMap<String, Vec<SocketAddr>>>>,
) -> Option<u64> {
    let addrs = resolve(&proxy.host, proxy.port, cache).await?;
    let attempts = retries.max(1);
    let mut best: Option<u64> = None;
    for _ in 0..attempts {
        for addr in &addrs {
            let start = Instant::now();
            let res = tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr)).await;
            match res {
                Ok(Ok(_stream)) => {
                    let rtt = start.elapsed().as_millis() as u64;
                    best = Some(match best {
                        Some(b) => b.min(rtt),
                        None => rtt,
                    });
                }
                _ => {}
            }
        }
        if best.is_some() {
            break;
        }
    }
    let rtt = best?;
    if max_ms > 0 && rtt > max_ms {
        return None;
    }
    Some(rtt)
}

/// TCP ping префильтр: возвращает (живые, мёртвые, статистику).
pub async fn filter(
    proxies: Vec<Proxy>,
    timeout_s: f64,
    concurrency: usize,
    retries: u32,
    max_ms: u64,
) -> (Vec<Proxy>, Vec<Proxy>, TcpStats) {
    let total = proxies.len();
    let timeout = Duration::from_secs_f64(timeout_s.max(0.1));
    let cache: Arc<tokio::sync::Mutex<HashMap<String, Vec<SocketAddr>>>> =
        Arc::new(tokio::sync::Mutex::new(HashMap::new()));

    let results: Vec<(Proxy, Option<u64>)> = {
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let pb = indicatif::ProgressBar::new(total as u64).with_style(
            indicatif::ProgressStyle::with_template(
                "{spinner:.green} TCP ping {pos}/{len} [{elapsed_precise}<{eta_precise}] живых: {msg}",
            )
            .unwrap_or_else(|_| indicatif::ProgressStyle::default_bar()),
        );
        let alive_count = Arc::new(AtomicUsize::new(0));
        let out: Vec<(Proxy, Option<u64>)> = stream::iter(proxies)
            .map(|p| {
                let cache = Arc::clone(&cache);
                let pb = pb.clone();
                let alive_count = Arc::clone(&alive_count);
                async move {
                    let rtt = probe(&p, timeout, retries, max_ms, &cache).await;
                    if rtt.is_some() {
                        alive_count.fetch_add(1, AOrdering::Relaxed);
                        pb.set_message(alive_count.load(AOrdering::Relaxed).to_string());
                    }
                    pb.inc(1);
                    (p, rtt)
                }
            })
            .buffer_unordered(concurrency.max(1))
            .collect()
            .await;
        pb.finish_and_clear();
        out
    };

    let mut alive = Vec::new();
    let mut dead = Vec::new();
    let mut max_seen = 0u64;
    for (mut p, rtt) in results {
        match rtt {
            Some(ms) => {
                max_seen = max_seen.max(ms);
                p.ping = Some(ms);
                alive.push(p);
            }
            None => dead.push(p),
        }
    }
    let stats = TcpStats {
        total,
        alive: alive.len(),
        dead: dead.len(),
        max_ms_seen: max_seen,
    };
    (alive, dead, stats)
}
