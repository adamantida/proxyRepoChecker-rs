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

async fn resolve(
    host: &str,
    port: u16,
    cache: &Arc<tokio::sync::Mutex<HashMap<String, Vec<SocketAddr>>>>,
) -> Option<Vec<SocketAddr>> {
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

fn filter_by_max_ms(rtt: u64, max_ms: u64) -> Option<u64> {
    if max_ms > 0 && rtt > max_ms {
        None
    } else {
        Some(rtt)
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
            if let Ok(Ok(_stream)) = res {
                let rtt = start.elapsed().as_millis() as u64;
                best = Some(match best {
                    Some(b) => b.min(rtt),
                    None => rtt,
                });
            }
        }
        if best.is_some() {
            break;
        }
    }
    let rtt = best?;
    filter_by_max_ms(rtt, max_ms)
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

#[cfg(test)]
mod tests {
    use super::*;

    fn proxy(port: u16) -> Proxy {
        Proxy {
            proto: "vless",
            host: "127.0.0.1".into(),
            port,
            name: "local".into(),
            base_url: String::new(),
            orig_url: String::new(),
            settings: serde_json::Value::Null,
            stream: None,
            country: String::new(),
            display: String::new(),
            ping: None,
            speed_kbs: 0.0,
        }
    }

    #[test]
    fn max_ping_threshold_accepts_unlimited_and_inclusive_limit() {
        assert_eq!(filter_by_max_ms(0, 0), Some(0));
        assert_eq!(filter_by_max_ms(25, 0), Some(25));
        assert_eq!(filter_by_max_ms(25, 25), Some(25));
        assert_eq!(filter_by_max_ms(26, 25), None);
    }

    #[tokio::test]
    async fn filter_connects_to_local_listener_and_reports_dead_port_statistics() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live_port = listener.local_addr().unwrap().port();
        let dead_reservation = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let dead_port = dead_reservation.local_addr().unwrap().port();
        drop(dead_reservation);

        let accepted = tokio::spawn(async move {
            let (_stream, _) = listener.accept().await.unwrap();
        });
        let (alive, dead, stats) =
            filter(vec![proxy(live_port), proxy(dead_port)], 1.0, 2, 3, 0).await;
        accepted.await.unwrap();

        assert_eq!((stats.total, stats.alive, stats.dead), (2, 1, 1));
        assert_eq!(stats.max_ms_seen, alive[0].ping.unwrap());
        assert!(alive[0].ping.is_some());
        assert_eq!(alive[0].port, live_port);
        assert_eq!(dead.len(), 1);
        assert_eq!(dead[0].port, dead_port);
        assert_eq!(dead[0].ping, None);
    }

    #[tokio::test]
    async fn filter_handles_empty_input_with_zeroed_statistics() {
        let (alive, dead, stats) = filter(Vec::new(), 0.1, 0, 0, 0).await;
        assert!(alive.is_empty());
        assert!(dead.is_empty());
        assert_eq!(stats.total, 0);
        assert_eq!(stats.alive, 0);
        assert_eq!(stats.dead, 0);
        assert_eq!(stats.max_ms_seen, 0);
    }

    #[tokio::test]
    async fn resolve_uses_cached_positive_and_negative_lookups() {
        let cache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let expected = "127.0.0.1:8080".parse::<SocketAddr>().unwrap();
        cache
            .lock()
            .await
            .insert("cached.example".into(), vec![expected]);
        cache
            .lock()
            .await
            .insert("missing.example".into(), Vec::new());

        assert_eq!(
            resolve("cached.example", 1, &cache).await,
            Some(vec![expected])
        );
        assert_eq!(resolve("missing.example", 1, &cache).await, None);
    }

    #[tokio::test]
    async fn resolve_looks_up_hostname_and_caches_results() {
        let cache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));

        let first = resolve("localhost", 80, &cache).await.unwrap();
        assert!(!first.is_empty());
        assert_eq!(resolve("localhost", 80, &cache).await, Some(first));
        assert!(cache.lock().await.contains_key("localhost"));
    }

    #[tokio::test]
    async fn resolve_caches_invalid_hostname_as_negative_result() {
        let cache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let invalid_host = "bad\0host";

        assert_eq!(resolve(invalid_host, 80, &cache).await, None);
        assert_eq!(cache.lock().await.get(invalid_host), Some(&Vec::new()));
        assert_eq!(resolve(invalid_host, 80, &cache).await, None);
    }

    #[tokio::test]
    async fn probe_checks_each_cached_address_and_keeps_the_best_rtt() {
        let first = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let second = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let first_addr = first.local_addr().unwrap();
        let second_addr = second.local_addr().unwrap();
        let first_accept = tokio::spawn(async move { first.accept().await.unwrap() });
        let second_accept = tokio::spawn(async move { second.accept().await.unwrap() });
        let mut sample = proxy(0);
        sample.host = "cached-multi-address.example".into();
        let cache = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
            sample.host.clone(),
            vec![first_addr, second_addr],
        )])));

        let rtt = probe(&sample, Duration::from_secs(1), 1, 0, &cache).await;

        assert!(rtt.is_some());
        first_accept.await.unwrap();
        second_accept.await.unwrap();
    }

    #[tokio::test]
    async fn probe_returns_none_for_a_cached_negative_host_lookup() {
        let mut sample = proxy(443);
        sample.host = "cached-negative.example".into();
        let cache = Arc::new(tokio::sync::Mutex::new(HashMap::from([(
            sample.host.clone(),
            Vec::new(),
        )])));

        assert_eq!(
            probe(&sample, Duration::from_secs(1), 1, 0, &cache).await,
            None
        );
    }
}
