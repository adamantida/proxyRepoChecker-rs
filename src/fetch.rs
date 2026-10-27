use crate::outbound::b64_any;
use crate::output;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use reqwest::header::{HeaderValue, ETAG, IF_MODIFIED_SINCE, IF_NONE_MATCH, LAST_MODIFIED};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::path::{Path, PathBuf};
use std::time::Duration;

pub const DEFAULT_MAX_SOURCE_BYTES: usize = 16 * 1024 * 1024;
pub const DEFAULT_FETCH_RETRIES: usize = 2;
const MAX_FETCH_RETRIES: usize = 8;
const RETRY_BASE_DELAY: Duration = Duration::from_millis(200);
const CACHE_FORMAT_VERSION: u8 = 1;
const MAX_REDIRECT_HOPS: usize = 10;
const MAX_CACHE_FILE_BYTES: usize = 64 * 1024 * 1024;
const MAX_BUFFERED_RESPONSE_BYTES: usize = 256 * 1024 * 1024;

#[derive(Clone, Debug)]
pub struct FetchOptions {
    pub concurrency: usize,
    pub timeout: Duration,
    pub max_response_bytes: usize,
    /// Number of retries after the initial attempt, capped at [`MAX_FETCH_RETRIES`].
    pub retries: usize,
    /// This deliberately weakens HTTPS verification and must be explicitly opted into.
    pub insecure_tls: bool,
    /// `None` disables validators and stale-on-error cache fallback.
    pub cache_dir: Option<PathBuf>,
}

impl Default for FetchOptions {
    fn default() -> Self {
        Self {
            concurrency: 32,
            timeout: Duration::from_secs(15),
            max_response_bytes: DEFAULT_MAX_SOURCE_BYTES,
            retries: DEFAULT_FETCH_RETRIES,
            insecure_tls: false,
            cache_dir: None,
        }
    }
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum CacheDisposition {
    Disabled,
    Downloaded,
    NotModified,
    StaleFallback,
}

#[derive(Clone, Debug)]
pub struct FetchSuccess {
    pub url: String,
    pub text: String,
    pub cache: CacheDisposition,
}

pub struct FetchFailure {
    pub url: String,
    pub reason: String,
    pub attempts: usize,
}

pub struct FetchReport {
    pub texts: Vec<String>,
    pub successes: Vec<FetchSuccess>,
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

#[cfg(test)]
async fn fetch_one(
    client: &reqwest::Client,
    url: &str,
    timeout: Duration,
) -> Result<String, String> {
    let result = fetch_one_with_options(
        client,
        url,
        &FetchOptions {
            timeout,
            retries: 0,
            ..FetchOptions::default()
        },
        None,
    )
    .await;
    result
        .map(|success| success.text)
        .map_err(|error| error.message)
}

#[cfg(test)]
async fn fetch_all(urls: &[String], concurrency: usize, timeout_s: f64) -> FetchReport {
    fetch_all_with_options(
        urls,
        &FetchOptions {
            concurrency,
            timeout: Duration::from_secs_f64(timeout_s.max(1.0)),
            ..FetchOptions::default()
        },
    )
    .await
}

pub async fn fetch_all_with_options(urls: &[String], options: &FetchOptions) -> FetchReport {
    if urls.is_empty() {
        return FetchReport {
            texts: Vec::new(),
            successes: Vec::new(),
            failures: Vec::new(),
        };
    }
    let client = match build_client(options.insecure_tls) {
        Ok(client) => client,
        Err(error) => {
            return FetchReport {
                texts: Vec::new(),
                successes: Vec::new(),
                failures: urls
                    .iter()
                    .map(|url| FetchFailure {
                        url: url.clone(),
                        reason: format!("не удалось создать HTTP-клиент: {error}"),
                        attempts: 0,
                    })
                    .collect(),
            };
        }
    };

    let pb = ProgressBar::new(urls.len() as u64).with_style(
        ProgressStyle::with_template(
            "{spinner:.green} загрузка источников {pos}/{len} [{elapsed_precise}<{eta_precise}]",
        )
        .unwrap_or_else(|_| ProgressStyle::default_bar()),
    );

    let mut effective_options = options.clone();
    effective_options.retries = effective_options.retries.min(MAX_FETCH_RETRIES);
    let memory_limited_concurrency =
        (MAX_BUFFERED_RESPONSE_BYTES / effective_options.max_response_bytes.max(1)).max(1);
    let concurrency = effective_options
        .concurrency
        .max(1)
        .min(memory_limited_concurrency);
    let results: Vec<(String, Result<FetchSuccess, AttemptFailure>)> =
        stream::iter(urls.iter().cloned())
            .map(|url| {
                let client = client.clone();
                let pb = pb.clone();
                let options = effective_options.clone();
                async move {
                    let out = fetch_one_with_options(&client, &url, &options, None).await;
                    pb.inc(1);
                    (url, out)
                }
            })
            .buffer_unordered(concurrency)
            .collect()
            .await;

    pb.finish_and_clear();
    let mut report = FetchReport {
        texts: Vec::new(),
        successes: Vec::new(),
        failures: Vec::new(),
    };
    for (url, result) in results {
        match result {
            Ok(success) => {
                report.texts.push(success.text.clone());
                report.successes.push(success);
            }
            Err(error) => report.failures.push(FetchFailure {
                url,
                reason: error.message,
                attempts: error.attempts,
            }),
        }
    }
    report
}

fn build_client(insecure_tls: bool) -> Result<reqwest::Client, reqwest::Error> {
    build_client_with_roots(insecure_tls, &[])
}

fn build_client_with_roots(
    insecure_tls: bool,
    roots: &[reqwest::Certificate],
) -> Result<reqwest::Client, reqwest::Error> {
    let mut builder =
        reqwest::Client::builder()
            .user_agent(UA)
            .redirect(reqwest::redirect::Policy::custom(|attempt| {
                if is_https_downgrade(attempt.previous(), attempt.url()) {
                    attempt.error("refusing HTTPS-to-HTTP redirect")
                } else if attempt.previous().len() >= MAX_REDIRECT_HOPS {
                    attempt.error("too many redirects")
                } else {
                    attempt.follow()
                }
            }));
    for root in roots {
        builder = builder.add_root_certificate(root.clone());
    }
    if insecure_tls {
        builder.danger_accept_invalid_certs(true).build()
    } else {
        builder.build()
    }
}

fn is_https_downgrade(previous: &[reqwest::Url], next: &reqwest::Url) -> bool {
    next.scheme() == "http" && previous.iter().any(|url| url.scheme() == "https")
}

#[derive(Clone, Debug, Deserialize, Serialize)]
struct CachedSubscription {
    format_version: u8,
    body: String,
    etag: Option<String>,
    last_modified: Option<String>,
}

#[derive(Debug)]
struct AttemptFailure {
    message: String,
    retryable: bool,
    attempts: usize,
}

async fn fetch_one_with_options(
    client: &reqwest::Client,
    url: &str,
    options: &FetchOptions,
    cache_override: Option<CachedSubscription>,
) -> Result<FetchSuccess, AttemptFailure> {
    if options.max_response_bytes == 0 {
        return Err(AttemptFailure {
            message: "лимит размера ответа должен быть больше нуля".into(),
            retryable: false,
            attempts: 0,
        });
    }

    let cache_path = options.cache_dir.as_ref().map(|dir| cache_path(dir, url));
    let cached = cache_override
        .or_else(|| {
            cache_path
                .as_deref()
                .and_then(|path| read_cache(path, options.max_response_bytes))
        })
        .filter(|entry| entry.body.len() <= options.max_response_bytes);
    let retries = options.retries.min(MAX_FETCH_RETRIES);
    let mut last_error = None;

    for attempt in 0..=retries {
        match fetch_attempt(client, url, options, cached.as_ref()).await {
            Ok(AttemptResult::Body {
                body,
                etag,
                last_modified,
            }) => {
                let decoded = maybe_decode(&body);
                if let Some(path) = cache_path.as_deref() {
                    let record = CachedSubscription {
                        format_version: CACHE_FORMAT_VERSION,
                        body,
                        etag,
                        last_modified,
                    };
                    if let Ok(bytes) = serde_json::to_vec(&record) {
                        if let Some(parent) = path.parent() {
                            let _ = std::fs::create_dir_all(parent);
                        }
                        let _ = output::write_bytes_atomic(path, &bytes, Some(0o600));
                    }
                }
                return Ok(FetchSuccess {
                    url: url.to_owned(),
                    text: decoded,
                    cache: if options.cache_dir.is_some() {
                        CacheDisposition::Downloaded
                    } else {
                        CacheDisposition::Disabled
                    },
                });
            }
            Ok(AttemptResult::NotModified) => {
                if let Some(cached) = cached.as_ref() {
                    return Ok(FetchSuccess {
                        url: url.to_owned(),
                        text: maybe_decode(&cached.body),
                        cache: CacheDisposition::NotModified,
                    });
                }
                return Err(AttemptFailure {
                    message: "HTTP 304 без сохранённого ответа".into(),
                    retryable: false,
                    attempts: attempt + 1,
                });
            }
            Err(error) => {
                let may_retry = error.retryable && attempt < retries;
                let mut error = error;
                error.attempts = attempt + 1;
                last_error = Some(error);
                if may_retry {
                    let multiplier = 1_u32 << attempt.min(5);
                    tokio::time::sleep(RETRY_BASE_DELAY * multiplier).await;
                    continue;
                }
                break;
            }
        }
    }

    let error = last_error.unwrap_or_else(|| AttemptFailure {
        message: "загрузка не выполнена".into(),
        retryable: false,
        attempts: 0,
    });
    if let Some(cached) = cached {
        return Ok(FetchSuccess {
            url: url.to_owned(),
            text: maybe_decode(&cached.body),
            cache: CacheDisposition::StaleFallback,
        });
    }
    Err(AttemptFailure {
        message: error.message,
        retryable: false,
        attempts: error.attempts,
    })
}

enum AttemptResult {
    Body {
        body: String,
        etag: Option<String>,
        last_modified: Option<String>,
    },
    NotModified,
}

async fn fetch_attempt(
    client: &reqwest::Client,
    url: &str,
    options: &FetchOptions,
    cached: Option<&CachedSubscription>,
) -> Result<AttemptResult, AttemptFailure> {
    let mut request = client.get(url).timeout(options.timeout);
    if let Some(cached) = cached {
        if let Some(etag) = cached.etag.as_deref().and_then(valid_header_value) {
            request = request.header(IF_NONE_MATCH, etag);
        }
        if let Some(last_modified) = cached.last_modified.as_deref().and_then(valid_header_value) {
            request = request.header(IF_MODIFIED_SINCE, last_modified);
        }
    }

    let mut response = request.send().await.map_err(|error| {
        let message = if error.is_timeout() {
            "истекло время ожидания источника"
        } else if error.is_connect() {
            "не удалось подключиться к источнику"
        } else {
            "ошибка HTTP-запроса к источнику"
        };
        AttemptFailure {
            message: message.into(),
            retryable: error.is_timeout() || error.is_connect(),
            attempts: 0,
        }
    })?;
    let status = response.status();
    if status == reqwest::StatusCode::NOT_MODIFIED {
        return Ok(AttemptResult::NotModified);
    }
    if !status.is_success() {
        return Err(AttemptFailure {
            message: format!("HTTP {status}"),
            retryable: status == reqwest::StatusCode::TOO_MANY_REQUESTS || status.is_server_error(),
            attempts: 0,
        });
    }
    if response
        .content_length()
        .is_some_and(|length| length > options.max_response_bytes as u64)
    {
        return Err(AttemptFailure {
            message: format!("ответ превышает лимит {} байт", options.max_response_bytes),
            retryable: false,
            attempts: 0,
        });
    }

    let etag = response
        .headers()
        .get(ETAG)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let last_modified = response
        .headers()
        .get(LAST_MODIFIED)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    let mut body = Vec::new();
    while let Some(chunk) = response.chunk().await.map_err(|_| AttemptFailure {
        message: "ошибка чтения ответа источника".into(),
        retryable: true,
        attempts: 0,
    })? {
        if body.len().saturating_add(chunk.len()) > options.max_response_bytes {
            return Err(AttemptFailure {
                message: format!("ответ превышает лимит {} байт", options.max_response_bytes),
                retryable: false,
                attempts: 0,
            });
        }
        body.extend_from_slice(&chunk);
    }
    Ok(AttemptResult::Body {
        body: String::from_utf8_lossy(&body).into_owned(),
        etag,
        last_modified,
    })
}

fn valid_header_value(value: &str) -> Option<HeaderValue> {
    HeaderValue::from_str(value).ok()
}

fn cache_path(directory: &Path, url: &str) -> PathBuf {
    let digest = Sha256::digest(url.as_bytes());
    let filename = digest
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    directory.join(format!("{filename}.json"))
}

fn read_cache(path: &Path, max_body_bytes: usize) -> Option<CachedSubscription> {
    use std::io::Read;

    let max_file_bytes = max_body_bytes
        .saturating_mul(6)
        .saturating_add(4096)
        .min(MAX_CACHE_FILE_BYTES);
    let metadata_len = std::fs::metadata(path).ok()?.len();
    if metadata_len > max_file_bytes as u64 {
        return None;
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)
        .ok()?
        .take(max_file_bytes as u64 + 1)
        .read_to_end(&mut bytes)
        .ok()?;
    if bytes.len() > max_file_bytes {
        return None;
    }
    let cached: CachedSubscription = serde_json::from_slice(&bytes).ok()?;
    (cached.format_version == CACHE_FORMAT_VERSION && cached.body.len() <= max_body_bytes)
        .then_some(cached)
}

#[cfg(test)]
mod tests {
    use super::{
        build_client, build_client_with_roots, cache_path, fetch_all, fetch_all_with_options,
        fetch_one, fetch_one_with_options, is_https_downgrade, maybe_decode, read_cache,
        valid_header_value, CachedSubscription, FetchOptions, CACHE_FORMAT_VERSION,
    };
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

    fn serve_sequence(responses: Vec<&'static [u8]>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        thread::spawn(move || {
            for response in responses {
                let Ok((mut stream, _)) = listener.accept() else {
                    return;
                };
                let mut request = [0_u8; 4096];
                let _ = stream.read(&mut request);
                let _ = stream.write_all(response);
                let _ = stream.shutdown(Shutdown::Both);
            }
        });
        format!("http://{address}/test")
    }

    fn serve_capture(response: &'static [u8]) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let address = listener.local_addr().unwrap();
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let mut request = [0_u8; 4096];
            let count = stream.read(&mut request).unwrap_or(0);
            let request = String::from_utf8_lossy(&request[..count]).into_owned();
            let _ = stream.write_all(response);
            let _ = stream.shutdown(Shutdown::Both);
            request
        });
        (format!("http://{address}/test"), handle)
    }

    async fn serve_tls(
        cert: rustls::pki_types::CertificateDer<'static>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
    ) -> String {
        serve_tls_response(
            cert,
            key,
            b"HTTP/1.1 200 OK\r\nContent-Length: 11\r\nConnection: close\r\n\r\nhello world"
                .to_vec(),
        )
        .await
    }

    async fn serve_tls_response(
        cert: rustls::pki_types::CertificateDer<'static>,
        key: rustls::pki_types::PrivateKeyDer<'static>,
        response: Vec<u8>,
    ) -> String {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};
        use tokio_rustls::TlsAcceptor;

        let config = rustls::ServerConfig::builder()
            .with_no_client_auth()
            .with_single_cert(vec![cert], key)
            .unwrap();
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let acceptor = TlsAcceptor::from(std::sync::Arc::new(config));
        tokio::spawn(async move {
            // A failed certificate validation may still complete the server-side
            // handshake first, so tolerate an alert and accept the next attempt.
            for _ in 0..3 {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    continue;
                };
                let mut request = [0_u8; 2048];
                let _ = stream.read(&mut request).await;
                let _ = stream.write_all(&response).await;
                return;
            }
        });
        format!("https://localhost:{}/subscription", address.port())
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

    #[test]
    fn redirect_policy_rejects_https_downgrades_but_allows_http_redirects() {
        let https = reqwest::Url::parse("https://example.com/old").unwrap();
        let http = reqwest::Url::parse("http://example.com/new").unwrap();
        let https_next = reqwest::Url::parse("https://example.com/new").unwrap();
        assert!(is_https_downgrade(std::slice::from_ref(&https), &http));
        assert!(!is_https_downgrade(
            std::slice::from_ref(&https),
            &https_next
        ));
        assert!(!is_https_downgrade(&[], &http));
    }

    #[test]
    fn cache_reader_enforces_current_body_and_file_size_limits() {
        let directory = std::env::temp_dir().join(format!(
            "proxy-rs-fetch-cache-bound-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("cache.json");
        let cached = CachedSubscription {
            format_version: CACHE_FORMAT_VERSION,
            body: "x".repeat(11),
            etag: None,
            last_modified: None,
        };
        std::fs::write(&path, serde_json::to_vec(&cached).unwrap()).unwrap();
        assert!(read_cache(&path, 10).is_none());

        std::fs::write(&path, vec![b'x'; 20_000]).unwrap();
        assert!(read_cache(&path, 10).is_none());
        std::fs::File::create(&path)
            .unwrap()
            .set_len((super::MAX_CACHE_FILE_BYTES + 1) as u64)
            .unwrap();
        assert!(read_cache(&path, usize::MAX).is_none());
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn cache_reader_ignores_malformed_and_unknown_format_records() {
        let directory = std::env::temp_dir().join(format!(
            "proxy-rs-fetch-cache-format-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let path = directory.join("cache.json");

        std::fs::write(&path, b"not json").unwrap();
        assert!(read_cache(&path, 1024).is_none());

        let unsupported = CachedSubscription {
            format_version: CACHE_FORMAT_VERSION + 1,
            body: "vless://cached".into(),
            etag: None,
            last_modified: None,
        };
        std::fs::write(&path, serde_json::to_vec(&unsupported).unwrap()).unwrap();
        assert!(read_cache(&path, 1024).is_none());
        assert!(read_cache(&directory.join("missing.json"), 1024).is_none());
        assert!(read_cache(&directory, 1024).is_none());

        std::fs::remove_dir_all(directory).unwrap();
    }

    #[test]
    fn invalid_cache_validator_headers_are_ignored() {
        assert!(valid_header_value("\"safe-etag\"").is_some());
        assert!(valid_header_value("bad\r\nX-Injected: yes").is_none());
    }

    #[tokio::test]
    async fn configured_client_never_follows_https_redirects_to_http() {
        use std::io::ErrorKind;

        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let cert = generated.cert.der().clone();
        let key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(generated.key_pair.serialize_der().into());
        let destination = TcpListener::bind("127.0.0.1:0").unwrap();
        destination.set_nonblocking(true).unwrap();
        let destination_address = destination.local_addr().unwrap();
        let reached = thread::spawn(move || {
            let deadline = std::time::Instant::now() + Duration::from_millis(500);
            loop {
                match destination.accept() {
                    Ok(_) => return true,
                    Err(error) if error.kind() == ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return false;
                        }
                        thread::sleep(Duration::from_millis(10));
                    }
                    Err(_) => return true,
                }
            }
        });
        let redirect = format!(
            "HTTP/1.1 302 Found\r\nLocation: http://{destination_address}/target\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        let redirect_url = serve_tls_response(cert.clone(), key, redirect.into_bytes()).await;
        let trusted = reqwest::Certificate::from_der(cert.as_ref()).unwrap();
        let client = build_client_with_roots(false, &[trusted]).unwrap();
        let result = fetch_one_with_options(
            &client,
            &redirect_url,
            &FetchOptions {
                retries: 0,
                timeout: Duration::from_secs(3),
                ..FetchOptions::default()
            },
            None,
        )
        .await;

        assert!(result.is_err());
        assert!(
            !reached.join().unwrap(),
            "HTTP downgrade destination was reached"
        );
    }

    #[tokio::test]
    async fn configured_client_stops_after_the_redirect_hop_limit() {
        const REDIRECT: &[u8] = b"HTTP/1.1 302 Found\r\nLocation: /test\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let url = serve_sequence(vec![REDIRECT; super::MAX_REDIRECT_HOPS + 1]);

        let error = build_client(false)
            .unwrap()
            .get(url)
            .send()
            .await
            .unwrap_err();

        assert!(error.is_redirect(), "{error}");
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
        let report = fetch_all_with_options(
            std::slice::from_ref(&url),
            &FetchOptions {
                concurrency: 1,
                timeout: Duration::from_secs(2),
                retries: 0,
                ..FetchOptions::default()
            },
        )
        .await;

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
        assert!(error.starts_with("ошибка чтения ответа"), "{error}");
    }

    #[tokio::test]
    async fn fetch_all_handles_an_empty_url_list_without_network_access() {
        let report = fetch_all(&[], 0, 0.0).await;
        assert!(report.texts.is_empty());
        assert!(report.failures.is_empty());
    }

    #[tokio::test]
    async fn zero_response_limit_fails_without_a_network_attempt() {
        let error = fetch_one_with_options(
            &reqwest::Client::new(),
            "not a URL",
            &FetchOptions {
                max_response_bytes: 0,
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(error.attempts, 0);
        assert!(error.message.contains("больше нуля"));
    }

    #[tokio::test]
    async fn not_modified_without_a_valid_cache_is_a_non_retryable_error() {
        let url = serve_once(b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n");
        let error = fetch_one_with_options(
            &reqwest::Client::new(),
            &url,
            &FetchOptions {
                retries: 3,
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(error.attempts, 1);
        assert_eq!(error.message, "HTTP 304 без сохранённого ответа");
    }

    #[tokio::test]
    async fn invalid_cached_validators_are_not_sent_to_the_source() {
        let (url, server) =
            serve_capture(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok");
        let cache = CachedSubscription {
            format_version: CACHE_FORMAT_VERSION,
            body: "stale body".into(),
            etag: Some("bad\r\nIf-None-Match: injected".into()),
            last_modified: Some("bad\r\nIf-Modified-Since: injected".into()),
        };

        let result = fetch_one_with_options(
            &reqwest::Client::new(),
            &url,
            &FetchOptions {
                retries: 0,
                ..FetchOptions::default()
            },
            Some(cache),
        )
        .await
        .unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();

        assert_eq!(result.text, "ok");
        assert!(!request.contains("if-none-match:"));
        assert!(!request.contains("if-modified-since:"));
    }

    #[tokio::test]
    async fn client_error_status_is_not_retried() {
        let url =
            serve_once(b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n");

        let error = fetch_one_with_options(
            &reqwest::Client::new(),
            &url,
            &FetchOptions {
                retries: 3,
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(error.attempts, 1);
        assert_eq!(error.message, "HTTP 404 Not Found");
    }

    #[tokio::test]
    async fn rate_limited_source_is_retried_once_and_then_accepted() {
        let url = serve_sequence(vec![
            b"HTTP/1.1 429 Too Many Requests\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        ]);

        let result = fetch_one_with_options(
            &reqwest::Client::new(),
            &url,
            &FetchOptions {
                retries: 1,
                timeout: Duration::from_secs(2),
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap();

        assert_eq!(result.text, "ok");
    }

    #[tokio::test]
    async fn exhausted_retryable_server_errors_report_the_final_attempt_count() {
        let error_response =
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n";
        let url = serve_sequence(vec![error_response, error_response]);

        let error = fetch_one_with_options(
            &reqwest::Client::new(),
            &url,
            &FetchOptions {
                retries: 1,
                timeout: Duration::from_secs(2),
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap_err();

        assert_eq!(error.attempts, 2);
        assert_eq!(error.message, "HTTP 503 Service Unavailable");
    }

    #[tokio::test]
    async fn retries_temporary_server_errors_and_stops_after_success() {
        let url = serve_sequence(vec![
            b"HTTP/1.1 503 Service Unavailable\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
        ]);
        let report = fetch_all_with_options(
            &[url],
            &FetchOptions {
                concurrency: 1,
                retries: 1,
                timeout: Duration::from_secs(2),
                ..FetchOptions::default()
            },
        )
        .await;
        assert_eq!(report.texts, ["ok"]);
        assert!(report.failures.is_empty());
    }

    #[tokio::test]
    async fn rejects_responses_over_the_configured_limit() {
        let url =
            serve_once(b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\n123456");
        let client = reqwest::Client::new();
        let error = fetch_one_with_options(
            &client,
            &url,
            &FetchOptions {
                max_response_bytes: 5,
                retries: 0,
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("превышает лимит"));
        assert_eq!(error.attempts, 1);
    }

    #[tokio::test]
    async fn rejects_chunked_responses_over_the_configured_limit() {
        let url = serve_once(
            b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n6\r\n123456\r\n0\r\n\r\n",
        );
        let error = fetch_one_with_options(
            &reqwest::Client::new(),
            &url,
            &FetchOptions {
                max_response_bytes: 5,
                retries: 0,
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap_err();
        assert!(error.message.contains("превышает лимит"));
    }

    #[tokio::test]
    async fn uses_conditional_cache_and_falls_back_to_stale_data() {
        let directory = std::env::temp_dir().join(format!(
            "proxy-rs-fetch-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let url = "http://127.0.0.1:9/private-token?key=secret";
        let path = cache_path(&directory, url);
        let cached = CachedSubscription {
            format_version: CACHE_FORMAT_VERSION,
            body: "vless://cached".into(),
            etag: Some("\"v1\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
        };
        std::fs::write(&path, serde_json::to_vec(&cached).unwrap()).unwrap();

        let failing_client = reqwest::Client::new();
        let stale = fetch_one_with_options(
            &failing_client,
            url,
            &FetchOptions {
                timeout: Duration::from_millis(150),
                retries: 0,
                cache_dir: Some(directory.clone()),
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap();
        assert_eq!(stale.text, "vless://cached");
        assert_eq!(stale.cache, super::CacheDisposition::StaleFallback);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn sends_cache_validators_and_reuses_body_on_not_modified() {
        let directory = std::env::temp_dir().join(format!(
            "proxy-rs-fetch-validator-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir_all(&directory).unwrap();
        let (url, server) =
            serve_capture(b"HTTP/1.1 304 Not Modified\r\nConnection: close\r\n\r\n");
        let cached = CachedSubscription {
            format_version: CACHE_FORMAT_VERSION,
            body: "vless://cached".into(),
            etag: Some("\"test-etag\"".into()),
            last_modified: Some("Wed, 21 Oct 2015 07:28:00 GMT".into()),
        };
        let cache_file = cache_path(&directory, &url);
        std::fs::write(&cache_file, serde_json::to_vec(&cached).unwrap()).unwrap();

        let result = fetch_one_with_options(
            &reqwest::Client::new(),
            &url,
            &FetchOptions {
                retries: 0,
                cache_dir: Some(directory.clone()),
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap();
        let request = server.join().unwrap().to_ascii_lowercase();
        assert!(request.contains("if-none-match: \"test-etag\""));
        assert!(request.contains("if-modified-since: wed, 21 oct 2015 07:28:00 gmt"));
        assert_eq!(result.text, "vless://cached");
        assert_eq!(result.cache, super::CacheDisposition::NotModified);
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn successful_response_is_cached_atomically_with_owner_only_permissions() {
        let directory = std::env::temp_dir().join(format!(
            "proxy-rs-fetch-new-cache-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let url = serve_once(
            b"HTTP/1.1 200 OK\r\nContent-Length: 14\r\nETag: \"v2\"\r\nConnection: close\r\n\r\nvless://cached",
        );
        let result = fetch_one_with_options(
            &reqwest::Client::new(),
            &url,
            &FetchOptions {
                retries: 0,
                cache_dir: Some(directory.clone()),
                ..FetchOptions::default()
            },
            None,
        )
        .await
        .unwrap();
        let path = cache_path(&directory, &url);
        let cache = read_cache(&path, FetchOptions::default().max_response_bytes).unwrap();
        assert_eq!(cache.body, "vless://cached");
        assert_eq!(cache.etag.as_deref(), Some("\"v2\""));
        assert_eq!(result.text, "vless://cached");
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            assert_eq!(
                std::fs::metadata(&path).unwrap().permissions().mode() & 0o777,
                0o600
            );
        }
        std::fs::remove_dir_all(directory).unwrap();
    }

    #[tokio::test]
    async fn source_tls_verifies_by_default_and_allows_only_explicit_trust_override() {
        let generated = rcgen::generate_simple_self_signed(vec!["localhost".to_owned()]).unwrap();
        let cert = generated.cert.der().clone();
        let key =
            rustls::pki_types::PrivateKeyDer::Pkcs8(generated.key_pair.serialize_der().into());
        let options = FetchOptions {
            retries: 0,
            timeout: Duration::from_secs(3),
            ..FetchOptions::default()
        };

        let untrusted_url = serve_tls(cert.clone(), key.clone_key()).await;
        let rejected = fetch_one_with_options(
            &build_client(false).unwrap(),
            &untrusted_url,
            &options,
            None,
        )
        .await;
        assert!(
            rejected.is_err(),
            "default TLS verification must reject self-signed certs"
        );

        let trusted_url = serve_tls(cert.clone(), key.clone_key()).await;
        let trusted = reqwest::Client::builder()
            .add_root_certificate(reqwest::Certificate::from_der(cert.as_ref()).unwrap())
            .build()
            .unwrap();
        let accepted = fetch_one_with_options(&trusted, &trusted_url, &options, None)
            .await
            .unwrap();
        assert_eq!(accepted.text, "hello world");

        let insecure_url = serve_tls(cert, key).await;
        let accepted =
            fetch_one_with_options(&build_client(true).unwrap(), &insecure_url, &options, None)
                .await
                .unwrap();
        assert_eq!(accepted.text, "hello world");
    }
}
