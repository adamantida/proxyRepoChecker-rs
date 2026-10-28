Warning: truncated output (original token count: 24970)
Total output lines: 2809

use crate::cli::Cli;
use crate::parse::Proxy;
use crate::speed;
use anyhow::{bail, Context, Result};
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use serde_json::{json, Value};
use std::collections::{HashMap, HashSet};
use std::io::{Cursor, IsTerminal, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU16, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const UA: &str = "Mozilla/5.0 (proxy-rs)";

pub fn core_bin_name() -> &'static str {
    core_bin_name_for(std::env::consts::OS)
}

fn core_bin_name_for(os: &str) -> &'static str {
    if os == "windows" {
        "xray.exe"
    } else {
        "xray"
    }
}

fn asset_name() -> Result<&'static str> {
    use std::env::consts::{ARCH, OS};
    asset_name_for(OS, ARCH)
}

fn asset_name_for(os: &str, arch: &str) -> Result<&'static str> {
    Ok(match (os, arch) {
        ("windows", "x86_64") => "Xray-windows-64.zip",
        ("windows", "aarch64") => "Xray-windows-arm64-v8a.zip",
        ("linux", "x86_64") => "Xray-linux-64.zip",
        ("linux", "aarch64") => "Xray-linux-arm64-v8a.zip",
        ("macos", "x86_64") => "Xray-macos-64.zip",
        ("macos", "aarch64") => "Xray-macos-arm64-v8a.zip",
        _ => bail!("неподдерживаемая платформа для автозагрузки xray: {os}/{arch}"),
    })
}

/// URL релиза Xray-core под текущую ОС/архитектуру.
pub fn default_xray_url() -> Result<String> {
    Ok(format!(
        "https://github.com/XTLS/Xray-core/releases/latest/download/{}",
        asset_name()?
    ))
}

/// Проверяет, что файл — рабочее ядро (запускается `version` и содержит «Xray»).
pub fn valid_core(path: &Path) -> bool {
    let ok_size = std::fs::metadata(path)
        .map(|m| m.len() > 1_000_000)
        .unwrap_or(false);
    if !ok_size {
        return false;
    }
    let mut cmd = std::process::Command::new(path);
    cmd.arg("version")
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    match cmd.output() {
        Ok(out) => {
            let text = format!(
                "{}{}",
                String::from_utf8_lossy(&out.stdout),
                String::from_utf8_lossy(&out.stderr)
            );
            out.status.success() && text.to_lowercase().contains("xray")
        }
        Err(_) => false,
    }
}

/// Гарантирует наличие рабочего ядра: скачивает и распаковывает при необходимости.
pub async fn ensure_core(path: &Path, url: Option<&str>, no_download: bool) -> Result<()> {
    if valid_core(path) {
        return Ok(());
    }
    if no_download {
        bail!(
            "не найдено рабочее ядро xray: {} (автозагрузка отключена --no-xray-download)",
            path.display()
        );
    }
    let url = match url {
        Some(u) => u.to_string(),
        None => default_xray_url()?,
    };
    println!(">> xray: качаю ядро -> {}", path.display());
    if let Some(parent) = path.parent() {
        if !parent.as_os_str().is_empty() {
            std::fs::create_dir_all(parent)
                .with_context(|| format!("не удалось создать каталог {}", parent.display()))?;
        }
    }

    let client = reqwest::Client::builder()
        .user_agent(UA)
        .build()
        .context("reqwest client")?;
    let resp = client
        .get(&url)
        .timeout(Duration::from_secs(120))
        .send()
        .await
        .with_context(|| format!("не удалось скачать {url}"))?
        .error_for_status()
        .context("ошибка ответа при скачивании xray")?;

    let total = resp.content_length();
    let pb = ProgressBar::new(total.unwrap_or(0));
    pb.set_style(
        ProgressStyle::with_template(
            "{spinner:.green} xray {bytes}/{total_bytes} [{elapsed_precise}<{eta_precise}] {binary_bytes_per_sec}",
        )
        .expect("built-in Xray progress template is valid"),
    );

    let mut resp = resp;
    let mut bytes: Vec<u8> = Vec::with_capacity(total.unwrap_or(0) as usize);
    while let Some(chunk) = resp.chunk().await.context("тело ответа xray")? {
        bytes.extend_from_slice(&chunk);
        pb.inc(chunk.len() as u64);
    }
    pb.finish_and_clear();

    if bytes.len() < 1_000_000 {
        bail!(
            "скачан подозрительно маленький архив xray ({} байт)",
            bytes.len()
        );
    }

    let mut archive =
        zip::ZipArchive::new(Cursor::new(bytes)).context("не удалось прочитать zip с xray")?;
    let want = core_bin_name();
    let mut found: Option<Vec<u8>> = None;
    for i in 0..archive.len() {
        let mut entry = archive.by_index(i).context("чтение записи zip")?;
        let name = entry.name().to_string();
        let fname = name.rsplit(['/', '\\']).next().unwrap_or(&name);
        if fname == want {
            let mut buf = Vec::new();
            entry
                .read_to_end(&mut buf)
                .context("чтение xray из архива")?;
            found = Some(buf);
            break;
        }
    }
    let bin = found.ok_or_else(|| anyhow::anyhow!("в архиве не найден {want}"))?;
    if bin.len() < 1_000_000 {
        bail!("xray в архиве подозрительно маленький ({} байт)", bin.len());
    }

    crate::output::write_bytes_atomic_checked(path, &bin, Some(0o755), |tmp| {
        if valid_core(tmp) {
            Ok(())
        } else {
            bail!("скачанное ядро не запускается: {}", tmp.display())
        }
    })?;
    println!(">> xray: готово {}", path.display());
    Ok(())
}

#[derive(Clone)]
pub struct SpeedOpts {
    pub primary: Option<String>,
    pub connect_timeout_s: f64,
    pub timeout_s: f64,
    pub max_mb: f64,
    pub min_kb: f64,
    pub speed_probes: usize,
}

pub struct Checker {
    core: PathBuf,
    hysteria1_core: PathBuf,
    no_hysteria1_download: bool,
    hysteria1_core_ready: tokio::sync::OnceCell<std::result::Result<(), String>>,
    work: PathBuf,
    speed_threads: usize,
    start_timeout: Duration,
    base_port: u16,
    next: AtomicU16,
    bad_re: Regex,
    bad_outbounds: AtomicUsize,
    bad_outbound_rejections: Mutex<HashMap<String, &'static str>>,
    speed_unchecked: Mutex<HashSet<String>>,
    speed_measurements: Mutex<HashMap<String, speed::MeasurementSummary>>,
    speed_probe_results: Mutex<HashMap<String, Vec<speed::ProbeResult>>>,
}

fn bad_outbound_error_kind(log: &str) -> &'static str {
    let log = log.to_ascii_lowercase();
    if log.contains("unknown field") {
        "unknown_config_field"
    } else if log.contains("unknown protocol") || log.contains("protocol not found") {
        "unknown_protocol"
    } else if log.contains("unsupported") || log.contains("not supported") {
        "unsupported_feature"
    } else if log.contains("missing") || log.contains("required") {
        "missing_config_value"
    } else if log.contains("invalid") || log.contains("failed to parse") {
        "invalid_config_value"
    } else {
        "config_rejected"
    }
}

struct BatchConfig(PathBuf);

impl Drop for BatchConfig {
    fn drop(&mut self) {
        let _ = std::fs::remove_file(&self.0);
    }
}

struct ReporterTask(Option<tokio::task::JoinHandle<()>>);

impl ReporterTask {
    async fn stop(mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
            let _ = task.await;
        }
    }
}

impl Drop for ReporterTask {
    fn drop(&mut self) {
        if let Some(task) = self.0.take() {
            task.abort();
        }
    }
}

async fn port_open(port: u16) -> bool {
    port_open_with(port, |port| async move {
        tokio::net::TcpStream::connect(("127.0.0.1", port))
            .await
            .is_ok()
    })
    .await
}

async fn port_open_with<F, Fut>(port: u16, probe: F) -> bool
where
    F: FnOnce(u16) -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    tokio::time::timeout(Duration::from_millis(250), probe(port))
        .await
        .unwrap_or(false)
}

fn startup_deadline_reached(now: Instant, deadline: Instant) -> bool {
    now >= deadline
}

fn speed_meets_minimum(speed_kbs: f64, min_kb: f64) -> bool {
    speed_kbs + f64::EPSILON >= min_kb
}

fn supported_by_xray_speed_core(proxy: &Proxy) -> bool {
    (proxy.proto != "hysteria"
        && !(proxy.proto == "hysteria2" && proxy.settings["obfs"] == "gecko"))
        && (proxy.proto != "shadowsocks"
            || proxy.settings["method"]
                .as_str()
                .is_some_and(crate::outbound::ss_method_supported_by_xray))
}

pub(super) fn diagnose_supported(proxy: &Proxy) -> bool {
    matches!(
        proxy.proto,
        "vless" | "vmess" | "trojan" | "shadowsocks" | "hysteria2"
    ) && supported_by_xray_speed_core(proxy)
}

pub(super) async fn diagnose_runtime(
    proxy: Proxy,
    options: crate::diagnose::DiagnosticOptions,
    mut report: crate::diagnose::DiagnosticReport,
) -> crate::diagnose::DiagnosticReport {
    let checker = Checker {
        core: options.core.clone(),
        hysteria1_core: options
            .hysteria1_core
            .clone()
            .unwrap_or_else(|| crate::hysteria1::default_core_path_next_to(&options.core)),
        no_hysteria1_download: options.no_hysteria1_download,
        hysteria1_core_ready: tokio::sync::OnceCell::new(),
        work: options.workdir.clone(),
        speed_threads: 1,
        start_timeout: options.start_timeout.max(Duration::from_millis(1)),
        base_port: options.base_port,
        next: AtomicU16::new(options.base_port),
        bad_re: Regex::new(
            r#"(?i)failed to build outbound config with tag\s*[:=]?\s*["']?(out_\d+)"#,
        )
        .expect("built-in Xray error regex is valid"),
        bad_outbounds: AtomicUsize::new(0),
        bad_outbound_rejections: Mutex::new(HashMap::new()),
        speed_unchecked: Mutex::new(HashSet::new()),
        speed_measurements: Mutex::new(HashMap::new()),
        speed_probe_results: Mutex::new(HashMap::new()),
    };
    let port = checker.alloc_port().await;
    let config = checker.build_config(&[(proxy, port)]);
    let config_path = checker.work.join(format!("diagnose_{port}.json"));
    let _cleanup = BatchConfig(config_path.clone());
    let serialized = match serde_json::to_vec(&config) {
        Ok(serialized) => serialized,
        Err(error) => {
            report.failure = Some(crate::diagnose::DiagnosticFailure::RuntimeStart(format!(
                "serialize Xray config: {error}"
            )));
            return report;
        }
    };
    if let Err(error) = std::fs::write(&config_path, serialized) {
        report.failure = Some(crate::diagnose::DiagnosticFailure::RuntimeStart(format!(
            "write Xray config {}: {error}",
            config_path.display()
        )));
        return report;
    }

    let mut command = tokio::process::Command::new(&checker.core);
    command
        .arg("run")
        .arg("-c")
        .arg(&config_path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .kill_on_drop(true);
    #[cfg(windows)]
    command.creation_flags(CREATE_NO_WINDOW);

    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => {
            report.failure = Some(crate::diagnose::DiagnosticFailure::RuntimeStart(format!(
                "start Xray core {}: {error}",
                checker.core.display()
            )));
            return report;
        }
    };
    report.core_started = Some(true);

    let deadline = Instant::now() + checker.start_timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => {
                report.failure = Some(crate::diagnose::DiagnosticFailure::RuntimeExited(format!(
                    "Xray core exited before SOCKS became ready ({status})"
                )));
                return report;
            }
            Err(error) => {
                report.failure = Some(crate::diagnose::DiagnosticFailure::RuntimeExited(format!(
                    "wait for Xray core: {error}"
                )));
                let _ = child.kill().await;
                return report;
            }
            Ok(None) => {}
        }
        match socks5_handshake(port).await {
            Ok(()) => {
                report.socks_ready = Some(true);
                break;
            }
            Err(reason) if Instant::now() >= deadline => {
                report.failure = Some(if reason.contains("timed out") {
                    crate::diagnose::DiagnosticFailure::StartupTimeout
                } else {
                    crate::diagnose::DiagnosticFailure::SocksNotReady(reason)
                });
                let _ = child.kill().await;
                return report;
            }
            Err(_) => {}
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }

    let opts = options.speed_opts();
    let measurement = speed::measure_detailed(
        port,
        opts.primary.as_deref(),
        opts.connect_timeout_s,
        opts.timeout_s,
        opts.max_mb,
        opts.speed_probes,
    )
    .await;
    report.download_succeeded = Some(measurement.successful_probes > 0);
    report.speed_median_kbs = Some(measurement.median_kbs);
    report.measurement = Some(measurement.into());
    if measurement.successful_probes == 0 {
        report.failure = Some(crate::diagnose::DiagnosticFailure::DownloadFailed);
    } else if !speed_meets_minimum(measurement.median_kbs, opts.min_kb) {
        report.failure = Some(crate::diagnose::DiagnosticFailure::BelowMinimum {
            measured_kbs: measurement.median_kbs,
            minimum_kbs: opts.min_kb,
        });
    }
    let _ = child.kill().await;
    report
}

async fn socks5_handshake(port: u16) -> std::result::Result<(), String> {
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    let address = format!("127.0.0.1:{port}");
    let operation = async {
        let mut stream = tokio::net::TcpStream::connect(&address)
            .await
            .map_err(|error| format!("connect to local SOCKS listener: {error}"))?;
        stream
            .write_all(&[5, 1, 0])
            .await
            .map_err(|error| format!("send SOCKS5 greeting: {error}"))?;
        let mut response = [0; 2];
        stream
            .read_exact(&mut response)
            .await
            .map_err(|error| format!("read SOCKS5 greeting response: {error}"))?;
        if response != [5, 0] {
            return Err(format!("SOCKS5 rejected no-auth greeting: {response:?}"));
        }
        Ok(())
    };
    tokio::time::timeout(Duration::from_millis(300), operation)
        .await
        .map_err(|_| "SOCKS5 handshake timed out".to_string())?
}

async fn read_output(child: tokio::process::Child) -> String {
    format_read_output(child.wait_with_output().await)
}

fn format_read_output(output: std::io::Result<std::process::Output>) -> String {
    match output {
        Ok(out) => format!(
            "{}\n{}",
            String::from_utf8_lossy(&out.stdout),
            String::from_utf8_lossy(&out.stderr)
        ),
        Err(e) => format!("чтение вывода ядра: {e}"),
    }
}

impl Checker {
    /// Detailed speed results collected by the regular pipeline, keyed by
    /// `Proxy::…16970 tokens truncated…    .run_all(
                vec![sample_proxy(), sample_proxy()],
                2,
                1,
                speed_opts(),
                "test",
                None,
            )
            .await
            .unwrap();

        assert!(result.is_empty());
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[test]
    fn build_config_pairs_each_inbound_with_its_outbound_and_route() {
        let checker = Checker {
            core: PathBuf::new(),
            hysteria1_core: PathBuf::new(),
            no_hysteria1_download: true,
            hysteria1_core_ready: tokio::sync::OnceCell::new(),
            work: PathBuf::new(),
            speed_threads: 1,
            start_timeout: Duration::from_secs(1),
            base_port: 12000,
            next: AtomicU16::new(12000),
            bad_re: Regex::new("out_\\d+").unwrap(),
            bad_outbounds: AtomicUsize::new(0),
            bad_outbound_rejections: Mutex::new(HashMap::new()),
            speed_unchecked: Mutex::new(HashSet::new()),
            speed_measurements: Mutex::new(HashMap::new()),
            speed_probe_results: Mutex::new(HashMap::new()),
        };
        let config = checker.build_config(&[(sample_proxy(), 12001)]);
        assert_eq!(config["inbounds"][0]["port"], 12001);
        assert_eq!(config["inbounds"][0]["protocol"], "socks");
        assert_eq!(config["outbounds"][0]["tag"], "out_12001");
        assert_eq!(config["outbounds"][0]["protocol"], "vless");
        assert_eq!(config["outbounds"][0]["streamSettings"]["network"], "ws");
        assert_eq!(config["routing"]["rules"][0]["inboundTag"][0], "in_12001");
        assert_eq!(config["routing"]["rules"][0]["outboundTag"], "out_12001");
    }

    #[test]
    fn build_config_translates_hysteria2_to_xrays_hysteria_outbound() {
        let checker = Checker {
            core: PathBuf::new(),
            hysteria1_core: PathBuf::new(),
            no_hysteria1_download: true,
            hysteria1_core_ready: tokio::sync::OnceCell::new(),
            work: PathBuf::new(),
            speed_threads: 1,
            start_timeout: Duration::from_secs(1),
            base_port: 12000,
            next: AtomicU16::new(12000),
            bad_re: Regex::new("out_\\d+").unwrap(),
            bad_outbounds: AtomicUsize::new(0),
            bad_outbound_rejections: Mutex::new(HashMap::new()),
            speed_unchecked: Mutex::new(HashSet::new()),
            speed_measurements: Mutex::new(HashMap::new()),
            speed_probe_results: Mutex::new(HashMap::new()),
        };
        let mut proxy = sample_proxy();
        proxy.proto = "hysteria2";
        proxy.settings = json!({"version": 2, "address": "hy2.example", "port": 443});
        proxy.stream = Some(json!({
            "network": "hysteria",
            "security": "tls",
            "tlsSettings": {"serverName": "sni.example"},
            "hysteriaSettings": {"version": 2, "auth": "test-password"}
        }));

        let config = checker.build_config(&[(proxy, 12001)]);

        assert_eq!(config["outbounds"][0]["protocol"], "hysteria");
        assert_eq!(config["outbounds"][0]["settings"]["version"], 2);
        assert_eq!(config["outbounds"][0]["settings"]["address"], "hy2.example");
        assert_eq!(config["outbounds"][0]["settings"]["port"], 443);
        assert_eq!(
            config["outbounds"][0]["streamSettings"]["network"],
            "hysteria"
        );
        assert_eq!(
            config["outbounds"][0]["streamSettings"]["hysteriaSettings"]["auth"],
            "test-password"
        );
    }

    #[test]
    fn build_config_omits_stream_settings_when_proxy_has_none() {
        let checker = Checker {
            core: PathBuf::new(),
            hysteria1_core: PathBuf::new(),
            no_hysteria1_download: true,
            hysteria1_core_ready: tokio::sync::OnceCell::new(),
            work: PathBuf::new(),
            speed_threads: 1,
            start_timeout: Duration::from_secs(1),
            base_port: 12000,
            next: AtomicU16::new(12000),
            bad_re: Regex::new("out_\\d+").unwrap(),
            bad_outbounds: AtomicUsize::new(0),
            bad_outbound_rejections: Mutex::new(HashMap::new()),
            speed_unchecked: Mutex::new(HashSet::new()),
            speed_measurements: Mutex::new(HashMap::new()),
            speed_probe_results: Mutex::new(HashMap::new()),
        };
        let mut proxy = sample_proxy();
        proxy.stream = None;

        let config = checker.build_config(&[(proxy, 12001)]);

        assert!(config["outbounds"][0].get("streamSettings").is_none());
    }

    #[tokio::test]
    async fn run_all_empty_input_returns_without_starting_xray() {
        let checker = Arc::new(Checker {
            core: PathBuf::new(),
            hysteria1_core: PathBuf::new(),
            no_hysteria1_download: true,
            hysteria1_core_ready: tokio::sync::OnceCell::new(),
            work: PathBuf::new(),
            speed_threads: 1,
            start_timeout: Duration::from_secs(1),
            base_port: 12000,
            next: AtomicU16::new(12000),
            bad_re: Regex::new("out_\\d+").unwrap(),
            bad_outbounds: AtomicUsize::new(0),
            bad_outbound_rejections: Mutex::new(HashMap::new()),
            speed_unchecked: Mutex::new(HashSet::new()),
            speed_measurements: Mutex::new(HashMap::new()),
            speed_probe_results: Mutex::new(HashMap::new()),
        });
        let got = checker
            .run_all(
                Vec::new(),
                10,
                1,
                SpeedOpts {
                    primary: None,
                    connect_timeout_s: 1.0,
                    timeout_s: 1.0,
                    max_mb: 1.0,
                    min_kb: 0.0,
                    speed_probes: 1,
                },
                "test",
                None,
            )
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    #[tokio::test]
    async fn run_all_routes_hysteria1_to_its_runtime_without_starting_xray() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let checker = test_checker(
            dir.path("unused-xray"),
            work.clone(),
            Duration::from_secs(1),
        );
        let proxy = crate::parse::parse_link(
            "hysteria://hy.example:443?auth=token&upmbps=10&downmbps=20#h1",
        )
        .unwrap();

        let got = checker
            .run_all(vec![proxy], 1, 1, speed_opts(), "test", None)
            .await
            .unwrap();

        // The isolated test explicitly disables downloads. The Hysteria route
        // must be attempted and accounted for, without trying to run Xray.
        assert!(got.is_empty());
        assert_eq!(checker.speed_unchecked_keys().len(), 1);
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn run_all_separates_hysteria_and_unverified_protocols_from_xray_checks() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let checker = test_checker(
            dir.path("missing-xray"),
            work.clone(),
            Duration::from_secs(1),
        );
        let hysteria1 = crate::parse::parse_link(
            "hysteria://hy.example:443?auth=token&upmbps=10&downmbps=20#h1",
        )
        .unwrap();
        let hysteria2_gecko = crate::parse::parse_link(
            "hysteria2://token@hy2.example:443?obfs=gecko&obfs-password=secret#h2-gecko",
        )
        .unwrap();
        let mut xray_checked = sample_proxy();
        xray_checked.proto = "vless";

        let got = checker
            .run_all(
                vec![hysteria1.clone(), hysteria2_gecko.clone(), xray_checked],
                3,
                1,
                speed_opts(),
                "mixed protocols",
                None,
            )
            .await
            .unwrap();

        assert!(got.is_empty());
        let unchecked = checker.speed_unchecked_keys();
        assert!(unchecked.contains(&hysteria1.key()));
        assert!(unchecked.contains(&hysteria2_gecko.key()));
        assert_eq!(unchecked.len(), 2);
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_all_filters_slow_proxies_and_reports_good_file_write_errors() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let core = dir.path("mock-xray");
        let script = r##"#!/bin/sh
if [ "$1" = version ]; then
  echo 'Xray mock'
  exit 0
fi
exec python3 - "$3" <<'PY'
import json, socket, sys, threading
port = int(json.load(open(sys.argv[1]))["inbounds"][0]["port"])
server = socket.socket()
server.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
server.bind(("127.0.0.1", port))
server.listen()
def exact(client, count):
    data = b""
    while len(data) < count:
        part = client.recv(count - len(data))
        if not part:
            raise EOFError()
        data += part
    return data
def handle(client):
    try:
        _, methods = exact(client, 2)
        exact(client, methods)
        client.sendall(b"\x05\x00")
        _, _, _, address_type = exact(client, 4)
        if address_type == 1:
            exact(client, 4)
        elif address_type == 3:
            length = exact(client, 1)[0]
            exact(client, length)
        elif address_type == 4:
            exact(client, 16)
        exact(client, 2)
        client.sendall(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
        request = b""
        while b"\r\n\r\n" not in request:
            part = client.recv(4096)
            if not part:
                return
            request += part
        body = b"x" * 2048
        client.sendall(b"HTTP/1.1 200 OK\r\nContent-Length: 2048\r\nConnection: close\r\n\r\n" + body)
    except Exception:
        pass
    finally:
        client.close()
while True:
    client, _ = server.accept()
    threading.Thread(target=handle, args=(client,), daemon=True).start()
PY
"##;
        std::fs::write(&core, script).unwrap();
        let mut permissions = std::fs::metadata(&core).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&core, permissions).unwrap();

        let checker = test_checker(core, work.clone(), Duration::from_secs(3));
        let good_path = dir.path("good.txt");
        let proxies = checker
            .run_all(
                vec![sample_proxy()],
                1,
                1,
                SpeedOpts {
                    primary: Some("http://example.test/download".into()),
                    connect_timeout_s: 1.0,
                    timeout_s: 2.0,
                    max_mb: 0.5,
                    min_kb: 1.0,
                    speed_probes: 1,
                },
                "test",
                Some(good_path.to_string_lossy().into_owned()),
            )
            .await
            .unwrap();

        assert_eq!(proxies.len(), 1);
        assert!(proxies[0].speed_kbs >= 1.0);
        assert_eq!(
            std::fs::read_to_string(good_path).unwrap(),
            format!("{}\n", crate::output::line(&proxies[0], true))
        );
        let previous_good = std::fs::read_to_string(dir.path("good.txt")).unwrap();
        assert_eq!(std::fs::read_dir(&work).unwrap().count(), 0);

        let slow = checker
            .run_all(
                vec![sample_proxy()],
                1,
                1,
                SpeedOpts {
                    primary: Some("http://example.test/download".into()),
                    connect_timeout_s: 1.0,
                    timeout_s: 2.0,
                    max_mb: 0.5,
                    min_kb: f64::MAX,
                    speed_probes: 1,
                },
                "slow proxy",
                Some(dir.path("good.txt").to_string_lossy().into_owned()),
            )
            .await
            .unwrap();
        assert!(slow.is_empty());
        assert_eq!(
            std::fs::read_to_string(dir.path("good.txt")).unwrap(),
            previous_good,
            "an empty speed result must not erase the last good output"
        );

        let invalid_good_path = dir.path("good-directory");
        std::fs::create_dir(&invalid_good_path).unwrap();
        let error = checker
            .run_all(
                vec![sample_proxy(), sample_proxy()],
                1,
                2,
                SpeedOpts {
                    primary: Some("http://example.test/download".into()),
                    connect_timeout_s: 1.0,
                    timeout_s: 2.0,
                    max_mb: 0.5,
                    min_kb: 1.0,
                    speed_probes: 1,
                },
                "write error",
                Some(invalid_good_path.to_string_lossy().into_owned()),
            )
            .await
            .unwrap_err();
        assert!(error
            .to_string()
            .contains("ошибка промежуточной записи good-файла"));
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn reporter_stop_handles_missing_task_and_aborts_running_task() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        ReporterTask(None).stop().await;

        let flag = DropFlag(Arc::clone(&dropped));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _flag = flag;
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.unwrap();
        ReporterTask(Some(task)).stop().await;

        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn progress_reporter_keeps_reporting_until_all_items_finish() {
        let good = Arc::new(Mutex::new(vec![sample_proxy()]));
        let pb = ProgressBar::hidden();
        pb.set_length(2);
        let reporter = spawn_progress_reporter(
            pb.clone(),
            good,
            Instant::now(),
            false,
            Duration::from_millis(1),
        )
        .unwrap();

        tokio::time::timeout(Duration::from_secs(1), async {
            while !reporter.is_finished() {
                pb.inc(1);
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("reporter should exit after progress reaches the total");
        reporter.await.unwrap();
    }

    #[tokio::test]
    async fn bad_outbound_error_splits_unmatched_batch_and_cleans_configs() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let checker = test_checker(
            dir.path("missing-xray"),
            work.clone(),
            Duration::from_secs(1),
        );
        let pb = ProgressBar::hidden();
        pb.set_length(2);
        let result = Checker::handle_failure(
            &checker,
            "fixture core exit without an outbound tag",
            vec![
                (sample_proxy(), checker.base_port),
                (sample_proxy(), checker.base_port.wrapping_add(1)),
            ],
            speed_opts(),
            pb.clone(),
        )
        .await;

        assert!(result.is_empty());
        assert_eq!(checker.bad_outbounds(), 0);
        assert_eq!(pb.position(), 2);
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }
}
