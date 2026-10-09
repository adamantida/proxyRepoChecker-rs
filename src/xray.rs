use crate::cli::Cli;
use crate::parse::Proxy;
use crate::speed;
use anyhow::{bail, Context, Result};
use futures::future::BoxFuture;
use futures::stream::{self, StreamExt};
use indicatif::{ProgressBar, ProgressStyle};
use regex::Regex;
use serde_json::{json, Value};
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::process::Stdio;
use std::sync::atomic::{AtomicU16, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

const CREATE_NO_WINDOW: u32 = 0x0800_0000;
const UA: &str = "Mozilla/5.0 (proxy-rs)";

pub fn core_bin_name() -> &'static str {
    if cfg!(windows) {
        "xray.exe"
    } else {
        "xray"
    }
}

fn asset_name() -> Result<&'static str> {
    use std::env::consts::{ARCH, OS};
    Ok(match (OS, ARCH) {
        ("windows", "x86_64") => "Xray-windows-64.zip",
        ("windows", "aarch64") => "Xray-windows-arm64-v8a.zip",
        ("linux", "x86_64") => "Xray-linux-64.zip",
        ("linux", "aarch64") => "Xray-linux-arm64-v8a.zip",
        ("macos", "x86_64") => "Xray-macos-64.zip",
        ("macos", "aarch64") => "Xray-macos-arm64-v8a.zip",
        _ => bail!("неподдерживаемая платформа для автозагрузки xray: {OS}/{ARCH}"),
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
            std::fs::create_dir_all(parent).ok();
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
        bail!("скачан подозрительно маленький архив xray ({} байт)", bytes.len());
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
            entry.read_to_end(&mut buf).context("чтение xray из архива")?;
            found = Some(buf);
            break;
        }
    }
    let bin = found.ok_or_else(|| anyhow::anyhow!("в архиве не найден {want}"))?;
    if bin.len() < 1_000_000 {
        bail!("xray в архиве подозрительно маленький ({} байт)", bin.len());
    }

    let tmp = path.with_extension("tmp");
    std::fs::write(&tmp, &bin).with_context(|| format!("не удалось записать {}", tmp.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = std::fs::metadata(&tmp) {
            let mut perms = meta.permissions();
            perms.set_mode(0o755);
            let _ = std::fs::set_permissions(&tmp, perms);
        }
    }
    std::fs::rename(&tmp, path).with_context(|| format!("не удалось заменить {}", path.display()))?;

    if !valid_core(path) {
        bail!("скачанное ядро не запускается: {}", path.display());
    }
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
    match child.wait_with_output().await {
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
        }))
    }

    async fn alloc_port(&self) -> u16 {
        loop {
            let cur = self.next.load(Ordering::Relaxed);
            let next = if cur >= 65_000 { self.base_port } else { cur + 1 };
            if self
                .next
                .compare_exchange(cur, next, Ordering::Relaxed, Ordering::Relaxed)
                .is_ok()
            {
                // пропускаем занятые и зарезервированные системой порты
                if std::net::TcpListener::bind(("127.0.0.1", cur)).is_err() {
                    continue;
                }
                if port_open(cur).await {
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
    ) -> BoxFuture<'static, Vec<Proxy>> {
        let this = Arc::clone(self);
        let label = label.to_string();
        Box::pin(async move {
            if proxies.is_empty() {
                return Vec::new();
            }
            let chunks: Vec<Vec<Proxy>> = proxies
                .chunks(batch.max(1))
                .map(|c| c.to_vec())
                .collect();
            let pb = ProgressBar::new(chunks.len() as u64).with_style(
                ProgressStyle::with_template(&format!(
                    "{{spinner:.green}} {label} {{pos}}/{{len}} батчей [{{elapsed_precise}}<{{eta_precise}}]"
                ))
                .unwrap_or_else(|_| ProgressStyle::default_bar()),
            );
            let all_good: Arc<Mutex<Vec<Proxy>>> = Arc::new(Mutex::new(Vec::new()));
            let nested: Vec<Vec<Proxy>> = stream::iter(chunks)
                .map(|chunk| {
                    let this = Arc::clone(&this);
                    let pb = pb.clone();
                    let opts = opts.clone();
                    let all_good = Arc::clone(&all_good);
                    let good_path = good_path.clone();
                    async move {
                        let res = this.run_stage(chunk, opts).await;
                        pb.inc(1);
                        if let Some(path) = &good_path {
                            if !res.is_empty() {
                                let mut g = all_good.lock().unwrap();
                                g.extend(res.iter().cloned());
                                let mut sorted = g.clone();
                                sorted.sort_by(|a, b| {
                                    b.speed_kbs
                                        .partial_cmp(&a.speed_kbs)
                                        .unwrap_or(std::cmp::Ordering::Equal)
                                });
                                let lines: Vec<String> =
                                    sorted.iter().map(|p| crate::output::line(p, true)).collect();
                                let _ = crate::output::write_lines_atomic(path, &lines);
                            }
                        }
                        res
                    }
                })
                .buffer_unordered(workers.max(1))
                .collect()
                .await;
            pb.finish_and_clear();
            nested.into_iter().flatten().collect()
        })
    }

    fn run_stage(
        self: &Arc<Self>,
        items: Vec<Proxy>,
        opts: SpeedOpts,
    ) -> BoxFuture<'static, Vec<Proxy>> {
        let this = Arc::clone(self);
        Box::pin(async move {
            if items.is_empty() {
                return Vec::new();
            }
            let mut mapping: Vec<(Proxy, u16)> = Vec::with_capacity(items.len());
            for p in items {
                let port = this.alloc_port().await;
                mapping.push((p, port));
            }

            let config = this.build_config(&mapping);
            let cfg_path = this.work.join(format!("batch_{}.json", mapping[0].1));
            let bytes = match serde_json::to_vec(&config) {
                Ok(b) => b,
                Err(e) => {
                    eprintln!("[ошибка] сериализация batch-конфига: {e}");
                    return Vec::new();
                }
            };
            if let Err(e) = std::fs::write(&cfg_path, bytes) {
                eprintln!("[ошибка] запись {}: {e}", cfg_path.display());
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
                    let _ = std::fs::remove_file(&cfg_path);
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
                        let _ = std::fs::remove_file(&cfg_path);
                        return Checker::handle_failure(&this, &text, mapping, opts).await;
                    }
                    Ok(None) => {}
                    Err(e) => {
                        eprintln!("[ошибка] try_wait: {e}");
                        let _ = child.kill().await;
                        let _ = std::fs::remove_file(&cfg_path);
                        return Vec::new();
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
                let _ = std::fs::remove_file(&cfg_path);
                return Checker::handle_failure(&this, &text, mapping, opts).await;
            }

            tokio::time::sleep(Duration::from_millis(500)).await;

            let concurrency = this.speed_threads;
            let results: Vec<Proxy> = stream::iter(mapping)
                .map(|(p, port)| {
                    let opts = opts.clone();
                    async move {
                        let kbs = speed::measure(
                            port,
                            opts.primary.as_deref(),
                            opts.connect_timeout_s,
                            opts.timeout_s,
                            opts.max_mb,
                        )
                        .await;
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
            let _ = std::fs::remove_file(&cfg_path);
            results
        })
    }

    async fn handle_failure(
        this: &Arc<Self>,
        log: &str,
        mapping: Vec<(Proxy, u16)>,
        opts: SpeedOpts,
    ) -> Vec<Proxy> {
        if let Some(caps) = this.bad_re.captures(log) {
            let tag = caps[1].to_string();
            if let Ok(port) = tag.trim_start_matches("out_").parse::<u16>() {
                if let Some(pos) = mapping.iter().position(|(_, p)| *p == port) {
                    println!("   [батч] убран битый outbound {tag}");
                    let mut rest = mapping;
                    rest.remove(pos);
                    let items = rest.into_iter().map(|(p, _)| p).collect();
                    return this.run_stage(items, opts).await;
                }
            }
        }
        if mapping.len() > 1 {
            let mid = mapping.len() / 2;
            let left: Vec<Proxy> = mapping[..mid].iter().map(|(p, _)| p.clone()).collect();
            let right: Vec<Proxy> = mapping[mid..].iter().map(|(p, _)| p.clone()).collect();
            let (a, b) = tokio::join!(
                this.run_stage(left, opts.clone()),
                this.run_stage(right, opts)
            );
            let mut out = a;
            out.extend(b);
            out
        } else {
            Vec::new()
        }
    }
}
