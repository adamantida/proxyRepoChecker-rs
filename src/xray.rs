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
    /// `Proxy::key()`. Entries are recorded even when every probe fails.
    pub fn speed_measurements(&self) -> HashMap<String, speed::MeasurementSummary> {
        self.speed_measurements.lock().unwrap().clone()
    }

    pub fn speed_probe_results(&self) -> HashMap<String, Vec<speed::ProbeResult>> {
        self.speed_probe_results.lock().unwrap().clone()
    }

    pub fn new(cli: &Cli) -> Result<Arc<Self>> {
        let core = PathBuf::from(&cli.core);
        if !core.exists() {
            anyhow::bail!("не найдено ядро: {}", core.display());
        }
        let work = PathBuf::from(&cli.workdir);
        std::fs::create_dir_all(&work)
            .with_context(|| format!("не удалось создать {}", work.display()))?;
        Ok(Arc::new(Self {
            hysteria1_core: crate::hysteria1::default_core_path_next_to(&core),
            no_hysteria1_download: cli.no_hysteria1_download,
            hysteria1_core_ready: tokio::sync::OnceCell::new(),
            core,
            work,
            speed_threads: cli.speed_threads.max(1),
            start_timeout: Duration::from_secs_f64(cli.core_start_timeout.max(1.0)),
            base_port: cli.lport,
            next: AtomicU16::new(cli.lport),
            bad_re: Regex::new(
                r#"(?i)failed to build outbound config with tag\s*[:=]?\s*["']?(out_\d+)"#,
            )
            .unwrap(),
            bad_outbounds: AtomicUsize::new(0),
            bad_outbound_rejections: Mutex::new(HashMap::new()),
            speed_unchecked: Mutex::new(HashSet::new()),
            speed_measurements: Mutex::new(HashMap::new()),
            speed_probe_results: Mutex::new(HashMap::new()),
        }))
    }

    /// Сколько битых outbound было убрано в фазе скорости.
    pub fn bad_outbounds(&self) -> usize {
        self.bad_outbounds.load(Ordering::Relaxed)
    }

    /// Proxy keys whose Xray outbound configuration was rejected before probing.
    pub fn bad_outbound_rejections(&self) -> HashMap<String, &'static str> {
        self.bad_outbound_rejections.lock().unwrap().clone()
    }

    pub fn speed_unchecked_keys(&self) -> HashSet<String> {
        self.speed_unchecked.lock().unwrap().clone()
    }

    async fn alloc_port(&self) -> u16 {
        self.alloc_port_with(port_open).await
    }

    async fn ensure_hysteria1_core(&self) -> std::result::Result<(), String> {
        self.hysteria1_core_ready
            .get_or_init(|| async {
                crate::hysteria1::ensure_core(&self.hysteria1_core, self.no_hysteria1_download)
                    .await
                    .map_err(|error| format!("{error:#}"))
            })
            .await
            .clone()
    }

    async fn run_hysteria1_stage(
        self: &Arc<Self>,
        proxies: Vec<Proxy>,
        opts: SpeedOpts,
        pb: ProgressBar,
    ) -> Vec<Proxy> {
        if proxies.is_empty() {
            return Vec::new();
        }
        if let Err(error) = self.ensure_hysteria1_core().await {
            eprintln!("[предупреждение] проверка Hysteria 1 пропущена: {error}");
            self.speed_unchecked
                .lock()
                .unwrap()
                .extend(proxies.iter().map(Proxy::key));
            pb.inc(proxies.len() as u64);
            return Vec::new();
        }

        let concurrency = self.speed_threads.max(1);
        stream::iter(proxies)
            .map(|mut proxy| {
                let this = Arc::clone(self);
                let opts = opts.clone();
                let pb = pb.clone();
                async move {
                    let port = this.alloc_port().await;
                    let outcome = crate::hysteria1::check_proxy_with_probes(
                        &this.hysteria1_core,
                        &this.work,
                        &proxy,
                        port,
                        &opts,
                    )
                    .await
                    .unwrap_or_default();
                    proxy.speed_kbs = outcome.measurement.median_kbs;
                    let key = proxy.key();
                    this.speed_measurements
                        .lock()
                        .unwrap()
                        .insert(key.clone(), outcome.measurement);
                    this.speed_probe_results
                        .lock()
                        .unwrap()
                        .insert(key, outcome.probes);
                    pb.inc(1);
                    speed_meets_minimum(proxy.speed_kbs, opts.min_kb).then_some(proxy)
                }
            })
            .buffer_unordered(concurrency)
            .filter_map(|proxy| async move { proxy })
            .collect()
            .await
    }

    async fn alloc_port_with<F, Fut>(&self, is_open: F) -> u16
    where
        F: Fn(u16) -> Fut,
        Fut: std::future::Future<Output = bool>,
    {
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let next = if cur >= 65_000 {
                self.base_port
            } else {
                cur + 1
            };
            if self
                .next
                .compare_exchange(cur, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                // пропускаем занятые и зарезервированные системой порты
                if std::net::TcpListener::bind(("127.0.0.1", cur)).is_err() {
                    continue;
                }
                if is_open(cur).await {
                    continue;
                }
                return cur;
            }
        }
    }

    fn build_config(&self, mapping: &[(Proxy, u16)]) -> Value {
        let mut inbounds = Vec::new();
        let mut outbounds = Vec::new();
        let mut rules = Vec::new();
        for (p, port) in mapping {
            let in_tag = format!("in_{port}");
            let out_tag = format!("out_{port}");
            inbounds.push(json!({
                "port": port,
                "listen": "127.0.0.1",
                "protocol": "socks",
                "tag": in_tag,
                "settings": {"udp": false}
            }));
            let mut outbound = json!({
                "protocol": if p.proto == "hysteria2" {
                    // Xray calls its Hysteria 2 outbound simply `hysteria`.
                    "hysteria"
                } else {
                    p.proto
                },
                "tag": out_tag,
                "settings": p.settings
            });
            if let Some(stream) = &p.stream {
                outbound["streamSettings"] = stream.clone();
            }
            outbounds.push(outbound);
            rules.push(json!({
                "type": "field",
                "inboundTag": [in_tag],
                "outboundTag": out_tag
            }));
        }
        json!({
            "log": {"loglevel": "warning"},
            "inbounds": inbounds,
            "outbounds": outbounds,
            "routing": {"domainStrategy": "AsIs", "rules": rules}
        })
    }

    pub fn run_all(
        self: &Arc<Self>,
        proxies: Vec<Proxy>,
        batch: usize,
        workers: usize,
        opts: SpeedOpts,
        label: &str,
        good_path: Option<String>,
    ) -> BoxFuture<'static, Result<Vec<Proxy>>> {
        let this = Arc::clone(self);
        let label = label.to_string();
        Box::pin(async move {
            if proxies.is_empty() {
                return Ok(Vec::new());
            }
            let total = proxies.len();
            let chunks: Vec<Vec<Proxy>> =
                proxies.chunks(batch.max(1)).map(|c| c.to_vec()).collect();
            let pb = ProgressBar::new(total as u64).with_style(
                ProgressStyle::with_template(&format!(
                    "{{spinner:.green}} {label} {{pos}}/{{len}} прокси [{{elapsed_precise}}<{{eta_precise}}]"
                ))
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
            );
            let all_good: Arc<Mutex<Vec<Proxy>>> = Arc::new(Mutex::new(Vec::new()));
            let write_error: Arc<Mutex<Option<String>>> = Arc::new(Mutex::new(None));
            let start = Instant::now();
            let is_tty = std::io::stderr().is_terminal();

            // В не-TTY (CI) indicatif ничего не рисует — печатаем прогресс раз в минуту.
            let reporter = ReporterTask(spawn_progress_reporter(
                pb.clone(),
                Arc::clone(&all_good),
                start,
                is_tty,
                Duration::from_secs(60),
            ));

            let nested: Vec<Vec<Proxy>> = stream::iter(chunks)
                .map(|chunk| {
                    let this = Arc::clone(&this);
                    let pb = pb.clone();
                    let opts = opts.clone();
                    let all_good = Arc::clone(&all_good);
                    let good_path = good_path.clone();
                    let write_error = Arc::clone(&write_error);
                    async move {
                        let res = this.run_stage(chunk, opts, pb.clone()).await;
                        if let Some(path) = &good_path {
                            if !res.is_empty() {
                                let mut g = all_good.lock().unwrap();
                                g.extend(res.iter().cloned());
                                let mut sorted = g.clone();
                                sort_by_speed(&mut sorted);
                                let lines: Vec<String> = sorted
                                    .iter()
                                    .map(|p| crate::output::line(p, true))
                                    .collect();
                                if let Err(error) = crate::output::write_lines_atomic(path, &lines)
                                {
                                    let mut first = write_error.lock().unwrap();
                                    if first.is_none() {
                                        *first = Some(format!("{error:#}"));
                                    }
                                }
                            }
                        }
                        res
                    }
                })
                .buffer_unordered(workers.max(1))
                .collect()
                .await;
            pb.finish_and_clear();
            reporter.stop().await;
            if !is_tty {
                let good = all_good.lock().unwrap().len();
                println!(
                    ">> {}",
                    progress_line(
                        total as u64,
                        total as u64,
                        start.elapsed().as_secs_f64(),
                        good
                    )
                );
            }
            if let Some(error) = write_error.lock().unwrap().take() {
                anyhow::bail!("ошибка промежуточной записи good-файла: {error}");
            }
            Ok(nested.into_iter().flatten().collect())
        })
    }

    fn run_stage(
        self: &Arc<Self>,
        items: Vec<Proxy>,
        opts: SpeedOpts,
        pb: ProgressBar,
    ) -> BoxFuture<'static, Vec<Proxy>> {
        self.run_stage_with_serializer(items, opts, pb, serde_json::to_vec)
    }

    fn run_stage_with_serializer<F>(
        self: &Arc<Self>,
        items: Vec<Proxy>,
        opts: SpeedOpts,
        pb: ProgressBar,
        serialize: F,
    ) -> BoxFuture<'static, Vec<Proxy>>
    where
        F: Fn(&Value) -> std::result::Result<Vec<u8>, serde_json::Error> + Send + 'static,
    {
        let this = Arc::clone(self);
        Box::pin(async move {
            if items.is_empty() {
                return Vec::new();
            }
            let hysteria1_items: Vec<Proxy> = items
                .iter()
                .filter(|proxy| proxy.proto == "hysteria")
                .cloned()
                .collect();
            let unsupported = items
                .iter()
                .filter(|proxy| proxy.proto != "hysteria" && !supported_by_xray_speed_core(proxy))
                .count() as u64;
            if unsupported > 0 {
                this.speed_unchecked.lock().unwrap().extend(
                    items
                        .iter()
                        .filter(|proxy| {
                            proxy.proto != "hysteria" && !supported_by_xray_speed_core(proxy)
                        })
                        .map(Proxy::key),
                );
            }
            let hysteria1_good = this
                .run_hysteria1_stage(hysteria1_items, opts.clone(), pb.clone())
                .await;
            if unsupported > 0 {
                eprintln!(
                    "[предупреждение] Xray пропускает speed-check для {unsupported} прокси с неподдерживаемым Xray транспортом или методом Shadowsocks"
                );
                pb.inc(unsupported);
            }
            let items: Vec<Proxy> = items
                .into_iter()
                .filter(|proxy| proxy.proto != "hysteria" && supported_by_xray_speed_core(proxy))
                .collect();
            if items.is_empty() {
                return hysteria1_good;
            }
            let total_items = items.len() as u64;
            let mut mapping: Vec<(Proxy, u16)> = Vec::with_capacity(items.len());
            for p in items {
                let port = this.alloc_port().await;
                mapping.push((p, port));
            }

            let config = this.build_config(&mapping);
            let cfg_path = this.work.join(format!("batch_{}.json", mapping[0].1));
            let _config = BatchConfig(cfg_path.clone());
            let bytes = match serialize(&config) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[ошибка] сериализация batch-конфига: {e}");
                    pb.inc(total_items);
                    return hysteria1_good;
                }
            };
            if let Err(e) = std::fs::write(&cfg_path, bytes) {
                eprintln!("[ошибка] запись {}: {e}", cfg_path.display());
                pb.inc(total_items);
                return hysteria1_good;
            }

            let mut cmd = tokio::process::Command::new(&this.core);
            cmd.arg("run")
                .arg("-c")
                .arg(&cfg_path)
                .stdin(Stdio::null())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .kill_on_drop(true);
            #[cfg(windows)]
            {
                cmd.creation_flags(CREATE_NO_WINDOW);
            }

            let mut child = match cmd.spawn() {
                Ok(c) => c,
                Err(e) => {
                    eprintln!("[ошибка] не удалось запустить ядро: {e}");
                    pb.inc(total_items);
                    return hysteria1_good;
                }
            };

            let first_port = mapping[0].1;
            let deadline = Instant::now() + this.start_timeout;
            let mut started = false;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        let text = read_output(child).await;
                        let mut passed =
                            Checker::handle_failure(&this, &text, mapping, opts, pb.clone()).await;
                        passed.extend(hysteria1_good);
                        return passed;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        let mut passed =
                            Checker::handle_wait_error(e, child, cfg_path, total_items, pb).await;
                        passed.extend(hysteria1_good);
                        return passed;
                    }
                }
                if port_open(first_port).await {
                    started = true;
                    break;
                }
                if startup_deadline_reached(Instant::now(), deadline) {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            if !started {
                let _ = child.kill().await;
                let text = read_output(child).await;
                let mut passed =
                    Checker::handle_failure(&this, &text, mapping, opts, pb.clone()).await;
                passed.extend(hysteria1_good);
                return passed;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;

            let concurrency = this.speed_threads;
            let results: Vec<Proxy> = stream::iter(mapping)
                .map(|(p, port)| {
                    let opts = opts.clone();
                    let pb = pb.clone();
                    let this = Arc::clone(&this);
                    async move {
                        let probes = speed::measure_detailed_probes(
                            port,
                            opts.primary.as_deref(),
                            opts.connect_timeout_s,
                            opts.timeout_s,
                            opts.max_mb,
                            opts.speed_probes,
                        )
                        .await;
                        let measurement = speed::summarize_probe_results(&probes);
                        pb.inc(1);
                        let kbs = measurement.median_kbs;
                        let key = p.key();
                        this.speed_measurements
                            .lock()
                            .unwrap()
                            .insert(key.clone(), measurement);
                        this.speed_probe_results.lock().unwrap().insert(key, probes);
                        if speed_meets_minimum(kbs, opts.min_kb) {
                            let mut p = p;
                            p.speed_kbs = kbs;
                            Some(p)
                        } else {
                            None
                        }
                    }
                })
                .buffer_unordered(concurrency)
                .filter_map(|x| async move { x })
                .collect()
                .await;

            let _ = child.kill().await;
            let mut passed = results;
            passed.extend(hysteria1_good);
            passed
        })
    }

    async fn handle_wait_error(
        error: std::io::Error,
        mut child: tokio::process::Child,
        cfg_path: PathBuf,
        total_items: u64,
        pb: ProgressBar,
    ) -> Vec<Proxy> {
        eprintln!("[ошибка] try_wait: {error}");
        let _ = child.kill().await;
        drop(BatchConfig(cfg_path));
        pb.inc(total_items);
        Vec::new()
    }

    async fn handle_failure(
        this: &Arc<Self>,
        log: &str,
        mapping: Vec<(Proxy, u16)>,
        opts: SpeedOpts,
        pb: ProgressBar,
    ) -> Vec<Proxy> {
        if let Some(caps) = this.bad_re.captures(log) {
            let tag = caps[1].to_string();
            if let Ok(port) = tag.trim_start_matches("out_").parse::<u16>() {
                if let Some(pos) = mapping.iter().position(|(_, p)| *p == port) {
                    this.bad_outbounds.fetch_add(1, Ordering::Relaxed);
                    this.bad_outbound_rejections
                        .lock()
                        .unwrap()
                        .insert(mapping[pos].0.key(), bad_outbound_error_kind(log));
                    pb.inc(1);
                    let mut rest = mapping;
                    rest.remove(pos);
                    let items = rest.into_iter().map(|(p, _)| p).collect();
                    return this.run_stage(items, opts, pb).await;
                }
            }
        }
        if mapping.len() > 1 {
            let mid = mapping.len() / 2;
            let left: Vec<Proxy> = mapping[..mid].iter().map(|(p, _)| p.clone()).collect();
            let right: Vec<Proxy> = mapping[mid..].iter().map(|(p, _)| p.clone()).collect();
            let (a, b) = tokio::join!(
                this.run_stage(left, opts.clone(), pb.clone()),
                this.run_stage(right, opts, pb)
            );
            let mut out = a;
            out.extend(b);
            out
        } else {
            pb.inc(1);
            Vec::new()
        }
    }
}

/// Форматирует секунды как `ЧЧ:ММ:СС`.
fn fmt_hms(secs: f64) -> String {
    let total = secs.max(0.0) as u64;
    format!(
        "{:02}:{:02}:{:02}",
        total / 3600,
        (total % 3600) / 60,
        total % 60
    )
}

/// Строка периодического прогресса: `[done/total] прошло | осталось | good N`.
fn progress_line(pos: u64, len: u64, elapsed_s: f64, good: usize) -> String {
    let width = len.to_string().len().max(1);
    let eta = if len > pos {
        let rate = pos as f64 / elapsed_s.max(f64::EPSILON);
        if rate > 0.0 {
            format!("~{}", fmt_hms((len - pos) as f64 / rate))
        } else {
            "~--:--:--".to_string()
        }
    } else {
        "00:00:00".to_string()
    };
    format!(
        "[{:>width$}/{}] прошло {} | осталось {} | good {}",
        pos,
        len,
        fmt_hms(elapsed_s),
        eta,
        good,
        width = width
    )
}

fn sort_by_speed(proxies: &mut [Proxy]) {
    proxies.sort_by(compare_speed_descending);
}

fn compare_speed_descending(a: &Proxy, b: &Proxy) -> std::cmp::Ordering {
    b.speed_kbs
        .partial_cmp(&a.speed_kbs)
        .unwrap_or(std::cmp::Ordering::Equal)
}

fn spawn_progress_reporter(
    pb: ProgressBar,
    all_good: Arc<Mutex<Vec<Proxy>>>,
    start: Instant,
    is_tty: bool,
    interval: Duration,
) -> Option<tokio::task::JoinHandle<()>> {
    if is_tty {
        None
    } else {
        Some(tokio::spawn(async move {
            let mut tick = tokio::time::interval(interval);
            loop {
                tick.tick().await;
                let pos = pb.position();
                let len = pb.length().unwrap_or(0);
                let good = all_good.lock().unwrap().len();
                println!(
                    ">> {}",
                    progress_line(pos, len, start.elapsed().as_secs_f64(), good)
                );
                if len > 0 && pos >= len {
                    break;
                }
            }
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::Parser;
    use std::io::Write;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static TEST_ID: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = TEST_ID.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("proxy-rs-xray-{}-{id}", std::process::id()));
            std::fs::create_dir_all(&path).unwrap();
            Self(path)
        }

        fn path(&self, name: &str) -> PathBuf {
            self.0.join(name)
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    async fn serve_once(
        status: &'static str,
        body: Vec<u8>,
    ) -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            let headers = format!(
                "HTTP/1.1 {status}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len(),
            );
            stream.write_all(headers.as_bytes()).await.unwrap();
            stream.write_all(&body).await.unwrap();
        });
        (format!("http://{address}/xray.zip"), task)
    }

    #[cfg(unix)]
    fn executable_fixture(path: &Path, output: &str, exit_code: i32) {
        use std::os::unix::fs::PermissionsExt;

        let mut script = format!("#!/bin/sh\nprintf '%s\\n' '{output}'\nexit {exit_code}\n");
        script.push('#');
        script.push_str(&"x".repeat(1_000_100 - script.len()));
        std::fs::write(path, script).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    fn valid_fixture_bytes() -> Vec<u8> {
        let dir = TestDir::new();
        let path = dir.path("xray-fixture");
        executable_fixture(&path, "Xray test fixture", 0);
        std::fs::read(path).unwrap()
    }

    #[cfg(unix)]
    fn long_running_fixture(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let mut script = "#!/bin/sh\nexec /usr/bin/tail -f /dev/null\n".to_string();
        script.push('#');
        script.push_str(&"x".repeat(1_000_100 - script.len()));
        std::fs::write(path, script).unwrap();
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    fn zip_with_core(name: &str, core: &[u8]) -> Vec<u8> {
        use zip::write::SimpleFileOptions;

        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        writer
            .start_file(
                name,
                SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored),
            )
            .unwrap();
        writer.write_all(core).unwrap();
        writer.finish().unwrap().into_inner()
    }

    #[test]
    fn formats_duration() {
        assert_eq!(fmt_hms(0.0), "00:00:00");
        assert_eq!(fmt_hms(3661.0), "01:01:01");
        assert_eq!(fmt_hms(59.9), "00:00:59");
    }

    #[test]
    fn formats_negative_duration_as_zero() {
        assert_eq!(fmt_hms(-1.0), "00:00:00");
    }

    #[test]
    fn progress_line_has_counts_and_eta() {
        let line = progress_line(50, 100, 10.0, 3);
        assert!(line.starts_with("[ 50/100]"), "{line}");
        assert!(line.contains("прошло 00:00:10"), "{line}");
        assert!(line.contains("осталось ~00:00:10"), "{line}");
        assert!(line.ends_with("good 3"), "{line}");
    }

    #[test]
    fn progress_line_start_has_unknown_eta() {
        let line = progress_line(0, 100, 0.0, 0);
        assert!(line.starts_with("[  0/100]"), "{line}");
        assert!(line.contains("осталось ~--:--:--"), "{line}");
    }

    #[test]
    fn progress_line_done_has_zero_eta() {
        let line = progress_line(100, 100, 42.0, 7);
        assert!(line.contains("осталось 00:00:00"), "{line}");
    }

    fn sample_proxy() -> Proxy {
        Proxy {
            proto: "vless",
            host: "proxy.example".into(),
            port: 443,
            name: "sample".into(),
            base_url: "vless://sample".into(),
            orig_url: "vless://sample".into(),
            settings: json!({"address":"proxy.example","port":443,"id":"uuid"}),
            stream: Some(json!({"network":"ws","security":"tls"})),
            country: "DE".into(),
            display: "[DE] - 001".into(),
            ping: None,
            speed_kbs: 0.0,
        }
    }

    fn test_checker(core: PathBuf, work: PathBuf, start_timeout: Duration) -> Arc<Checker> {
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        Arc::new(Checker {
            hysteria1_core: PathBuf::new(),
            no_hysteria1_download: true,
            hysteria1_core_ready: tokio::sync::OnceCell::new(),
            core,
            work,
            speed_threads: 1,
            start_timeout,
            base_port: port,
            next: AtomicU16::new(port),
            bad_re: Regex::new(
                r#"(?i)failed to build outbound config with tag\s*[:=]?\s*["']?(out_\d+)"#,
            )
            .unwrap(),
            bad_outbounds: AtomicUsize::new(0),
            bad_outbound_rejections: Mutex::new(HashMap::new()),
            speed_unchecked: Mutex::new(HashSet::new()),
            speed_measurements: Mutex::new(HashMap::new()),
            speed_probe_results: Mutex::new(HashMap::new()),
        })
    }

    fn speed_opts() -> SpeedOpts {
        SpeedOpts {
            primary: None,
            connect_timeout_s: 1.0,
            timeout_s: 1.0,
            max_mb: 1.0,
            min_kb: 0.0,
            speed_probes: 1,
        }
    }

    #[tokio::test]
    async fn alloc_port_skips_a_port_held_by_another_process() {
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();
        let dir = TestDir::new();
        let checker = test_checker(
            dir.path("missing-xray"),
            dir.path("work"),
            Duration::from_secs(1),
        );
        checker.next.store(occupied_port, Ordering::Relaxed);

        let allocated = checker.alloc_port_with(|_| async { false }).await;

        assert_ne!(allocated, occupied_port);
        assert!(std::net::TcpListener::bind(("127.0.0.1", allocated)).is_ok());
    }

    #[test]
    fn core_name_matches_the_host_platform() {
        assert_eq!(
            core_bin_name(),
            if cfg!(windows) { "xray.exe" } else { "xray" }
        );
        assert_eq!(core_bin_name_for("windows"), "xray.exe");
        assert_eq!(core_bin_name_for("linux"), "xray");
        assert_eq!(core_bin_name_for("macos"), "xray");
    }

    #[test]
    fn core_name_for_platform_is_exact_for_windows_and_unix() {
        assert_eq!(core_bin_name_for("windows"), "xray.exe");
        for os in ["linux", "macos", "freebsd", "unknown"] {
            assert_eq!(core_bin_name_for(os), "xray", "unexpected name for {os}");
        }
    }

    #[test]
    fn default_download_url_matches_the_platform_archive() {
        let url = default_xray_url().unwrap();
        assert!(url.starts_with("https://github.com/XTLS/Xray-core/releases/latest/download/"));
        assert!(url.ends_with(asset_name().unwrap()));
    }

    #[test]
    fn maps_supported_platform_assets() {
        let cases = [
            (("windows", "x86_64"), "Xray-windows-64.zip"),
            (("windows", "aarch64"), "Xray-windows-arm64-v8a.zip"),
            (("linux", "x86_64"), "Xray-linux-64.zip"),
            (("linux", "aarch64"), "Xray-linux-arm64-v8a.zip"),
            (("macos", "x86_64"), "Xray-macos-64.zip"),
            (("macos", "aarch64"), "Xray-macos-arm64-v8a.zip"),
        ];

        for ((os, arch), expected) in cases {
            assert_eq!(asset_name_for(os, arch).unwrap(), expected);
        }
    }

    #[test]
    fn current_platform_download_url_names_the_exact_supported_asset() {
        let expected = asset_name_for(std::env::consts::OS, std::env::consts::ARCH).unwrap();
        let url = default_xray_url().unwrap();
        assert!(url.ends_with(expected), "unexpected Xray asset URL: {url}");
    }

    #[test]
    fn rejects_unsupported_platform_assets_with_platform_name() {
        let error = asset_name_for("freebsd", "x86_64").unwrap_err();
        assert!(error.to_string().contains("freebsd/x86_64"));
    }

    #[test]
    fn valid_core_rejects_missing_and_small_files() {
        let dir = TestDir::new();
        assert!(!valid_core(&dir.path("missing-xray")));

        let small = dir.path("small-xray");
        std::fs::write(&small, vec![0; 1024]).unwrap();
        assert!(!valid_core(&small));
    }

    #[cfg(unix)]
    #[test]
    fn valid_core_rejects_file_at_the_exact_minimum_size() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let path = dir.path("exactly-minimum-xray");
        let mut script = "#!/bin/sh\necho Xray version fixture\n".to_string();
        script.push_str(&"#".repeat(1_000_000 - script.len()));
        std::fs::write(&path, script).unwrap();
        let mut permissions = std::fs::metadata(&path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&path, permissions).unwrap();

        assert_eq!(std::fs::metadata(&path).unwrap().len(), 1_000_000);
        assert!(!valid_core(&path));
    }

    #[tokio::test]
    async fn port_open_probe_handles_open_closed_and_timed_out_checks() {
        assert!(port_open_with(443, |_| async { true }).await);
        assert!(!port_open_with(443, |_| async { false }).await);
        assert!(!port_open_with(443, |_| async { std::future::pending::<bool>().await }).await);

        // Port zero is reserved and cannot accept a TCP connection.
        assert!(!port_open(0).await);
    }

    #[test]
    fn startup_deadline_is_reached_at_equality_only_after_the_boundary() {
        let now = Instant::now();
        let deadline = now + Duration::from_secs(1);
        assert!(!startup_deadline_reached(
            deadline - Duration::from_nanos(1),
            deadline
        ));
        assert!(startup_deadline_reached(deadline, deadline));
        assert!(startup_deadline_reached(
            deadline + Duration::from_nanos(1),
            deadline
        ));
    }

    #[test]
    fn speed_minimum_accepts_exact_threshold_and_one_epsilon_below() {
        assert!(speed_meets_minimum(1.0, 1.0));
        assert!(speed_meets_minimum(1.0 - f64::EPSILON, 1.0));
        assert!(!speed_meets_minimum(1.0 - 2.0 * f64::EPSILON, 1.0));
    }

    #[test]
    fn xray_speed_eligibility_excludes_only_unsupported_hysteria_variants() {
        let mut hysteria1 = sample_proxy();
        hysteria1.proto = "hysteria";
        assert!(!supported_by_xray_speed_core(&hysteria1));

        let mut hysteria2 = sample_proxy();
        hysteria2.proto = "hysteria2";
        hysteria2.settings["obfs"] = json!("salamander");
        assert!(supported_by_xray_speed_core(&hysteria2));
        hysteria2.settings["obfs"] = json!("gecko");
        assert!(!supported_by_xray_speed_core(&hysteria2));

        let mut old_shadowsocks = sample_proxy();
        old_shadowsocks.proto = "shadowsocks";
        old_shadowsocks.settings["method"] = json!("aes-128-cfb");
        assert!(!supported_by_xray_speed_core(&old_shadowsocks));
        old_shadowsocks.settings["method"] = json!("aes-256-gcm");
        assert!(supported_by_xray_speed_core(&old_shadowsocks));

        let vmess = sample_proxy();
        assert!(supported_by_xray_speed_core(&vmess));
    }

    #[test]
    fn xray_speed_eligibility_handles_missing_and_non_string_shadowsocks_methods() {
        let mut shadowsocks = sample_proxy();
        shadowsocks.proto = "shadowsocks";
        shadowsocks.settings = json!({});
        assert!(!supported_by_xray_speed_core(&shadowsocks));

        shadowsocks.settings["method"] = json!(42);
        assert!(!supported_by_xray_speed_core(&shadowsocks));

        shadowsocks.settings["method"] = json!("chacha20-poly1305");
        assert!(supported_by_xray_speed_core(&shadowsocks));

        let mut unknown = sample_proxy();
        unknown.proto = "future-protocol";
        assert!(supported_by_xray_speed_core(&unknown));
    }

    #[test]
    fn build_config_preserves_supported_proxy_protocols_and_their_settings() {
        let dir = TestDir::new();
        let checker = test_checker(
            dir.path("unused-xray"),
            dir.path("work"),
            Duration::from_secs(1),
        );
        let cases = [
            ("vless", "vless", json!({"id":"vless-id"})),
            ("vmess", "vmess", json!({"id":"vmess-id"})),
            (
                "shadowsocks",
                "shadowsocks",
                json!({"method":"aes-256-gcm","password":"ss-secret"}),
            ),
            ("trojan", "trojan", json!({"password":"trojan-secret"})),
            (
                "hysteria2",
                "hysteria",
                json!({"version":2,"address":"hy.example","port":443}),
            ),
        ];
        let mapping: Vec<_> = cases
            .iter()
            .enumerate()
            .map(|(index, (proto, _, settings))| {
                let mut proxy = sample_proxy();
                proxy.proto = proto;
                proxy.settings = settings.clone();
                proxy.stream = Some(json!({"network":"test-transport"}));
                (proxy, 13000 + index as u16)
            })
            .collect();

        let config = checker.build_config(&mapping);

        assert_eq!(config["inbounds"].as_array().unwrap().len(), cases.len());
        assert_eq!(config["outbounds"].as_array().unwrap().len(), cases.len());
        assert_eq!(
            config["routing"]["rules"].as_array().unwrap().len(),
            cases.len()
        );
        for (index, (_, expected_proto, expected_settings)) in cases.iter().enumerate() {
            let outbound = &config["outbounds"][index];
            assert_eq!(outbound["protocol"], *expected_proto);
            assert_eq!(outbound["settings"], *expected_settings);
            assert_eq!(outbound["streamSettings"]["network"], "test-transport");
            assert_eq!(outbound["tag"], format!("out_{}", 13000 + index));
            assert_eq!(
                config["routing"]["rules"][index]["outboundTag"],
                format!("out_{}", 13000 + index)
            );
        }
    }

    #[cfg(unix)]
    #[test]
    fn valid_core_checks_executable_status_and_xray_marker() {
        let dir = TestDir::new();

        let valid = dir.path("valid-xray");
        executable_fixture(&valid, "Xray version fixture", 0);
        assert!(valid_core(&valid));

        let missing_marker = dir.path("missing-marker");
        executable_fixture(&missing_marker, "unrelated program", 0);
        assert!(!valid_core(&missing_marker));

        let failed = dir.path("failed-xray");
        executable_fixture(&failed, "Xray version fixture", 1);
        assert!(!valid_core(&failed));

        let not_executable = dir.path("not-executable");
        std::fs::write(&not_executable, vec![b'x'; 1_000_100]).unwrap();
        assert!(!valid_core(&not_executable));
    }

    #[tokio::test]
    async fn ensure_core_rejects_missing_core_when_download_is_disabled() {
        let dir = TestDir::new();
        let error = ensure_core(&dir.path("missing-xray"), None, true)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("--no-xray-download"));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_core_reuses_a_valid_core_without_network_access() {
        let dir = TestDir::new();
        let path = dir.path("xray");
        executable_fixture(&path, "Xray version fixture", 0);

        ensure_core(&path, Some("http://127.0.0.1:1/unreachable"), false)
            .await
            .unwrap();
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_core_downloads_zip_into_missing_parent_and_validates_binary() {
        let dir = TestDir::new();
        let path = dir.path("nested/xray");
        let archive = zip_with_core("nested/xray", &valid_fixture_bytes());
        let (url, server) = serve_once("200 OK", archive).await;

        ensure_core(&path, Some(&url), false).await.unwrap();
        server.await.unwrap();

        assert!(std::fs::metadata(path).unwrap().len() > 1_000_000);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_core_downloads_to_a_relative_path_without_a_parent() {
        struct RemoveOnDrop(PathBuf);
        impl Drop for RemoveOnDrop {
            fn drop(&mut self) {
                let _ = std::fs::remove_file(&self.0);
            }
        }

        let name = format!("proxy-rs-xray-relative-{}", std::process::id());
        let path = PathBuf::from(name);
        let _cleanup = RemoveOnDrop(path.clone());
        let archive = zip_with_core("xray", &valid_fixture_bytes());
        let (url, server) = serve_once("200 OK", archive).await;

        ensure_core(&path, Some(&url), false).await.unwrap();
        server.await.unwrap();

        assert!(std::fs::metadata(&path).unwrap().len() > 1_000_000);
    }

    #[tokio::test]
    async fn ensure_core_rejects_small_and_invalid_zip_downloads() {
        let dir = TestDir::new();
        let too_small = dir.path("small/xray");
        let (url, server) = serve_once("200 OK", b"small response".to_vec()).await;
        let error = ensure_core(&too_small, Some(&url), false)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert!(error.to_string().contains("маленький архив"));
        assert!(!too_small.exists());

        let invalid_zip = dir.path("invalid/xray");
        let (url, server) = serve_once("200 OK", vec![b'Z'; 1_000_001]).await;
        assert!(ensure_core(&invalid_zip, Some(&url), false).await.is_err());
        server.await.unwrap();
        assert!(!invalid_zip.exists());

        let missing_entry = dir.path("missing-entry/xray");
        let archive = zip_with_core("other-binary", &vec![b'x'; 1_000_100]);
        let (url, server) = serve_once("200 OK", archive).await;
        let error = ensure_core(&missing_entry, Some(&url), false)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert!(error.to_string().contains("в архиве не найден"));
        assert!(!missing_entry.exists());
    }

    #[tokio::test]
    async fn ensure_core_checks_downloaded_archive_size_at_the_exact_boundary() {
        let dir = TestDir::new();

        let under_boundary = dir.path("under-boundary/xray");
        let (url, server) = serve_once("200 OK", vec![b'Z'; 999_999]).await;
        let error = ensure_core(&under_boundary, Some(&url), false)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert!(error.to_string().contains("маленький архив"));

        let at_boundary = dir.path("at-boundary/xray");
        let (url, server) = serve_once("200 OK", vec![b'Z'; 1_000_000]).await;
        let error = ensure_core(&at_boundary, Some(&url), false)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert!(!error.to_string().contains("маленький архив"));
    }

    #[tokio::test]
    async fn ensure_core_rejects_small_binary_inside_large_archive() {
        use zip::write::SimpleFileOptions;

        let dir = TestDir::new();
        let path = dir.path("small-binary/xray");
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file(core_bin_name(), options).unwrap();
        writer.write_all(b"too small").unwrap();
        writer.start_file("padding", options).unwrap();
        writer.write_all(&vec![b'x'; 1_000_100]).unwrap();
        let archive = writer.finish().unwrap().into_inner();
        let (url, server) = serve_once("200 OK", archive).await;

        let error = ensure_core(&path, Some(&url), false).await.unwrap_err();
        server.await.unwrap();

        assert!(error
            .to_string()
            .contains("xray в архиве подозрительно маленький"));
        assert!(!path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn ensure_core_preserves_existing_file_when_downloaded_binary_is_invalid() {
        let dir = TestDir::new();
        let path = dir.path("xray");
        std::fs::write(&path, b"previous cache").unwrap();
        let invalid_core = vec![b'x'; 1_000_100];
        let archive = zip_with_core("xray", &invalid_core);
        let (url, server) = serve_once("200 OK", archive).await;

        let error = ensure_core(&path, Some(&url), false).await.unwrap_err();
        server.await.unwrap();

        assert!(error.to_string().contains("скачанное ядро не запускается"));
        assert_eq!(std::fs::read(path).unwrap(), b"previous cache");
    }

    #[tokio::test]
    async fn ensure_core_reports_http_and_parent_creation_errors() {
        let dir = TestDir::new();
        let (url, server) = serve_once("404 Not Found", b"not found".to_vec()).await;
        let error = ensure_core(&dir.path("http-error/xray"), Some(&url), false)
            .await
            .unwrap_err();
        server.await.unwrap();
        assert!(error
            .to_string()
            .contains("ошибка ответа при скачивании xray"));

        let parent = dir.path("file-parent");
        std::fs::write(&parent, b"file").unwrap();
        let error = ensure_core(&parent.join("xray"), Some("http://127.0.0.1:1/no"), false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("не удалось создать каталог"));

        // No explicit URL: exercise default URL selection before the failing
        // parent creation, without making a request to GitHub.
        let error = ensure_core(&parent.join("default-url/xray"), None, false)
            .await
            .unwrap_err();
        assert!(error.to_string().contains("не удалось создать каталог"));

        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let unused_port = listener.local_addr().unwrap().port();
        drop(listener);
        let error = ensure_core(
            &dir.path("connection-error/xray"),
            Some(&format!("http://127.0.0.1:{unused_port}/no")),
            false,
        )
        .await
        .unwrap_err();
        assert!(error.to_string().contains("не удалось скачать"));
    }

    #[tokio::test]
    async fn ensure_core_reports_truncated_http_body() {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.unwrap();
            stream
                .write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 32\r\nConnection: close\r\n\r\npartial",
                )
                .await
                .unwrap();
        });

        let dir = TestDir::new();
        let path = dir.path("truncated/xray");
        let error = ensure_core(&path, Some(&format!("http://{address}/xray.zip")), false)
            .await
            .unwrap_err();
        server.await.unwrap();

        assert!(error.to_string().contains("тело ответа xray"), "{error:#}");
        assert!(!path.exists());
    }

    #[tokio::test]
    async fn checker_run_stage_cleans_config_when_core_cannot_be_spawned() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let checker = test_checker(
            dir.path("missing-xray"),
            work.clone(),
            Duration::from_secs(1),
        );

        let result = checker
            .run_stage(vec![sample_proxy()], speed_opts(), ProgressBar::hidden())
            .await;

        assert!(result.is_empty());
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[tokio::test]
    async fn checker_run_stage_skips_launch_when_config_cannot_be_written() {
        let dir = TestDir::new();
        let checker = test_checker(
            dir.path("missing-xray"),
            dir.path("missing-work-directory"),
            Duration::from_secs(1),
        );

        let result = checker
            .run_stage(vec![sample_proxy()], speed_opts(), ProgressBar::hidden())
            .await;

        assert!(result.is_empty());
        assert!(!dir.path("missing-work-directory").exists());
    }

    #[tokio::test]
    async fn checker_run_stage_handles_config_serialization_failure() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let checker = test_checker(
            dir.path("missing-xray"),
            work.clone(),
            Duration::from_secs(1),
        );
        let result = checker
            .run_stage_with_serializer(
                vec![sample_proxy()],
                speed_opts(),
                ProgressBar::hidden(),
                |_| {
                    Err(serde_json::Error::io(std::io::Error::other(
                        "fixture serialization failure",
                    )))
                },
            )
            .await;

        assert!(result.is_empty());
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checker_wait_error_kills_child_and_cleans_batch_config() {
        let dir = TestDir::new();
        let cfg_path = dir.path("batch.json");
        std::fs::write(&cfg_path, b"fixture").unwrap();
        let child = tokio::process::Command::new("sleep")
            .arg("30")
            .spawn()
            .unwrap();
        let pb = ProgressBar::hidden();
        pb.set_length(1);

        let result = Checker::handle_wait_error(
            std::io::Error::other("fixture wait failure"),
            child,
            cfg_path.clone(),
            1,
            pb.clone(),
        )
        .await;

        assert!(result.is_empty());
        assert_eq!(pb.position(), 1);
        assert!(!cfg_path.exists());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checker_run_stage_handles_core_exiting_before_socks_listener_starts() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let core = dir.path("exits-before-listen");
        executable_fixture(&core, "Xray startup fixture", 1);
        let checker = test_checker(core, work.clone(), Duration::from_secs(1));

        let result = checker
            .run_stage(vec![sample_proxy()], speed_opts(), ProgressBar::hidden())
            .await;

        assert!(result.is_empty());
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn checker_run_stage_kills_core_when_startup_times_out() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let core = dir.path("never-listens");
        long_running_fixture(&core);
        let checker = test_checker(core, work.clone(), Duration::from_millis(100));

        let result = checker
            .run_stage(vec![sample_proxy()], speed_opts(), ProgressBar::hidden())
            .await;

        assert!(result.is_empty());
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn cancelling_run_stage_kills_core_and_removes_config() {
        use std::os::unix::fs::PermissionsExt;

        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let pid_file = dir.path("core.pid");
        let core = dir.path("long-running-core");
        let script = format!(
            "#!/bin/sh\necho $$ > '{}'\nexec /usr/bin/tail -f /dev/null\n",
            pid_file.display()
        );
        std::fs::write(&core, script).unwrap();
        let mut permissions = std::fs::metadata(&core).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(&core, permissions).unwrap();

        let checker = test_checker(core, work.clone(), Duration::from_secs(30));
        let task = tokio::spawn(checker.run_stage(
            vec![sample_proxy()],
            speed_opts(),
            ProgressBar::hidden(),
        ));
        tokio::time::timeout(Duration::from_secs(3), async {
            while !pid_file.exists() {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("mock Xray process started");
        let pid = std::fs::read_to_string(&pid_file)
            .unwrap()
            .trim()
            .parse::<u32>()
            .unwrap();
        task.abort();
        let _ = task.await;

        assert_eq!(std::fs::read_dir(&work).unwrap().count(), 0);
        tokio::time::timeout(Duration::from_secs(3), async {
            loop {
                let alive = std::process::Command::new("kill")
                    .arg("-0")
                    .arg(pid.to_string())
                    .output()
                    .map(|output| output.status.success())
                    .unwrap_or(false);
                if !alive {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(20)).await;
            }
        })
        .await
        .expect("dropped run_stage terminates its child process");
    }

    #[tokio::test]
    async fn dropping_reporter_guard_aborts_its_task() {
        let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
        struct DropFlag(Arc<std::sync::atomic::AtomicBool>);
        impl Drop for DropFlag {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }

        let flag = DropFlag(Arc::clone(&dropped));
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let task = tokio::spawn(async move {
            let _flag = flag;
            let _ = started_tx.send(());
            std::future::pending::<()>().await;
        });
        started_rx.await.unwrap();
        let guard = ReporterTask(Some(task));
        drop(guard);
        tokio::task::yield_now().await;
        assert!(dropped.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn checker_discards_reported_bad_outbound_and_counts_it() {
        let dir = TestDir::new();
        let checker = test_checker(
            dir.path("missing-xray"),
            dir.path("work"),
            Duration::from_secs(1),
        );
        let mut proxy = sample_proxy();
        proxy.name = "bad outbound".into();
        let rejected_key = proxy.key();
        let port = checker.base_port;

        let result = Checker::handle_failure(
            &checker,
            &format!("failed to build outbound config with tag: out_{port}: unknown field streamSettings.badField"),
            vec![(proxy, port)],
            speed_opts(),
            ProgressBar::hidden(),
        )
        .await;

        assert!(result.is_empty());
        assert_eq!(checker.bad_outbounds(), 1);
        assert_eq!(
            checker.bad_outbound_rejections().get(&rejected_key),
            Some(&"unknown_config_field")
        );
    }

    #[test]
    fn bad_outbound_error_classification_uses_only_fixed_safe_labels() {
        for (log, expected) in [
            ("unknown field: secret token", "unknown_config_field"),
            ("unknown protocol: private.example", "unknown_protocol"),
            ("unsupported transport", "unsupported_feature"),
            ("missing required field", "missing_config_value"),
            ("invalid configuration", "invalid_config_value"),
            ("private.example:443", "config_rejected"),
