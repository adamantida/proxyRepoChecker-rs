use crate::parse::Proxy;
use futures::stream::{self, StreamExt};
use std::collections::HashMap;
use std::future::Future;
use std::net::{IpAddr, SocketAddr, ToSocketAddrs};
use std::sync::Arc;
use std::time::{Duration, Instant};

#[derive(Debug, Default)]
pub struct TcpStats {
    /// Все прокси, попавшие в фазу (включая UDP-прокси без TCP-проверки).
    pub total: usize,
    /// Сколько прокси реально проверено TCP-коннектом (`total - udp_skipped`).
    pub checked: usize,
    /// Все выжившие: TCP-живые + UDP-прокси без проверки (столько же в `alive.txt`).
    pub alive: usize,
    /// Выжившие именно после TCP-проверки.
    pub tcp_alive: usize,
    pub dead: usize,
    pub udp_skipped: usize,
    pub max_ms_seen: u64,
}

/// Hysteria работает поверх UDP, поэтому TCP-коннект к её порту ничего не
/// проверяет: такие узлы переходят в следующую фазу без TCP-проверки.
pub fn is_udp_proxy(proxy: &Proxy) -> bool {
    matches!(proxy.proto, "hysteria" | "hysteria2")
}

/// Кэш разрешённых имён: `host -> IP-адреса` без порта, чтобы узлы с одинаковым
/// хостом и разными портами переиспользовали одно разрешение.
type AddrCache = Arc<tokio::sync::Mutex<HashMap<String, Arc<tokio::sync::OnceCell<Vec<IpAddr>>>>>>;

async fn resolve(
    host: &str,
    port: u16,
    cache: &AddrCache,
    dns_concurrency: &Arc<tokio::sync::Semaphore>,
    timeout: Duration,
) -> Option<Vec<SocketAddr>> {
    if let Ok(addr) = host.parse::<std::net::IpAddr>() {
        return Some(vec![SocketAddr::new(addr, port)]);
    }
    let cell = {
        let mut c = cache.lock().await;
        Arc::clone(c.entry(host.to_owned()).or_default())
    };
    // OnceCell даёт «single flight»: одновременные проверки одного хоста ждут
    // одно разрешение, а не запускают по потоку на каждый узел.
    let ips = cell
        .get_or_init(|| lookup_host(host, dns_concurrency, timeout))
        .await;
    if ips.is_empty() {
        None
    } else {
        Some(ips.iter().map(|ip| SocketAddr::new(*ip, port)).collect())
    }
}

/// `getaddrinfo` блокирует поток ОС, поэтому уводим его с async-рантайма.
/// Число одновременных разрешений ограничено семафором; результат (включая
/// неудачу) кэшируется вызывающим через [`OnceCell`].
async fn lookup_host(
    host: &str,
    dns_concurrency: &Arc<tokio::sync::Semaphore>,
    timeout: Duration,
) -> Vec<IpAddr> {
    let Ok(permit) = dns_concurrency.clone().acquire_owned().await else {
        return Vec::new();
    };
    let lookup_host = host.to_owned();
    let (sender, receiver) = tokio::sync::oneshot::channel();
    let spawned = std::thread::Builder::new()
        .name("proxy-dns".to_owned())
        .spawn(move || {
            let ips = (lookup_host.as_str(), 0)
                .to_socket_addrs()
                .map(|addresses| addresses.map(|addr| addr.ip()).collect::<Vec<_>>())
                .unwrap_or_default();
            let _ = sender.send(ips);
            drop(permit);
        });
    if spawned.is_err() {
        return Vec::new();
    }
    match resolve_with_timeout(receiver, timeout).await {
        Some(Ok(ips)) => ips,
        _ => Vec::new(),
    }
}

async fn resolve_with_timeout<F, T>(resolution: F, timeout: Duration) -> Option<T>
where
    F: Future<Output = T>,
{
    tokio::time::timeout(timeout, resolution).await.ok()
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
    cache: &AddrCache,
    dns_concurrency: &Arc<tokio::sync::Semaphore>,
) -> Option<u64> {
    let addrs = resolve(&proxy.host, proxy.port, cache, dns_concurrency, timeout).await?;
    let attempts = retries.max(1);
    let mut best: Option<u64> = None;
    for _ in 0..attempts {
        // Проверяем все адреса хоста параллельно: последовательный перебор
        // умножал время мёртвого узла на число A/AAAA-записей.
        let rtts = futures::future::join_all(addrs.iter().map(|addr| async move {
            let start = Instant::now();
            match tokio::time::timeout(timeout, tokio::net::TcpStream::connect(addr)).await {
                Ok(Ok(_stream)) => Some(start.elapsed().as_millis() as u64),
                _ => None,
            }
        }))
        .await;
        if let Some(rtt) = rtts.into_iter().flatten().min() {
            best = Some(match best {
                Some(b) => b.min(rtt),
                None => rtt,
            });
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
    let input_total = proxies.len();
    let udp_skipped = proxies.iter().filter(|proxy| is_udp_proxy(proxy)).count();
    let checked = proxies.len().saturating_sub(udp_skipped);
    let mut udp_proxies = Vec::with_capacity(udp_skipped);
    let tcp_proxies = proxies
        .into_iter()
        .filter_map(|proxy| {
            if is_udp_proxy(&proxy) {
                udp_proxies.push(proxy);
                None
            } else {
                Some(proxy)
            }
        })
        .collect::<Vec<_>>();
    let timeout = Duration::from_secs_f64(timeout_s.max(0.1));
    let cache: AddrCache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
    // Резолвинг — блокирующий вызов ОС, который в плохих сетях живёт секундами.
    // Раньше потолок был 64, и он становился горлышком всей фазы: каждый
    // медленный хост держал слот до конца системного таймаута.
    let dns_concurrency = Arc::new(tokio::sync::Semaphore::new(concurrency.clamp(1, 256)));

    let results: Vec<(Proxy, Option<u64>)> = {
        use std::sync::atomic::{AtomicUsize, Ordering as AOrdering};
        let pb = indicatif::ProgressBar::new(input_total as u64).with_style(
            indicatif::ProgressStyle::with_template(
                "{spinner:.green} TCP ping {pos}/{len} [{elapsed_precise}<{eta_precise}] живых: {msg}",
            )
            .expect("built-in TCP progress template is valid"),
        );
        let alive_count = Arc::new(AtomicUsize::new(0));
        let out: Vec<(Proxy, Option<u64>)> = stream::iter(tcp_proxies)
            .map(|p| {
                let cache = Arc::clone(&cache);
                let dns_concurrency = Arc::clone(&dns_concurrency);
                let pb = pb.clone();
                let alive_count = Arc::clone(&alive_count);
                async move {
                    let rtt = probe(&p, timeout, retries, max_ms, &cache, &dns_concurrency).await;
                    if rtt.is_some() {
                        // Перерисовка прогресс-бара на каждый живой узел заметно
                        // тормозит на Windows-консоли, обновляем пачками.
                        let count = alive_count.fetch_add(1, AOrdering::Relaxed) + 1;
                        if count % 16 == 0 {
                            pb.set_message(count.to_string());
                        }
                    }
                    pb.inc(1);
                    (p, rtt)
                }
            })
            .buffer_unordered(concurrency.max(1))
            .collect()
            .await;
        pb.inc(udp_skipped as u64);
        pb.finish_and_clear();
        out
    };

    let mut alive = udp_proxies;
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
        total: input_total,
        checked,
        alive: alive.len(),
        tcp_alive: alive.len().saturating_sub(udp_skipped),
        dead: dead.len(),
        udp_skipped,
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
    async fn dns_resolution_timeout_returns_dead_without_waiting_for_resolver() {
        let pending = async {
            tokio::time::sleep(Duration::from_secs(60)).await;
            Vec::<SocketAddr>::new()
        };

        assert!(resolve_with_timeout(pending, Duration::from_millis(1))
            .await
            .is_none());
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

    fn cache_with(entries: &[(&str, Vec<IpAddr>)]) -> AddrCache {
        let mut map = HashMap::new();
        for (host, ips) in entries {
            let cell = tokio::sync::OnceCell::new();
            cell.set(ips.clone()).unwrap();
            map.insert((*host).to_owned(), Arc::new(cell));
        }
        Arc::new(tokio::sync::Mutex::new(map))
    }

    #[tokio::test]
    async fn filter_handles_empty_input_with_zeroed_statistics() {
        let (alive, dead, stats) = filter(Vec::new(), 0.1, 0, 0, 0).await;
        assert!(alive.is_empty());
        assert!(dead.is_empty());
        assert_eq!(stats.total, 0);
        assert_eq!(stats.checked, 0);
        assert_eq!(stats.alive, 0);
        assert_eq!(stats.tcp_alive, 0);
        assert_eq!(stats.dead, 0);
        assert_eq!(stats.udp_skipped, 0);
        assert_eq!(stats.max_ms_seen, 0);
    }

    #[tokio::test]
    async fn filter_keeps_hysteria_udp_proxies_without_tcp_probe_or_ping_value() {
        let mut hysteria1 = proxy(9);
        hysteria1.proto = "hysteria";
        let mut hysteria2 = proxy(9);
        hysteria2.proto = "hysteria2";

        let (alive, dead, stats) = filter(vec![hysteria1, hysteria2], 0.1, 1, 1, 1).await;

        assert_eq!(alive.len(), 2);
        assert!(alive.iter().all(|proxy| proxy.ping.is_none()));
        assert!(dead.is_empty());
        // UDP-узлы входят в общий итог фазы: иначе «живых» в фазе 1 меньше,
        // чем узлов в фазе 2 и в alive.txt.
        assert_eq!(stats.total, 2);
        assert_eq!(stats.checked, 0);
        assert_eq!(stats.alive, 2);
        assert_eq!(stats.tcp_alive, 0);
        assert_eq!(stats.dead, 0);
        assert_eq!(stats.udp_skipped, 2);
    }

    #[tokio::test]
    async fn filter_reports_total_alive_count_that_matches_returned_proxies() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let live_port = listener.local_addr().unwrap().port();
        let accept = tokio::spawn(async move { let _ = listener.accept().await; });
        let mut udp = proxy(live_port);
        udp.proto = "hysteria2";
        let mut dead = proxy(live_port);
        dead.host = "127.0.0.3".into();

        let (alive, dead_nodes, stats) =
            filter(vec![proxy(live_port), udp, dead], 0.5, 4, 1, 0).await;
        accept.await.unwrap();

        assert_eq!(alive.len(), 2);
        assert_eq!(dead_nodes.len(), 1);
        assert_eq!(stats.total, 3);
        assert_eq!(stats.checked, 2);
        assert_eq!(stats.alive, alive.len());
        assert_eq!(stats.tcp_alive, 1);
        assert_eq!(stats.dead, 1);
        assert_eq!(stats.udp_skipped, 1);
    }

    #[tokio::test]
    async fn resolve_reuses_cached_ips_with_the_requested_port() {
        let cache = cache_with(&[
            ("cached.example", vec![IpAddr::from([127, 0, 0, 1])]),
            ("missing.example", Vec::new()),
        ]);
        let dns_concurrency = Arc::new(tokio::sync::Semaphore::new(4));

        assert_eq!(
            resolve(
                "cached.example",
                8443,
                &cache,
                &dns_concurrency,
                Duration::from_secs(1)
            )
            .await,
            Some(vec!["127.0.0.1:8443".parse::<SocketAddr>().unwrap()])
        );
        assert_eq!(
            resolve(
                "cached.example",
                9443,
                &cache,
                &dns_concurrency,
                Duration::from_secs(1)
            )
            .await,
            Some(vec!["127.0.0.1:9443".parse::<SocketAddr>().unwrap()])
        );
        assert_eq!(
            resolve(
                "missing.example",
                1,
                &cache,
                &dns_concurrency,
                Duration::from_secs(1)
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn resolve_looks_up_hostname_and_caches_results() {
        let cache: AddrCache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let dns_concurrency = Arc::new(tokio::sync::Semaphore::new(4));

        let first = resolve(
            "localhost",
            80,
            &cache,
            &dns_concurrency,
            Duration::from_secs(1),
        )
        .await
        .unwrap();
        assert!(!first.is_empty());
        assert_eq!(
            resolve(
                "localhost",
                80,
                &cache,
                &dns_concurrency,
                Duration::from_secs(1)
            )
            .await,
            Some(first)
        );
        assert!(cache
            .lock()
            .await
            .get("localhost")
            .is_some_and(|cell| cell.get().is_some()));
    }

    #[tokio::test]
    async fn resolve_caches_invalid_hostname_as_negative_result() {
        let cache: AddrCache = Arc::new(tokio::sync::Mutex::new(HashMap::new()));
        let dns_concurrency = Arc::new(tokio::sync::Semaphore::new(4));
        let invalid_host = "bad\0host";

        assert_eq!(
            resolve(
                invalid_host,
                80,
                &cache,
                &dns_concurrency,
                Duration::from_secs(1)
            )
            .await,
            None
        );
        assert!(cache
            .lock()
            .await
            .get(invalid_host)
            .is_some_and(|cell| cell.get().is_some_and(Vec::is_empty)));
        assert_eq!(
            resolve(
                invalid_host,
                80,
                &cache,
                &dns_concurrency,
                Duration::from_secs(1)
            )
            .await,
            None
        );
    }

    #[tokio::test]
    async fn probe_checks_each_cached_address_and_keeps_the_best_rtt() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let accepted = tokio::spawn(async move { listener.accept().await.unwrap() });
        let mut sample = proxy(port);
        sample.host = "cached-multi-address.example".into();
        let cache = cache_with(&[(
            "cached-multi-address.example",
            vec![
                IpAddr::from([127, 0, 0, 3]),
                IpAddr::from([127, 0, 0, 1]),
            ],
        )]);
        let dns_concurrency = Arc::new(tokio::sync::Semaphore::new(4));

        let rtt = probe(
            &sample,
            Duration::from_secs(1),
            1,
            0,
            &cache,
            &dns_concurrency,
        )
        .await;

        assert!(rtt.is_some());
        accepted.await.unwrap();
    }

    #[tokio::test]
    async fn probe_returns_none_for_a_cached_negative_host_lookup() {
        let mut sample = proxy(443);
        sample.host = "cached-negative.example".into();
        let cache = cache_with(&[("cached-negative.example", Vec::new())]);
        let dns_concurrency = Arc::new(tokio::sync::Semaphore::new(4));

        assert_eq!(
            probe(
                &sample,
                Duration::from_secs(1),
                1,
                0,
                &cache,
                &dns_concurrency,
            )
            .await,
            None
        );
    }
}
