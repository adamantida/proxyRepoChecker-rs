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

/// Ищет файл: сначала по пути, потом рядом с exe, потом в bin/ проекта.
fn resolve_default(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    if p.exists() {
        return p;
    }
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            let cand = dir.join(path);
            if cand.exists() {
                return cand;
            }
        }
    }
    let bin = PathBuf::from("bin").join(path);
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

#[derive(Default)]
struct Summary {
    fetch_sources: usize,
    fetch_loaded: usize,
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
    alive: usize,
    good: usize,
    avg_kbs: f64,
    median_kbs: f64,
    alive_file: Option<(String, usize)>,
    good_file: Option<(String, usize)>,
    dead_file: Option<(String, usize)>,
    total_secs: f64,
}

fn print_summary(s: &Summary) {
    let line = "═".repeat(46);
    println!("\n{}", line.bold().cyan());
    println!("{}", "  ИТОГИ".bold().cyan());
    println!("{}", line.bold().cyan());

    println!("  {} {}", "Общее время:".bold(), fmt_dur(s.total_secs).yellow());

    println!("  {}", "Источники".bold().underline());
    println!(
        "    подписок: {}   загружено: {}   ({})",
        s.fetch_sources,
        s.fetch_loaded,
        fmt_dur(s.fetch_secs)
    );
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
        println!(
            "    стран: {}   ({})",
            s.geo_countries,
            fmt_dur(s.geo_secs)
        );
    }

    if s.tcp_enabled {
        println!("  {}", "TCP ping".bold().underline());
        let pct = if s.tcp_total > 0 {
            s.tcp_alive as f64 / s.tcp_total as f64 * 100.0
        } else {
            0.0
        };
        println!(
            "    живых: {}/{} ({pct:.1}%)   ({})",
            s.tcp_alive.to_string().green(),
            s.tcp_total,
            fmt_dur(s.tcp_secs)
        );
    }

    if s.speed_enabled {
        println!("  {}", "Скорость (xray)".bold().underline());
        let pct = if s.speed_total > 0 {
            s.speed_passed as f64 / s.speed_total as f64 * 100.0
        } else {
            0.0
        };
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
    }

    println!(
        "  {} {}",
        "Живых:".bold(),
        s.alive.to_string().green().bold()
    );

    let files: Vec<String> = [
        s.alive_file.as_ref(),
        s.good_file.as_ref(),
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
    let mut cli = Cli::parse();
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
    let mut texts =
        fetch::fetch_all(&src.fetch_urls, cli.fetch_concurrency, cli.fetch_timeout).await;
    sum.fetch_loaded = texts.len();
    sum.fetch_secs = t.elapsed().as_secs_f64();
    info!("Загружено подписок: {}", texts.len());

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
        let txt: Vec<String> = list
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
        info!("Неподдерживаемые протоколы: {}", txt.join(", "));
    }
    if !stats.invalid_reasons.is_empty() {
        let mut list: Vec<(&&'static str, &usize)> = stats.invalid_reasons.iter().collect();
        list.sort_by(|a, b| b.1.cmp(a.1));
        let txt: Vec<String> = list
            .iter()
            .map(|(k, v)| format!("{k}: {v}"))
            .collect();
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
            output::write_lines(path, &dead_urls)?;
            sum.dead_file = Some((path.clone(), dead_urls.len()));
        }
        sum.total_secs = started.elapsed().as_secs_f64();
        print_summary(&sum);
        return Ok(());
    }

    // --max-ping отсекает по TCP RTT (из фазы 1)
    if cli.max_ping > 0 {
        let before = candidates.len();
        candidates.retain(|p| {
            if p.ping.unwrap_or(u64::MAX) <= cli.max_ping {
                true
            } else {
                dead_urls.push(p.base_url.clone());
                false
            }
        });
        if candidates.len() < before {
            info!(
                "--max-ping {}: отброшено {} с более высоким TCP ping",
                cli.max_ping,
                before - candidates.len()
            );
        }
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
            )
            .await;
        sum.speed_secs = t.elapsed().as_secs_f64();
        sum.speed_passed = passed.len();

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
        output::write_lines(
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

    output::write_lines(
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
        output::write_lines(path, &dead_urls)?;
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
        sum.median_kbs = if sorted.len() % 2 == 0 {
            (sorted[mid - 1] + sorted[mid]) / 2.0
        } else {
            sorted[mid]
        };
    }
    sum.total_secs = started.elapsed().as_secs_f64();

    info!(
        "Готово за {:.1}s | живых: {} | good: {}",
        sum.total_secs,
        alive.len(),
        good.len()
    );
    print_summary(&sum);
    Ok(())
}
