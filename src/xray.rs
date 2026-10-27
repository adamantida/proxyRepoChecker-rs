use crate::cli::Cli;
use crate::parse::Proxy;
use crate::speed;
use anyhow::{bail, Context, Result};
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use serde_json::{json, Value};
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
        .unwrap_or_else(|_| ProgressStyle::default_bar()),
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
}

pub struct Checker {
    core: PathBuf,
    work: PathBuf,
    speed_threads: usize,
    start_timeout: Duration,
    base_port: u16,
    next: AtomicU16,
    bad_re: Regex,
    bad_outbounds: AtomicUsize,
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
    matches!(
        tokio::time::timeout(
            Duration::from_millis(250),
            tokio::net::TcpStream::connect(("127.0.0.1", port))
        )
        .await,
        Ok(Ok(_))
    )
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
    pub fn new(cli: &Cli) -> Result<Arc<Self>> {
        let core = PathBuf::from(&cli.core);
        if !core.exists() {
            anyhow::bail!("не найдено ядро: {}", core.display());
        }
        let work = PathBuf::from(&cli.workdir);
        std::fs::create_dir_all(&work)
            .with_context(|| format!("не удалось создать {}", work.display()))?;
        Ok(Arc::new(Self {
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
        }))
    }

    /// Сколько битых outbound было убрано в фазе скорости.
    pub fn bad_outbounds(&self) -> usize {
        self.bad_outbounds.load(Ordering::Relaxed)
    }

    async fn alloc_port(&self) -> u16 {
        self.alloc_port_with(port_open).await
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
                "protocol": p.proto,
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
                    return Vec::new();
                }
            };
            if let Err(e) = std::fs::write(&cfg_path, bytes) {
                eprintln!("[ошибка] запись {}: {e}", cfg_path.display());
                pb.inc(total_items);
                return Vec::new();
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
                    return Vec::new();
                }
            };

            let first_port = mapping[0].1;
            let deadline = Instant::now() + this.start_timeout;
            let mut started = false;
            loop {
                match child.try_wait() {
                    Ok(Some(_)) => {
                        let text = read_output(child).await;
                        return Checker::handle_failure(&this, &text, mapping, opts, pb.clone())
                            .await;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        return Checker::handle_wait_error(e, child, cfg_path, total_items, pb)
                            .await;
                    }
                }
                if port_open(first_port).await {
                    started = true;
                    break;
                }
                if Instant::now() >= deadline {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(100)).await;
            }

            if !started {
                let _ = child.kill().await;
                let text = read_output(child).await;
                return Checker::handle_failure(&this, &text, mapping, opts, pb.clone()).await;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;

            let concurrency = this.speed_threads;
            let results: Vec<Proxy> = stream::iter(mapping)
                .map(|(p, port)| {
                    let opts = opts.clone();
                    let pb = pb.clone();
                    async move {
                        let kbs = speed::measure(
                            port,
                            opts.primary.as_deref(),
                            opts.connect_timeout_s,
                            opts.timeout_s,
                            opts.max_mb,
                        )
                        .await;
                        pb.inc(1);
                        if kbs + f64::EPSILON >= opts.min_kb {
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
            results
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
        })
    }

    fn speed_opts() -> SpeedOpts {
        SpeedOpts {
            primary: None,
            connect_timeout_s: 1.0,
            timeout_s: 1.0,
            max_mb: 1.0,
            min_kb: 0.0,
        }
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
    async fn ensure_core_rejects_small_binary_inside_large_archive() {
        use zip::write::SimpleFileOptions;

        let dir = TestDir::new();
        let path = dir.path("small-binary/xray");
        let cursor = Cursor::new(Vec::new());
        let mut writer = zip::ZipWriter::new(cursor);
        let options =
            SimpleFileOptions::default().compression_method(zip::CompressionMethod::Stored);
        writer.start_file("xray", options).unwrap();
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
        let task = tokio::spawn(async move {
            let _flag = flag;
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
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
        let port = checker.base_port;

        let result = Checker::handle_failure(
            &checker,
            &format!("failed to build outbound config with tag: out_{port}"),
            vec![(proxy, port)],
            speed_opts(),
            ProgressBar::hidden(),
        )
        .await;

        assert!(result.is_empty());
        assert_eq!(checker.bad_outbounds(), 1);
    }

    #[tokio::test]
    async fn run_all_processes_nonempty_batch_and_returns_failed_proxies_as_empty() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let checker = test_checker(dir.path("missing-xray"), work, Duration::from_secs(1));

        let result = checker
            .run_all(vec![sample_proxy()], 1, 1, speed_opts(), "test", None)
            .await
            .unwrap();

        assert!(result.is_empty());
    }

    #[test]
    fn checker_new_validates_core_and_work_directory() {
        let dir = TestDir::new();
        let missing_core = dir.path("missing-xray");
        let missing_core_cli = Cli::try_parse_from([
            "proxy-rs",
            "--core",
            missing_core.to_str().unwrap(),
            "--workdir",
            dir.path("work").to_str().unwrap(),
        ])
        .unwrap();
        assert!(Checker::new(&missing_core_cli)
            .err()
            .unwrap()
            .to_string()
            .contains("не найдено ядро"));

        let core = dir.path("xray");
        std::fs::write(&core, b"fixture").unwrap();
        let work = dir.path("work");
        let cli = Cli::try_parse_from([
            "proxy-rs",
            "--core",
            core.to_str().unwrap(),
            "--workdir",
            work.to_str().unwrap(),
        ])
        .unwrap();
        let checker = Checker::new(&cli).unwrap();
        assert_eq!(checker.core, core);
        assert!(work.is_dir());

        let blocked_parent = dir.path("blocked-parent");
        std::fs::write(&blocked_parent, b"file").unwrap();
        let blocked_cli = Cli::try_parse_from([
            "proxy-rs",
            "--core",
            core.to_str().unwrap(),
            "--workdir",
            blocked_parent.join("work").to_str().unwrap(),
        ])
        .unwrap();
        assert!(Checker::new(&blocked_cli)
            .err()
            .unwrap()
            .to_string()
            .contains("не удалось создать"));
    }

    #[tokio::test]
    async fn alloc_port_skips_a_port_already_bound_by_another_process() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let occupied = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let occupied_port = occupied.local_addr().unwrap().port();
        let checker = test_checker(dir.path("missing-xray"), work, Duration::from_secs(1));
        checker.next.store(occupied_port, Ordering::Relaxed);

        let allocated = checker.alloc_port().await;

        assert_ne!(allocated, occupied_port);
        assert!(std::net::TcpListener::bind(("127.0.0.1", allocated)).is_ok());
    }

    #[tokio::test]
    async fn alloc_port_wraps_to_the_base_port_at_the_upper_boundary() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let checker = test_checker(dir.path("missing-xray"), work, Duration::from_secs(1));
        checker.next.store(65_000, Ordering::Relaxed);

        let allocated = checker.alloc_port().await;

        assert!(allocated >= 65_000 || allocated == checker.base_port);
    }

    #[tokio::test]
    async fn alloc_port_retries_when_probe_finds_a_listener_after_bind_check() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let checker = test_checker(dir.path("missing-xray"), work, Duration::from_secs(1));
        let calls = AtomicUsize::new(0);

        let allocated = checker
            .alloc_port_with(|_| {
                let attempt = calls.fetch_add(1, Ordering::Relaxed);
                async move { attempt == 0 }
            })
            .await;

        assert_eq!(calls.load(Ordering::Relaxed), 2);
        assert_ne!(allocated, checker.base_port);
    }

    #[test]
    fn speed_sort_is_descending_and_treats_nan_as_equal() {
        let mut slow = sample_proxy();
        slow.speed_kbs = 1.0;
        let mut nan = sample_proxy();
        nan.speed_kbs = f64::NAN;
        let mut fast = sample_proxy();
        fast.speed_kbs = 5.0;
        assert_eq!(
            compare_speed_descending(&fast, &slow),
            std::cmp::Ordering::Less
        );
        assert_eq!(
            compare_speed_descending(&fast, &nan),
            std::cmp::Ordering::Equal
        );
        let mut proxies = vec![slow, fast, nan];

        sort_by_speed(&mut proxies);

        assert_eq!(proxies.len(), 3);
        assert!(proxies.iter().any(|proxy| proxy.speed_kbs.is_nan()));
    }

    #[tokio::test]
    async fn progress_reporter_skips_tty_and_stops_after_all_items_finish() {
        let good = Arc::new(Mutex::new(Vec::new()));
        assert!(spawn_progress_reporter(
            ProgressBar::hidden(),
            Arc::clone(&good),
            Instant::now(),
            true,
            Duration::from_millis(1),
        )
        .is_none());

        let pb = ProgressBar::hidden();
        pb.set_length(1);
        pb.inc(1);
        spawn_progress_reporter(pb, good, Instant::now(), false, Duration::from_millis(1))
            .unwrap()
            .await
            .unwrap();
    }

    #[test]
    fn read_output_formats_wait_errors() {
        let message = format_read_output(Err(std::io::Error::other("fixture wait failure")));
        assert!(message.contains("чтение вывода ядра"));
        assert!(message.contains("fixture wait failure"));
    }

    #[tokio::test]
    async fn bad_outbound_failure_falls_back_when_tag_is_not_in_mapping() {
        let dir = TestDir::new();
        let checker = test_checker(
            dir.path("missing-xray"),
            dir.path("work"),
            Duration::from_secs(1),
        );
        let unmatched_port = if checker.base_port == u16::MAX {
            checker.base_port - 1
        } else {
            checker.base_port + 1
        };

        let result = Checker::handle_failure(
            &checker,
            &format!("failed to build outbound config with tag: out_{unmatched_port}"),
            vec![(sample_proxy(), checker.base_port)],
            speed_opts(),
            ProgressBar::hidden(),
        )
        .await;

        assert!(result.is_empty());
        assert_eq!(checker.bad_outbounds(), 0);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_all_splits_a_batch_when_the_core_exits_before_startup() {
        let dir = TestDir::new();
        let work = dir.path("work");
        std::fs::create_dir(&work).unwrap();
        let core = dir.path("exits-before-startup");
        executable_fixture(&core, "Xray startup fixture", 1);
        let checker = test_checker(core, work.clone(), Duration::from_secs(1));

        let result = checker
            .run_all(
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
            work: PathBuf::new(),
            speed_threads: 1,
            start_timeout: Duration::from_secs(1),
            base_port: 12000,
            next: AtomicU16::new(12000),
            bad_re: Regex::new("out_\\d+").unwrap(),
            bad_outbounds: AtomicUsize::new(0),
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
    fn build_config_omits_stream_settings_when_proxy_has_none() {
        let checker = Checker {
            core: PathBuf::new(),
            work: PathBuf::new(),
            speed_threads: 1,
            start_timeout: Duration::from_secs(1),
            base_port: 12000,
            next: AtomicU16::new(12000),
            bad_re: Regex::new("out_\\d+").unwrap(),
            bad_outbounds: AtomicUsize::new(0),
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
            work: PathBuf::new(),
            speed_threads: 1,
            start_timeout: Duration::from_secs(1),
            base_port: 12000,
            next: AtomicU16::new(12000),
            bad_re: Regex::new("out_\\d+").unwrap(),
            bad_outbounds: AtomicUsize::new(0),
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
                },
                "test",
                None,
            )
            .await
            .unwrap();
        assert!(got.is_empty());
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_all_keeps_successful_proxy_and_writes_incremental_good_file() {
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
        assert_eq!(std::fs::read_dir(work).unwrap().count(), 0);
    }
}
