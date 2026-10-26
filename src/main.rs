mod check;
mod cli;
mod fetch;
mod geoip;
mod outbound;
mod output;
mod parse;
mod sources;
mod speed;
mod tcp_ping;
mod xray;

use clap::Parser;
use cli::Cli;
use colored::Colorize;
use parse::Proxy;
use rand::seq::SliceRandom;
use std::collections::HashSet;
use std::path::Path;
use std::path::PathBuf;
use std::time::Instant;
use xray::SpeedOpts;

fn default_geoip_path() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(dir.join("geoip.dat"))
}

fn default_core_path() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(dir.join(xray::core_bin_name()))
}

/// Ищет файл: сначала по пути, потом рядом с exe, потом в bin/ проекта.
fn resolve_default(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    resolve_default_from(
        &p,
        std::env::current_exe().ok().as_deref(),
        Path::new("bin"),
    )
}

fn resolve_default_from(path: &Path, exe: Option<&Path>, bin_dir: &Path) -> PathBuf {
    let p = path.to_path_buf();
    if p.exists() {
        return p;
    }
    if let Some(dir) = exe.and_then(Path::parent) {
        let cand = dir.join(&p);
        if cand.exists() {
            return cand;
        }
    }
    let bin = bin_dir.join(&p);
    if bin.exists() {
        return bin;
    }
    p
}

fn fmt_dur(secs: f64) -> String {
    if secs < 60.0 {
        format!("{secs:.1}s")
    } else {
        let m = (secs / 60.0).floor() as u64;
        let s = secs - m as f64 * 60.0;
        format!("{m}m {s:.0}s")
    }
}

fn fmt_speed(kbs: f64) -> String {
    if kbs >= 1024.0 {
        format!("{:.1} MB/s", kbs / 1024.0)
    } else {
        format!("{kbs:.1} KB/s")
    }
}

fn percentage(part: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        part as f64 / total as f64 * 100.0
    }
}

fn retain_within_max_ping(candidates: &mut Vec<Proxy>, max_ping: u64) -> Vec<String> {
    if max_ping == 0 {
        return Vec::new();
    }

    let mut rejected = Vec::new();
    candidates.retain(|proxy| {
        if proxy.ping.unwrap_or(u64::MAX) <= max_ping {
            true
        } else {
            rejected.push(proxy.base_url.clone());
            false
        }
    });
    rejected
}

#[derive(Debug, Default)]
struct Summary {
    fetch_sources: usize,
    fetch_loaded: usize,
    fetch_failed: usize,
    fetch_failure_examples: Vec<String>,
    fetch_secs: f64,
    links: usize,
    unique: usize,
    duplicates: usize,
    invalid: usize,
    unsupported: usize,
    parse_secs: f64,
    geo: bool,
    geo_countries: usize,
    geo_secs: f64,
    tcp_enabled: bool,
    tcp_total: usize,
    tcp_alive: usize,
    tcp_secs: f64,
    speed_enabled: bool,
    speed_total: usize,
    speed_passed: usize,
    speed_secs: f64,
    bad_outbounds: usize,
    alive: usize,
    good: usize,
    avg_kbs: f64,
    median_kbs: f64,
    alive_file: Option<(String, usize)>,
    good_file: Option<(String, usize)>,
    clash_file: Option<(String, usize)>,
    dead_file: Option<(String, usize)>,
    raw_file: Option<(String, usize)>,
    parsed_file: Option<(String, usize)>,
    total_secs: f64,
}

fn print_summary(s: &Summary) {
    let line = "═".repeat(46);
    println!("\n{}", line.bold().cyan());
    println!("{}", "  ИТОГИ".bold().cyan());
    println!("{}", line.bold().cyan());

    println!(
        "  {} {}",
        "Общее время:".bold(),
        fmt_dur(s.total_secs).yellow()
    );

    println!("  {}", "Источники".bold().underline());
    println!(
        "    подписок: {}   загружено: {}   ({})",
        s.fetch_sources,
        s.fetch_loaded,
        fmt_dur(s.fetch_secs)
    );
    if s.fetch_failed > 0 {
        println!("    ошибок загрузки: {}", s.fetch_failed);
        for failure in &s.fetch_failure_examples {
            println!("      {failure}");
        }
    }
    println!(
        "    ссылок: {}   уникальных: {}   ({})",
        s.links,
        s.unique.to_string().green(),
        fmt_dur(s.parse_secs)
    );
    let mut bad = format!("дублей: {}", s.duplicates);
    if s.invalid > 0 {
        bad.push_str(&format!("   некорр.: {}", s.invalid));
    }
    if s.unsupported > 0 {
        bad.push_str(&format!("   неподдерж.: {}", s.unsupported));
    }
    println!("    {bad}");

    if s.geo {
        println!("  {}", "GeoIP".bold().underline());
        println!("    стран: {}   ({})", s.geo_countries, fmt_dur(s.geo_secs));
    }

    if s.tcp_enabled {
        println!("  {}", "TCP ping".bold().underline());
        let pct = percentage(s.tcp_alive, s.tcp_total);
        println!(
            "    живых: {}/{} ({pct:.1}%)   ({})",
            s.tcp_alive.to_string().green(),
            s.tcp_total,
            fmt_dur(s.tcp_secs)
        );
    }

    if s.speed_enabled {
        println!("  {}", "Скорость (xray)".bold().underline());
        let pct = percentage(s.speed_passed, s.speed_total);
        println!(
            "    прошли порог: {}/{} ({pct:.1}%)   ({})",
            s.speed_passed.to_string().green(),
            s.speed_total,
            fmt_dur(s.speed_secs)
        );
        if s.good > 0 {
            println!(
                "    средняя: {}   медиана: {}",
                fmt_speed(s.avg_kbs).cyan(),
                fmt_speed(s.median_kbs).cyan()
            );
        }
        if s.bad_outbounds > 0 {
            println!("    битых outbound: {}", s.bad_outbounds);
        }
    }

    println!(
        "  {} {}",
        "Живых:".bold(),
        s.alive.to_string().green().bold()
    );

    let files: Vec<String> = [
        s.raw_file.as_ref(),
        s.parsed_file.as_ref(),
        s.alive_file.as_ref(),
        s.good_file.as_ref(),
        s.clash_file.as_ref(),
        s.dead_file.as_ref(),
    ]
    .into_iter()
    .flatten()
    .map(|(p, n)| format!("{p} ({n})"))
    .collect();
    if !files.is_empty() {
        println!("  {} {}", "Файлы:".bold(), files.join(", "));
    }
    println!("{}", line.bold().cyan());
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();
    if let Err(message) = cli.validate() {
        use clap::{error::ErrorKind, CommandFactory};
        Cli::command()
            .error(ErrorKind::ValueValidation, message)
            .exit();
    }
    let summary = run(cli).await?;
    print_summary(&summary);
    Ok(())
}

async fn run(mut cli: Cli) -> anyhow::Result<Summary> {
    cli.sources = resolve_default(&cli.sources).display().to_string();
    cli.core = resolve_default(&cli.core).display().to_string();
    let started = Instant::now();

    macro_rules! info {
        ($($arg:tt)*) => {
            println!(">> [+{:.0}s] {}", started.elapsed().as_secs_f64(), format!($($arg)*))
        };
    }

    let mut sum = Summary::default();

    // ------------------------------------------------------------------
    // 1. Источники
    // ------------------------------------------------------------------
    let src = sources::collect(&cli)?;
    sum.fetch_sources = src.fetch_urls.len();
    info!("Источников подписок: {}", src.fetch_urls.len());
    if src.fetch_urls.is_empty() && src.direct_links.is_empty() {
        anyhow::bail!("нет источников для загрузки");
    }

    // ------------------------------------------------------------------
    // 2. Загрузка
    // ------------------------------------------------------------------
    let t = Instant::now();
    let report = fetch::fetch_all(&src.fetch_urls, cli.fetch_concurrency, cli.fetch_timeout).await;
    let mut texts = report.texts;
    sum.fetch_loaded = texts.len();
    sum.fetch_failed = report.failures.len();
    sum.fetch_failure_examples = report
        .failures
        .iter()
        .take(5)
        .map(|failure| format!("{} — {}", failure.url, failure.reason))
        .collect();
    sum.fetch_secs = t.elapsed().as_secs_f64();
    info!(
        "Загружено подписок: {}, ошибок: {}",
        texts.len(),
        sum.fetch_failed
    );
    for example in &sum.fetch_failure_examples {
        info!("Ошибка источника: {example}");
    }
    if texts.is_empty() && !src.fetch_urls.is_empty() && src.direct_links.is_empty() {
        anyhow::bail!(
            "не удалось загрузить ни одного источника (ошибок: {})",
            sum.fetch_failed
        );
    }
    if !texts.is_empty() {
        output::write_lines_atomic(&cli.raw_output, &texts)?;
        sum.raw_file = Some((cli.raw_output.clone(), texts.len()));
        info!("Сохранено: {} ({} подписок)", cli.raw_output, texts.len());
    }

    // ------------------------------------------------------------------
    // 3. Парсинг + дедуп
    // ------------------------------------------------------------------
    info!("Парсинг ссылок и дедуп...");
    let t = Instant::now();
    let mut links: Vec<String> = Vec::new();
    for t in &texts {
        links.extend(parse::extract_links(t));
    }
    texts.clear();
    links.extend(src.direct_links);

    let (mut proxies, stats) = parse::parse_all(links);
    sum.parse_secs = t.elapsed().as_secs_f64();
    sum.links = stats.links;
    sum.unique = stats.parsed;
    sum.duplicates = stats.duplicates;
    sum.invalid = stats.invalid;
    sum.unsupported = stats.unsupported.values().sum();
    info!(
        "Ссылок: {}, уникальных прокси: {}, дублей: {}, некорректных: {}",
        stats.links, stats.parsed, stats.duplicates, stats.invalid
    );
    if !stats.unsupported.is_empty() {
        let mut list: Vec<(&&'static str, &usize)> = stats.unsupported.iter().collect();
        list.sort_by(|a, b| b.1.cmp(a.1));
        let txt: Vec<String> = list.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        info!("Неподдерживаемые протоколы: {}", txt.join(", "));
    }
    if !stats.invalid_reasons.is_empty() {
        let mut list: Vec<(&&'static str, &usize)> = stats.invalid_reasons.iter().collect();
        list.sort_by(|a, b| b.1.cmp(a.1));
        let txt: Vec<String> = list.iter().map(|(k, v)| format!("{k}: {v}")).collect();
        info!("Причины некорректных: {}", txt.join(", "));
    }
    if proxies.is_empty() {
        anyhow::bail!("не найдено ни одного подходящего прокси");
    }

    if cli.shuffle {
        let mut rng = rand::thread_rng();
        proxies.shuffle(&mut rng);
    }
    if let Some(n) = cli.limit {
        if proxies.len() > n {
            proxies.truncate(n);
            info!("Обрезано до --limit {n}");
        }
    }

    output::write_lines_atomic(
        &cli.parsed_output,
        &proxies
            .iter()
            .map(|p| output::line(p, false))
            .collect::<Vec<_>>(),
    )?;
    sum.parsed_file = Some((cli.parsed_output.clone(), proxies.len()));
    info!(
        "Сохранено: {} ({} прокси до проверок)",
        cli.parsed_output,
        proxies.len()
    );

    // ------------------------------------------------------------------
    // 4. GeoIP: страна + переименование в [XX] - NNN
    // ------------------------------------------------------------------
    let rename = !cli.no_geoip;
    sum.geo = rename;
    if rename {
        let t = Instant::now();
        let db_path = match &cli.geoip_db {
            Some(p) => PathBuf::from(p),
            None => default_geoip_path()?,
        };
        geoip::ensure_db(&db_path, &cli.geoip_url).await?;
        let db = geoip::GeoDb::open(&db_path)?;
        let (nc, n4, n6) = db.stats();
        sum.geo_countries = nc;
        info!("GeoIP: база: {nc} стран, {n4} IPv4-сетей, {n6} IPv6-сетей");
        let counts = geoip::assign_countries(&mut proxies, &db).await?;
        geoip::print_country_summary(&counts);
        sum.geo_secs = t.elapsed().as_secs_f64();
    } else {
        for p in proxies.iter_mut() {
            p.display = p.name.clone();
        }
    }

    output::write_lines_atomic(
        &cli.parsed_output,
        &proxies
            .iter()
            .map(|p| output::line(p, rename))
            .collect::<Vec<_>>(),
    )?;
    info!(
        "Обновлено: {} (имена {})",
        cli.parsed_output,
        if rename { "GeoIP" } else { "без GeoIP" }
    );

    let mut dead_urls: Vec<String> = Vec::new();

    // ------------------------------------------------------------------
    // 5. Фаза 1: TCP ping — живые / мёртвые
    // ------------------------------------------------------------------
    let mut candidates = proxies;
    sum.tcp_enabled = !cli.no_tcp_ping;
    if !cli.no_tcp_ping {
        info!(
            "Фаза 1 (TCP ping): {} адресов, timeout {}s, конкурентность {}, попытки {}{}",
            candidates.len(),
            cli.tcp_timeout,
            cli.tcp_concurrency,
            cli.tcp_retries,
            if cli.tcp_max_ms > 0 {
                format!(", max {} ms", cli.tcp_max_ms)
            } else {
                String::new()
            }
        );
        let t = Instant::now();
        let (alive, dead, st) = tcp_ping::filter(
            candidates,
            cli.tcp_timeout,
            cli.tcp_concurrency,
            cli.tcp_retries,
            cli.tcp_max_ms,
        )
        .await;
        sum.tcp_secs = t.elapsed().as_secs_f64();
        sum.tcp_total = st.total;
        sum.tcp_alive = st.alive;
        info!(
            "Фаза 1: живых {}/{} (макс TCP RTT {} мс), мёртвых {}",
            st.alive, st.total, st.max_ms_seen, st.dead
        );
        dead_urls.extend(dead.iter().map(|p| p.base_url.clone()));
        candidates = alive;
    }

    if candidates.is_empty() {
        info!("Нет живых прокси после TCP ping.");
        if let Some(path) = &cli.dead {
            output::write_lines_atomic(path, &dead_urls)?;
            sum.dead_file = Some((path.clone(), dead_urls.len()));
        }
        sum.total_secs = started.elapsed().as_secs_f64();
        return Ok(sum);
    }

    // --max-ping отсекает по TCP RTT (из фазы 1)
    if cli.max_ping > 0 {
        let before = candidates.len();
        dead_urls.extend(retain_within_max_ping(&mut candidates, cli.max_ping));
        if candidates.len() < before {
            info!(
                "--max-ping {}: отброшено {} с более высоким TCP ping",
                cli.max_ping,
                before - candidates.len()
            );
        }
    }

    // Сохраняем результат TCP-этапа сразу
    if !cli.no_tcp_ping {
        let mut tcp_alive = candidates.clone();
        tcp_alive.sort_by_key(|p| p.ping.unwrap_or(u64::MAX));
        output::write_lines_atomic(
            &cli.output,
            &tcp_alive
                .iter()
                .map(|p| output::line(p, rename))
                .collect::<Vec<_>>(),
        )?;
        sum.alive_file = Some((cli.output.clone(), tcp_alive.len()));
        if let Some(path) = &cli.dead {
            output::write_lines_atomic(path, &dead_urls)?;
            sum.dead_file = Some((path.clone(), dead_urls.len()));
        }
        info!(
            "Сохранено после TCP: {} ({} живых){}",
            cli.output,
            tcp_alive.len(),
            match &cli.dead {
                Some(p) => format!(", {p} ({} мёртвых)", dead_urls.len()),
                None => String::new(),
            }
        );
    }

    let mut alive: Vec<Proxy>;
    let mut good: Vec<Proxy> = Vec::new();

    // ------------------------------------------------------------------
    // 6. Скорость через xray — единственный тест живости
    // ------------------------------------------------------------------
    sum.speed_enabled = !cli.no_speed;
    if cli.no_speed {
        candidates.sort_by_key(|p| p.ping.unwrap_or(u64::MAX));
        alive = candidates;
        info!(
            "Скорость выключена (--no-speed): живых по TCP ping {}",
            alive.len()
        );
    } else {
        let core_path = if PathBuf::from(&cli.core).exists() {
            PathBuf::from(&cli.core)
        } else {
            default_core_path()?
        };
        xray::ensure_core(&core_path, cli.xray_url.as_deref(), cli.no_xray_download).await?;
        cli.core = core_path.display().to_string();

        let checker = xray::Checker::new(&cli)?;
        info!(
            "Фаза 2 (скорость через xray): {} прокси, батчи {}, потоков {}, до {:.1} MB, мин {:.1} KB/s",
            candidates.len(),
            cli.batch,
            cli.speed_threads,
            cli.speed_max_mb,
            cli.min_kb
        );
        let opts = SpeedOpts {
            primary: cli.speed_url.clone(),
            connect_timeout_s: cli.speed_connect_timeout,
            timeout_s: cli.speed_timeout,
            max_mb: cli.speed_max_mb,
            min_kb: cli.min_kb,
        };
        sum.speed_total = candidates.len();
        let t = Instant::now();
        let passed = checker
            .run_all(
                candidates.clone(),
                cli.batch,
                cli.batch_workers,
                opts,
                "замер скорости",
                Some(cli.good_output.clone()),
            )
            .await?;
        sum.speed_secs = t.elapsed().as_secs_f64();
        sum.speed_passed = passed.len();
        sum.bad_outbounds = checker.bad_outbounds();

        let passed_keys: HashSet<String> = passed.iter().map(|p| p.key()).collect();
        for p in &candidates {
            if !passed_keys.contains(&p.key()) {
                dead_urls.push(p.base_url.clone());
            }
        }

        info!(
            "Фаза 2: прошли порог {}/{} (отсеяно мусора: {})",
            passed.len(),
            candidates.len(),
            candidates.len() - passed.len()
        );

        good = passed.clone();
        good.sort_by(|a, b| {
            b.speed_kbs
                .partial_cmp(&a.speed_kbs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        output::write_lines_atomic(
            &cli.good_output,
            &good
                .iter()
                .map(|p| output::line(p, rename))
                .collect::<Vec<_>>(),
        )?;
        sum.good_file = Some((cli.good_output.clone(), good.len()));
        println!("   Топ по скорости:");
        output::print_top(&good, true);

        alive = passed;
        alive.sort_by_key(|p| p.ping.unwrap_or(u64::MAX));
    }

    output::write_lines_atomic(
        &cli.output,
        &alive
            .iter()
            .map(|p| output::line(p, rename))
            .collect::<Vec<_>>(),
    )?;
    sum.alive_file = Some((cli.output.clone(), alive.len()));
    println!("   Живые (сортировка по ping):");
    output::print_top(&alive, !cli.no_speed);

    // ------------------------------------------------------------------
    // 7. Мёртвые + итог
    // ------------------------------------------------------------------
    if let Some(path) = &cli.dead {
        output::write_lines_atomic(path, &dead_urls)?;
        sum.dead_file = Some((path.clone(), dead_urls.len()));
    }

    sum.alive = alive.len();
    sum.good = good.len();
    if !good.is_empty() {
        let speeds: Vec<f64> = good.iter().map(|p| p.speed_kbs).collect();
        sum.avg_kbs = speeds.iter().sum::<f64>() / speeds.len() as f64;
        let mut sorted = speeds.clone();
        sorted.sort_by(|a, b| a.partial_cmp(b).unwrap_or(std::cmp::Ordering::Equal));
        let mid = sorted.len() / 2;
        sum.median_kbs = if sorted.len().is_multiple_of(2) {
            (sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            sorted[mid]
        };
    }

    // ------------------------------------------------------------------
    // 8. Clash/mihomo конфиг из good-прокси
    // ------------------------------------------------------------------
    if let Some(path) = &cli.clash {
        if good.is_empty() {
            info!("Clash: пропуск — нет прокси, прошедших порог скорости");
        } else {
            let sources: Vec<proxy_clash::Source> = good
                .iter()
                .map(|p| proxy_clash::Source {
                    proto: p.proto.to_string(),
                    name: if p.display.is_empty() {
                        p.name.clone()
                    } else {
                        p.display.clone()
                    },
                    server: p.host.clone(),
                    port: p.port,
                    country: p.country.clone(),
                    settings: p.settings.clone(),
                    stream: p.stream.clone(),
                })
                .collect();
            let opts = proxy_clash::Options {
                include_country: !cli.clash_no_country,
                include_categories: !cli.clash_no_categories,
            };
            let (text, report) = proxy_clash::render(&sources, &opts)?;
            output::write_text_atomic(path, &text)?;
            if report.skipped > 0 {
                info!(
                    "Clash: пропущено неподдерживаемых протоколов: {}",
                    report.skipped
                );
            }
            info!(
                ">> Сохранено: {} ({} прокси, групп {}, стран {})",
                path, report.proxies, report.groups, report.countries
            );
            sum.clash_file = Some((path.clone(), report.proxies));
        }
    }

    sum.total_secs = started.elapsed().as_secs_f64();

    info!(
        "Готово за {:.1}s | живых: {} | good: {}",
        sum.total_secs,
        alive.len(),
        good.len()
    );
    Ok(sum)
}

#[cfg(test)]
mod tests {
    use super::{
        default_core_path, default_geoip_path, resolve_default, resolve_default_from, xray,
    };
    use super::{
        fmt_dur, fmt_speed, percentage, print_summary, retain_within_max_ping, run, Summary,
    };
    use crate::parse;
    use clap::Parser;
    use std::fs;
    use std::path::{Path, PathBuf};
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

    struct TempDir(PathBuf);

    impl TempDir {
        fn new() -> Self {
            let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
            let path = std::env::temp_dir()
                .join(format!("proxy-rs-main-tests-{}-{id}", std::process::id()));
            fs::create_dir(&path).expect("create temporary test directory");
            Self(path)
        }

        fn path(&self) -> &Path {
            &self.0
        }

        fn write(&self, name: &str, contents: &str) -> PathBuf {
            let path = self.0.join(name);
            fs::write(&path, contents).expect("write test fixture");
            path
        }

        fn cli(&self, file: Option<&Path>, extra: &[&str]) -> crate::cli::Cli {
            let sources = self.0.join("missing-sources.json");
            let mut args = vec![
                "proxy-rs".to_string(),
                "--sources".to_string(),
                sources.display().to_string(),
                "--output".to_string(),
                self.0.join("alive.txt").display().to_string(),
                "--raw-output".to_string(),
                self.0.join("raw.txt").display().to_string(),
                "--parsed-output".to_string(),
                self.0.join("parsed.txt").display().to_string(),
                "--good-output".to_string(),
                self.0.join("good.txt").display().to_string(),
                "--no-geoip".to_string(),
                "--no-tcp-ping".to_string(),
                "--no-speed".to_string(),
            ];
            if let Some(file) = file {
                args.extend(["--file".to_string(), file.display().to_string()]);
            }
            args.extend(extra.iter().map(|arg| (*arg).to_string()));
            crate::cli::Cli::try_parse_from(args).expect("parse test CLI arguments")
        }
    }

    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn duration_switches_from_seconds_to_minutes() {
        assert_eq!(fmt_dur(59.0), "59.0s");
        assert_eq!(fmt_dur(60.0), "1m 0s");
        assert_eq!(fmt_dur(125.0), "2m 5s");
    }

    #[test]
    fn speed_switches_from_kilobytes_to_megabytes() {
        assert_eq!(fmt_speed(0.0), "0.0 KB/s");
        assert_eq!(fmt_speed(1024.0), "1.0 MB/s");
    }

    #[test]
    fn percentage_handles_zero_and_regular_totals() {
        assert_eq!(percentage(1, 0), 0.0);
        assert_eq!(percentage(1, 4), 25.0);
        assert_eq!(percentage(4, 4), 100.0);
    }

    #[test]
    fn max_ping_keeps_inclusive_and_unknown_pings_only_when_unlimited() {
        let links = vec![
            "vless://01234567-89ab-cdef-0123-456789abcdef@fast.example:443#fast".to_string(),
            "vless://01234567-89ab-cdef-0123-456789abcdef@slow.example:443#slow".to_string(),
            "vless://01234567-89ab-cdef-0123-456789abcdef@unknown.example:443#unknown".to_string(),
        ];
        let (mut proxies, _) = parse::parse_all(links);
        proxies[0].ping = Some(25);
        proxies[1].ping = Some(26);
        proxies[2].ping = None;

        let rejected = retain_within_max_ping(&mut proxies, 25);

        assert_eq!(proxies.len(), 1);
        assert_eq!(proxies[0].host, "fast.example");
        assert_eq!(
            rejected,
            [
                "vless://01234567-89ab-cdef-0123-456789abcdef@slow.example:443",
                "vless://01234567-89ab-cdef-0123-456789abcdef@unknown.example:443",
            ]
        );

        let mut unlimited = parse::parse_all(vec![
            "vless://01234567-89ab-cdef-0123-456789abcdef@unknown.example:443#unknown".to_string(),
        ])
        .0;
        assert!(retain_within_max_ping(&mut unlimited, 0).is_empty());
        assert_eq!(unlimited.len(), 1);
    }

    #[test]
    fn default_asset_paths_are_next_to_the_running_executable() {
        let exe = std::env::current_exe().unwrap();
        let exe_dir = exe.parent().unwrap();

        assert_eq!(default_geoip_path().unwrap(), exe_dir.join("geoip.dat"));
        assert_eq!(
            default_core_path().unwrap(),
            exe_dir.join(xray::core_bin_name())
        );
    }

    #[test]
    fn resolve_default_prefers_existing_path_and_falls_back_when_missing() {
        let temp = TempDir::new();
        let present = temp.write("present.dat", "fixture");

        assert_eq!(resolve_default(present.to_str().unwrap()), present);
        let missing = temp.path().join("missing.dat");
        assert_eq!(resolve_default(missing.to_str().unwrap()), missing);
    }

    #[test]
    fn resolve_default_finds_assets_beside_the_test_executable() {
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        let name = format!(
            "proxy-rs-main-path-test-{}-{}",
            std::process::id(),
            NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed)
        );
        let sibling = exe_dir.join(&name);
        fs::write(&sibling, "fixture").unwrap();

        let resolved = resolve_default(&name);

        let _ = fs::remove_file(&sibling);
        assert_eq!(resolved, sibling);
    }

    #[test]
    fn resolve_default_searches_executable_then_bin_then_original_path() {
        let temp = TempDir::new();
        let executable_dir = temp.path().join("exe");
        let bin_dir = temp.path().join("bin");
        fs::create_dir(&executable_dir).unwrap();
        fs::create_dir(&bin_dir).unwrap();
        let name = "asset.dat";
        let original = PathBuf::from(name);
        let sibling = executable_dir.join(name);
        let bundled = bin_dir.join(name);
        fs::write(&sibling, "executable asset").unwrap();
        fs::write(&bundled, "bundled asset").unwrap();

        assert_eq!(
            resolve_default_from(&original, Some(&executable_dir.join("proxy-rs")), &bin_dir),
            sibling
        );

        fs::remove_file(&sibling).unwrap();
        assert_eq!(resolve_default_from(&original, None, &bin_dir), bundled);

        fs::remove_file(&bundled).unwrap();
        assert_eq!(resolve_default_from(&original, None, &bin_dir), original);
    }

    #[tokio::test]
    async fn run_processes_direct_link_file_without_geoip_tcp_or_xray() {
        let temp = TempDir::new();
        let links = temp.write(
            "links.txt",
            "# local fixture\nvless://01234567-89ab-cdef-0123-456789abcdef@proxy.example:443#test-node\n",
        );

        let summary = run(temp.cli(Some(&links), &[])).await.unwrap();

        assert_eq!(summary.fetch_sources, 0);
        assert_eq!(summary.fetch_loaded, 0);
        assert_eq!(summary.links, 1);
        assert_eq!(summary.unique, 1);
        assert_eq!(summary.alive, 1);
        assert_eq!(summary.good, 0);
        assert!(!summary.geo);
        assert!(!summary.tcp_enabled);
        assert!(!summary.speed_enabled);
        assert_eq!(
            fs::read_to_string(temp.path().join("parsed.txt")).unwrap(),
            fs::read_to_string(temp.path().join("alive.txt")).unwrap()
        );
        assert!(fs::read_to_string(temp.path().join("alive.txt"))
            .unwrap()
            .contains("proxy.example"));
    }

    #[tokio::test]
    async fn run_reports_parse_statistics_and_applies_shuffle_and_limit() {
        let temp = TempDir::new();
        let links = temp.write(
            "links.txt",
            concat!(
                "vless://01234567-89ab-cdef-0123-456789abcdef@first.example:443#first\n",
                "vless://01234567-89ab-cdef-0123-456789abcdef@second.example:8443#second\n",
                "vless://01234567-89ab-cdef-0123-456789abcdef@first.example:443#duplicate\n",
                "ssr://unsupported\n",
                "vless://bad@broken.example:443#invalid\n"
            ),
        );

        let summary = run(temp.cli(Some(&links), &["--shuffle", "--limit", "1"]))
            .await
            .unwrap();

        assert_eq!(summary.links, 5);
        assert_eq!(summary.unique, 2);
        assert_eq!(summary.duplicates, 1);
        assert_eq!(summary.unsupported, 1);
        assert_eq!(summary.invalid, 1);
        assert_eq!(summary.alive, 1);
        let parsed = fs::read_to_string(temp.path().join("parsed.txt")).unwrap();
        assert_eq!(parsed.lines().count(), 1);
    }

    #[tokio::test]
    async fn run_keeps_a_proxy_reachable_on_a_local_tcp_listener() {
        let temp = TempDir::new();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        let links = temp.write(
            "links.txt",
            &format!(
                "vless://01234567-89ab-cdef-0123-456789abcdef@127.0.0.1:{port}#local-listener\n"
            ),
        );
        let mut cli = temp.cli(Some(&links), &[]);
        cli.no_tcp_ping = false;
        cli.tcp_timeout = 1.0;
        cli.tcp_retries = 1;
        cli.tcp_concurrency = 1;
        cli.dead = Some(temp.path().join("dead.txt").display().to_string());

        let summary = run(cli).await.unwrap();

        assert_eq!(summary.tcp_total, 1);
        assert_eq!(summary.tcp_alive, 1);
        assert_eq!(summary.alive, 1);
        assert_eq!(summary.dead_file.as_ref().unwrap().1, 0);
        assert!(fs::read_to_string(temp.path().join("alive.txt"))
            .unwrap()
            .contains("127.0.0.1"));
        assert_eq!(
            fs::read_to_string(temp.path().join("dead.txt")).unwrap(),
            ""
        );
        drop(listener);
    }

    #[tokio::test]
    async fn run_returns_summary_when_local_tcp_prefilter_finds_no_alive_proxy() {
        let temp = TempDir::new();
        let listener = std::net::TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let port = listener.local_addr().unwrap().port();
        drop(listener);
        let links = temp.write(
            "links.txt",
            &format!(
                "vless://01234567-89ab-cdef-0123-456789abcdef@127.0.0.1:{port}#closed-local-port\n"
            ),
        );
        let mut cli = temp.cli(Some(&links), &[]);
        cli.no_tcp_ping = false;
        cli.tcp_timeout = 0.2;
        cli.tcp_retries = 1;
        cli.tcp_concurrency = 1;
        cli.dead = Some(temp.path().join("dead.txt").display().to_string());

        let summary = run(cli).await.unwrap();

        assert_eq!(summary.tcp_total, 1);
        assert_eq!(summary.tcp_alive, 0);
        assert_eq!(summary.alive, 0);
        assert!(summary.dead_file.is_some());
        assert!(fs::read_to_string(temp.path().join("dead.txt"))
            .unwrap()
            .contains("127.0.0.1"));
    }

    #[tokio::test]
    async fn run_stops_when_selected_sources_and_direct_links_are_empty() {
        let temp = TempDir::new();
        let sources = temp.write("sources.json", r#"{"3":[]}"#);
        let mut cli = temp.cli(None, &["--set", "3"]);
        cli.sources = sources.display().to_string();

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("нет источников для загрузки"));
    }

    #[tokio::test]
    async fn run_returns_source_file_errors_before_pipeline_starts() {
        let temp = TempDir::new();
        let malformed = temp.write("sources.json", "{");
        let mut cli = temp.cli(None, &[]);
        cli.sources = malformed.display().to_string();

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("битый JSON"));
    }

    #[tokio::test]
    async fn run_rejects_files_without_supported_proxy_links() {
        let temp = TempDir::new();
        let links = temp.write("links.txt", "not a proxy link\nssr://unsupported\n");

        let error = run(temp.cli(Some(&links), &[])).await.unwrap_err();

        assert!(error
            .to_string()
            .contains("не найдено ни одного подходящего прокси"));
    }

    #[tokio::test]
    async fn run_propagates_output_write_errors() {
        let temp = TempDir::new();
        let links = temp.write(
            "links.txt",
            "vless://01234567-89ab-cdef-0123-456789abcdef@proxy.example:443#test-node\n",
        );
        let mut cli = temp.cli(Some(&links), &[]);
        cli.parsed_output = temp
            .path()
            .join("missing-directory/parsed.txt")
            .display()
            .to_string();

        let error = run(cli).await.unwrap_err();

        assert!(error.to_string().contains("не удалось создать"));
    }

    #[test]
    fn print_summary_covers_enabled_and_populated_sections() {
        let summary = Summary {
            fetch_sources: 2,
            fetch_loaded: 1,
            fetch_failed: 1,
            fetch_failure_examples: vec!["source failed".into()],
            links: 4,
            unique: 2,
            duplicates: 1,
            invalid: 1,
            unsupported: 1,
            geo: true,
            geo_countries: 3,
            tcp_enabled: true,
            tcp_total: 2,
            tcp_alive: 1,
            speed_enabled: true,
            speed_total: 1,
            speed_passed: 1,
            bad_outbounds: 1,
            alive: 1,
            good: 1,
            avg_kbs: 2048.0,
            median_kbs: 1024.0,
            raw_file: Some(("raw.txt".into(), 1)),
            parsed_file: Some(("parsed.txt".into(), 2)),
            alive_file: Some(("alive.txt".into(), 1)),
            good_file: Some(("good.txt".into(), 1)),
            clash_file: Some(("clash.yaml".into(), 1)),
            dead_file: Some(("dead.txt".into(), 1)),
            ..Summary::default()
        };

        print_summary(&summary);
    }

    #[test]
    fn print_summary_handles_disabled_and_empty_optional_sections() {
        print_summary(&Summary::default());
    }
}
