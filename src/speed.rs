use crate::check::socks_client;
use rand::seq::SliceRandom;
use std::future::Future;
use std::time::Duration;
use std::time::Instant;

pub const DEFAULT_TARGETS: &[&str] = &[
    "https://speed.cloudflare.com/__down?bytes=10000000",
    "https://proof.ovh.net/files/100Mb.dat",
    "http://speedtest.tele2.net/100MB.zip",
    "https://speed.hetzner.de/100MB.bin",
    "https://mirror.leaseweb.com/speedtest/100mb.bin",
    "https://yandex.ru/internet/api/v0/measure/download?size=10000000",
];

fn remaining_time(start: Instant, timeout: Duration) -> Option<Duration> {
    let remaining = timeout.saturating_sub(start.elapsed());
    if remaining.is_zero() {
        None
    } else {
        Some(remaining)
    }
}

async fn timeout_future<F: Future>(
    future: F,
    timeout: Duration,
) -> Result<F::Output, tokio::time::error::Elapsed> {
    tokio::time::timeout(timeout, future).await
}

fn target_pool(primary: Option<&str>) -> Vec<String> {
    let mut pool = Vec::new();
    if let Some(url) = primary.filter(|url| !url.is_empty()) {
        pool.push(url.to_string());
    }
    let mut targets: Vec<String> = DEFAULT_TARGETS.iter().map(|url| url.to_string()).collect();
    if primary.is_none() {
        let mut rng = rand::thread_rng();
        targets.shuffle(&mut rng);
    }
    pool.extend(targets);
    pool
}

fn positive_speed(total: u64, secs: f64) -> Option<f64> {
    let kbs = (total as f64 / 1024.0) / secs;
    if kbs <= 0.0 {
        None
    } else {
        Some(kbs)
    }
}

async fn try_download(
    client: &reqwest::Client,
    url: &str,
    _connect_timeout: Duration,
    timeout: Duration,
    max_bytes: u64,
) -> Option<(u64, f64)> {
    try_download_at(
        client,
        url,
        timeout,
        max_bytes,
        Instant::now(),
        Some(timeout),
    )
    .await
}

async fn try_download_at(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
    max_bytes: u64,
    start: Instant,
    request_timeout: Option<Duration>,
) -> Option<(u64, f64)> {
    let mut request = client.get(url);
    if let Some(request_timeout) = request_timeout {
        request = request.timeout(request_timeout);
    }
    let resp = request.send().await.ok()?;
    if !resp.status().is_success() {
        return None;
    }
    let mut resp = resp;
    let mut total: u64 = 0;
    loop {
        if total >= max_bytes {
            break;
        }
        let Some(remaining) = remaining_time(start, timeout) else {
            break;
        };
        match timeout_future(resp.chunk(), remaining).await {
            Ok(Ok(Some(chunk))) => {
                let remaining_bytes = max_bytes.saturating_sub(total);
                total += (chunk.len() as u64).min(remaining_bytes);
            }
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
    measure_with_client(
        socks_client(port, conn_timeout),
        primary,
        conn_timeout,
        timeout,
        max_bytes,
    )
    .await
}

async fn measure_with_client(
    client: anyhow::Result<reqwest::Client>,
    primary: Option<&str>,
    conn_timeout: Duration,
    timeout: Duration,
    max_bytes: u64,
) -> f64 {
    let client = match client {
        Ok(c) => c,
        Err(_) => return 0.0,
    };
    let pool = target_pool(primary);
    measure_pool(pool, |url| {
        let client = &client;
        async move { try_download(client, &url, conn_timeout, timeout, max_bytes).await }
    })
    .await
}

async fn measure_pool<F, Fut>(pool: Vec<String>, mut download: F) -> f64
where
    F: FnMut(String) -> Fut,
    Fut: Future<Output = Option<(u64, f64)>>,
{
    for url in &pool {
        if let Some((total, secs)) = download(url.clone()).await {
            if total < 1024 {
                continue;
            }
            if let Some(kbs) = positive_speed(total, secs) {
                return kbs;
            }
        }
    }
    0.0
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    #[test]
    fn remaining_time_returns_none_once_the_deadline_has_passed() {
        let start = Instant::now() - Duration::from_secs(1);
        assert_eq!(remaining_time(start, Duration::from_secs(1)), None);
        assert!(remaining_time(Instant::now(), Duration::from_secs(1)).is_some());
    }

    #[tokio::test]
    async fn timeout_future_reports_an_expired_timeout() {
        let result = timeout_future(std::future::pending::<()>(), Duration::ZERO).await;
        assert!(result.is_err());
    }

    #[tokio::test]
    async fn measure_returns_zero_when_client_creation_fails() {
        let speed = measure_with_client(
            Err(anyhow::anyhow!("fixture error")),
            None,
            Duration::from_secs(1),
            Duration::from_secs(1),
            1024,
        )
        .await;
        assert_eq!(speed, 0.0);
    }

    #[test]
    fn target_pool_shuffles_fallback_targets_without_losing_any() {
        let mut actual = target_pool(None);
        let mut expected: Vec<String> = DEFAULT_TARGETS.iter().map(|url| url.to_string()).collect();
        actual.sort();
        expected.sort();
        assert_eq!(actual, expected);
    }

    #[test]
    fn positive_speed_rejects_a_zero_result() {
        assert_eq!(positive_speed(1024, f64::INFINITY), None);
    }

    #[tokio::test]
    async fn measure_continues_when_a_download_has_no_positive_speed() {
        let speed = measure_pool(vec!["fixture".into()], |_| async {
            Some((1024, f64::INFINITY))
        })
        .await;
        assert_eq!(speed, 0.0);
    }

    async fn serve_once(response: Vec<u8>) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            stream.write_all(&response).await.unwrap();
            stream.shutdown().await.unwrap();
        });
        (format!("http://{addr}/download"), task)
    }

    async fn serve_slow_body(
        initial_body: &'static [u8],
        remaining_body: &'static [u8],
        delay: Duration,
    ) -> (String, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = Vec::new();
            let mut buffer = [0; 1024];
            loop {
                let read = stream.read(&mut buffer).await.unwrap();
                if read == 0 {
                    break;
                }
                request.extend_from_slice(&buffer[..read]);
                if request.windows(4).any(|window| window == b"\r\n\r\n") {
                    break;
                }
            }
            let content_length = initial_body.len() + remaining_body.len();
            let header = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {content_length}\r\nConnection: close\r\n\r\n"
            );
            stream.write_all(header.as_bytes()).await.unwrap();
            stream.write_all(initial_body).await.unwrap();
            tokio::time::sleep(delay).await;
            let _ = stream.write_all(remaining_body).await;
            let _ = stream.shutdown().await;
        });
        (format!("http://{addr}/slow"), task)
    }

    #[tokio::test]
    async fn local_http_fixtures_handle_eof_before_request_headers_finish() {
        let (url, server) = serve_once(Vec::new()).await;
        let address = url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET /download HTTP/1.1\r\nHost:")
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        server.await.unwrap();

        let (url, server) = serve_slow_body(b"a", b"b", Duration::ZERO).await;
        let address = url
            .strip_prefix("http://")
            .unwrap()
            .split('/')
            .next()
            .unwrap();
        let mut client = tokio::net::TcpStream::connect(address).await.unwrap();
        client
            .write_all(b"GET /slow HTTP/1.1\r\nHost:")
            .await
            .unwrap();
        client.shutdown().await.unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn try_download_reads_from_a_local_http_server() {
        let body = vec![b'x'; 2048];
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        let (url, server) = serve_once(response).await;
        let client = reqwest::Client::builder().http1_only().build().unwrap();

        let result = try_download(
            &client,
            &url,
            Duration::from_secs(1),
            Duration::from_secs(2),
            4096,
        )
        .await;
        server.await.unwrap();

        let (bytes, seconds) = result.unwrap();
        assert_eq!(bytes, 2048);
        assert!(seconds >= 0.1);
    }

    #[tokio::test]
    async fn try_download_rejects_non_success_status_and_empty_body() {
        let (url, server) = serve_once(
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 4\r\nConnection: close\r\n\r\nnope".to_vec(),
        )
        .await;
        let client = reqwest::Client::builder().http1_only().build().unwrap();
        let result = try_download(
            &client,
            &url,
            Duration::from_secs(1),
            Duration::from_secs(2),
            4096,
        )
        .await;
        server.await.unwrap();
        assert!(result.is_none());

        let (url, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
        )
        .await;
        let result = try_download(
            &client,
            &url,
            Duration::from_secs(1),
            Duration::from_secs(2),
            4096,
        )
        .await;
        server.await.unwrap();
        assert!(result.is_none());
    }

    #[tokio::test]
    async fn try_download_caps_counted_bytes_at_the_requested_limit() {
        let body = vec![b'c'; 2048];
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        let (url, server) = serve_once(response).await;
        let client = reqwest::Client::builder().http1_only().build().unwrap();

        let result = try_download(
            &client,
            &url,
            Duration::from_secs(1),
            Duration::from_secs(2),
            1024,
        )
        .await;
        server.await.unwrap();

        assert_eq!(result.unwrap().0, 1024);
    }

    #[tokio::test]
    async fn try_download_returns_none_for_an_invalid_url() {
        let client = reqwest::Client::new();

        let result = try_download(
            &client,
            "://invalid-url",
            Duration::from_secs(1),
            Duration::from_secs(1),
            1024,
        )
        .await;

        assert!(result.is_none());
    }

    #[tokio::test]
    async fn try_download_keeps_bytes_received_before_a_truncated_body() {
        let (url, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 20\r\nConnection: close\r\n\r\nshort".to_vec(),
        )
        .await;
        let client = reqwest::Client::builder().http1_only().build().unwrap();

        let result = try_download(
            &client,
            &url,
            Duration::from_secs(1),
            Duration::from_secs(2),
            4096,
        )
        .await;
        server.await.unwrap();

        assert_eq!(result.unwrap().0, 5);
    }

    #[tokio::test]
    async fn try_download_returns_bytes_received_before_a_slow_body_times_out() {
        let (url, server) =
            serve_slow_body(b"first chunk", b"second chunk", Duration::from_millis(150)).await;
        let client = reqwest::Client::builder().http1_only().build().unwrap();

        let result = try_download(
            &client,
            &url,
            Duration::from_secs(1),
            Duration::from_millis(50),
            4096,
        )
        .await;
        server.await.unwrap();

        assert_eq!(result.unwrap().0, b"first chunk".len() as u64);
    }

    #[tokio::test]
    async fn try_download_applies_its_chunk_timeout_without_a_request_timeout() {
        let (url, server) =
            serve_slow_body(b"first chunk", b"second chunk", Duration::from_millis(150)).await;
        let client = reqwest::Client::builder().http1_only().build().unwrap();

        let result = try_download_at(
            &client,
            &url,
            Duration::from_millis(50),
            4096,
            Instant::now(),
            None,
        )
        .await;
        server.await.unwrap();

        assert_eq!(result.unwrap().0, b"first chunk".len() as u64);
    }

    #[tokio::test]
    async fn try_download_stops_when_started_after_its_deadline() {
        let (url, server) = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\ndata".to_vec(),
        )
        .await;
        let client = reqwest::Client::builder().http1_only().build().unwrap();

        let result = try_download_at(
            &client,
            &url,
            Duration::from_secs(1),
            4096,
            Instant::now() - Duration::from_secs(2),
            Some(Duration::from_secs(1)),
        )
        .await;
        server.await.unwrap();

        assert!(result.is_none());
    }

    async fn socks5_once() -> (u16, tokio::task::JoinHandle<()>) {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        let task = tokio::spawn(async move {
            let (mut client, _) = listener.accept().await.unwrap();
            let mut greeting = [0; 2];
            client.read_exact(&mut greeting).await.unwrap();
            let mut methods = vec![0; greeting[1] as usize];
            client.read_exact(&mut methods).await.unwrap();
            client.write_all(&[5, 0]).await.unwrap();

            let mut request = [0; 4];
            client.read_exact(&mut request).await.unwrap();
            assert_eq!(request[0], 5);
            assert_eq!(request[1], 1);
            let host = match request[3] {
                1 => {
                    let mut octets = [0; 4];
                    client.read_exact(&mut octets).await.unwrap();
                    std::net::Ipv4Addr::from(octets).to_string()
                }
                3 => {
                    let mut length = [0; 1];
                    client.read_exact(&mut length).await.unwrap();
                    let mut name = vec![0; length[0] as usize];
                    client.read_exact(&mut name).await.unwrap();
                    let name = String::from_utf8(name).unwrap();
                    if name == "localhost" {
                        "127.0.0.1".to_string()
                    } else {
                        name
                    }
                }
                atyp => panic!("unsupported SOCKS address type {atyp}"),
            };
            let mut remote_port = [0; 2];
            client.read_exact(&mut remote_port).await.unwrap();
            let remote_port = u16::from_be_bytes(remote_port);
            let mut remote = tokio::net::TcpStream::connect((host.as_str(), remote_port))
                .await
                .unwrap();
            client
                .write_all(&[5, 0, 0, 1, 127, 0, 0, 1, 0, 0])
                .await
                .unwrap();
            tokio::io::copy_bidirectional(&mut client, &mut remote)
                .await
                .unwrap();
        });
        (port, task)
    }

    async fn socks5_client(port: u16) -> tokio::net::TcpStream {
        let mut client = tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .unwrap();
        client.write_all(&[5, 1, 0]).await.unwrap();
        let mut greeting = [0; 2];
        client.read_exact(&mut greeting).await.unwrap();
        assert_eq!(greeting, [5, 0]);
        client
    }

    #[tokio::test]
    async fn socks_fixture_accepts_a_non_localhost_domain_address() {
        let remote_listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let remote_port = remote_listener.local_addr().unwrap().port();
        let remote_task = tokio::spawn(async move {
            let (stream, _) = remote_listener.accept().await.unwrap();
            drop(stream);
        });
        let (proxy_port, proxy_task) = socks5_once().await;
        let mut client = socks5_client(proxy_port).await;
        let domain = b"127.0.0.1";
        let mut request = vec![5, 1, 0, 3, domain.len() as u8];
        request.extend_from_slice(domain);
        request.extend_from_slice(&remote_port.to_be_bytes());
        client.write_all(&request).await.unwrap();
        let mut response = [0; 10];
        client.read_exact(&mut response).await.unwrap();
        assert_eq!(response[1], 0);
        drop(client);
        proxy_task.await.unwrap();
        remote_task.await.unwrap();
    }

    #[tokio::test]
    async fn socks_fixture_rejects_an_unsupported_address_type() {
        let (port, server) = socks5_once().await;
        let mut client = socks5_client(port).await;
        client.write_all(&[5, 1, 0, 4]).await.unwrap();
        let error = server.await.unwrap_err();
        assert!(error.is_panic());
    }

    #[tokio::test]
    async fn measure_downloads_through_a_local_socks5_proxy() {
        let body = vec![b'z'; 2048];
        let mut response = format!(
            "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        )
        .into_bytes();
        response.extend_from_slice(&body);
        let (url, http_server) = serve_once(response).await;
        let url = url.replace("127.0.0.1", "localhost");
        let (socks_port, socks_server) = socks5_once().await;

        let speed = measure(socks_port, Some(&url), 1.0, 2.0, 0.5).await;

        http_server.await.unwrap();
        socks_server.await.unwrap();
        assert!(speed > 0.0);
    }

    #[tokio::test]
    async fn measure_returns_zero_for_an_unavailable_local_socks_proxy() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let speed = measure(port, None, 1.0, 1.0, 0.5).await;

        assert_eq!(speed, 0.0);
    }

    #[tokio::test]
    async fn measure_uses_fallback_targets_when_primary_url_is_empty() {
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);

        let speed = measure(port, Some(""), 1.0, 1.0, 0.5).await;

        assert_eq!(speed, 0.0);
    }

    #[tokio::test]
    async fn measure_skips_a_download_smaller_than_one_kilobyte() {
        let response =
            b"HTTP/1.1 200 OK\r\nContent-Length: 10\r\nConnection: close\r\n\r\nsmall body";
        let (url, http_server) = serve_once(response.to_vec()).await;
        let (socks_port, socks_server) = socks5_once().await;

        let speed = measure(socks_port, Some(&url), 1.0, 2.0, 0.5).await;

        http_server.await.unwrap();
        socks_server.await.unwrap();
        assert_eq!(speed, 0.0);
    }
}
