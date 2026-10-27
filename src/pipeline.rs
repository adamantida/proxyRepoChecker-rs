use super::Summary;
use crate::checkpoint;
use crate::cli::Cli;
use crate::fetch;
use crate::geoip;
use crate::output;
use crate::parse::{self, Proxy};
use crate::source_report;
use crate::sources;
use crate::tcp_ping;
use crate::xray::{self, SpeedOpts};
use rand::seq::SliceRandom;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::Instant;

pub(super) fn default_geoip_path() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(dir.join("geoip.dat"))
}

pub(super) fn default_core_path() -> anyhow::Result<PathBuf> {
    let exe = std::env::current_exe()?;
    let dir = exe
        .parent()
        .map(|p| p.to_path_buf())
        .unwrap_or_else(|| PathBuf::from("."));
    Ok(dir.join(xray::core_bin_name()))
}

/// Ищет файл: сначала по пути, потом рядом с exe, потом в bin/ проекта.
pub(super) fn resolve_default(path: &str) -> PathBuf {
    let p = PathBuf::from(path);
    resolve_default_from(
        &p,
        std::env::current_exe().ok().as_deref(),
        Path::new("bin"),
    )
}

pub(super) fn resolve_default_from(path: &Path, exe: Option<&Path>, bin_dir: &Path) -> PathBuf {
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

fn write_source_report(
    cli: &Cli,
    report: &source_report::SourceQualityReport,
) -> anyhow::Result<()> {
    if let Some(path) = &cli.source_report {
        output::write_text_atomic(path, &report.to_pretty_json()?)?;
    }
    Ok(())
}

fn save_checkpoint(cli: &Cli, checkpoint: &checkpoint::Checkpoint) -> anyhow::Result<()> {
    if let Some(path) = &cli.checkpoint {
        checkpoint.save(path)?;
    }
    Ok(())
}

pub(super) fn filter_proxy_protocol_country(
    proxies: &mut Vec<Proxy>,
    protocols: &[String],
    countries: &[String],
) -> usize {
    let before = proxies.len();
    let allowed_protocols: HashSet<String> = protocols
        .iter()
        .map(|protocol| protocol.to_ascii_lowercase())
        .collect();
    let allowed_countries: HashSet<String> = countries
        .iter()
        .map(|country| country.trim().to_ascii_uppercase())
        .collect();
    proxies.retain(|proxy| {
        (allowed_protocols.is_empty() || allowed_protocols.contains(proxy.proto))
            && (allowed_countries.is_empty()
                || allowed_countries.contains(&proxy.country.trim().to_ascii_uppercase()))
    });
    before - proxies.len()
}

pub(super) fn filter_previously_seen(proxies: &mut Vec<Proxy>, existing: &str) -> usize {
    let known: HashSet<String> = existing
        .lines()
        .filter_map(|line| parse::parse_link(line).ok().map(|proxy| proxy.key()))
        .collect();
    let before = proxies.len();
    proxies.retain(|proxy| !known.contains(&proxy.key()));
    before - proxies.len()
}

pub(super) fn retain_within_max_ping(candidates: &mut Vec<Proxy>, max_ping: u64) -> Vec<String> {
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

pub(super) async fn run(mut cli: Cli) -> anyhow::Result<Summary> {
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
    let mut checkpoint = if cli.resume {
        let path = cli
            .checkpoint
            .as_deref()
            .expect("CLI validation requires a checkpoint path for --resume");
        checkpoint::Checkpoint::load(path)?
            .ok_or_else(|| anyhow::anyhow!("checkpoint для --resume не найден: {path}"))?
    } else {
        checkpoint::Checkpoint::new()
    };
    let resume_stage = checkpoint.stage;
    if cli.resume
        && resume_stage < checkpoint::PipelineStage::Parsed
        && (resume_stage != checkpoint::PipelineStage::Fetched || checkpoint.raw_links.is_empty())
    {
        anyhow::bail!("checkpoint ещё не содержит завершённый этап парсинга");
    }
    let mut source_quality = checkpoint.source_quality.clone();
    let mut source_owner: HashMap<String, usize> = checkpoint.source_owners.clone();
    let mut proxies;
    if cli.resume {
        if resume_stage == checkpoint::PipelineStage::Fetched {
            let raw_links = checkpoint.raw_links.clone();
            let (restored, stats) = parse::parse_all(raw_links.clone());
            proxies = restored;
            sum.links = stats.links;
            sum.unique = stats.parsed;
            sum.duplicates = stats.duplicates;
            sum.invalid = stats.invalid;
            sum.unsupported = stats.unsupported.values().sum();
            checkpoint.parse_stats = checkpoint::ParseSnapshotStats {
                links: stats.links,
                parsed: stats.parsed,
                duplicates: stats.duplicates,
                invalid: stats.invalid,
                unsupported: stats.unsupported.values().sum(),
            };
            checkpoint.record_stage(
                checkpoint::PipelineStage::Parsed,
                raw_links,
                proxies
                    .iter()
                    .map(checkpoint::ProxySnapshot::from_proxy)
                    .collect(),
            );
            save_checkpoint(&cli, &checkpoint)?;
        } else {
            proxies = checkpoint
                .proxies
                .iter()
                .cloned()
                .map(checkpoint::ProxySnapshot::into_proxy)
                .collect::<anyhow::Result<Vec<_>>>()?;
            sum.links = checkpoint.parse_stats.links;
            sum.unique = checkpoint.parse_stats.parsed;
            sum.duplicates = checkpoint.parse_stats.duplicates;
            sum.invalid = checkpoint.parse_stats.invalid;
            sum.unsupported = checkpoint.parse_stats.unsupported;
        }
        if proxies.is_empty() {
            anyhow::bail!("checkpoint не содержит прокси для продолжения");
        }
        sum.unique = proxies.len();
        info!(
            "Продолжение с завершённого этапа {:?}: {} прокси",
            resume_stage,
            proxies.len()
        );
    } else {
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
        let report = fetch::fetch_all_with_options(
            &src.fetch_urls,
            &fetch::FetchOptions {
                concurrency: cli.fetch_concurrency,
                timeout: std::time::Duration::from_secs_f64(cli.fetch_timeout),
                max_response_bytes: cli.max_source_bytes,
                retries: cli.fetch_retries,
                insecure_tls: cli.insecure_source_tls,
                cache_dir: (!cli.no_fetch_cache).then(|| PathBuf::from(&cli.fetch_cache_dir)),
            },
        )
        .await;
        let mut seen_source_keys = HashSet::new();
        for success in &report.successes {
            let index = source_quality.sources.len();
            let row = source_quality.add_source(&success.url);
            row.status = source_report::SourceStatus::Fetched;
            row.counts.fetched = 1;
            for link in parse::extract_links(&success.text) {
                if let Ok(proxy) = parse::parse_link(&link) {
                    row.counts.valid += 1;
                    let key = proxy.key();
                    if !seen_source_keys.insert(key.clone()) {
                        row.counts.duplicate += 1;
                    } else {
                        source_owner.insert(key, index);
                    }
                }
            }
            row.status = source_report::SourceStatus::Parsed;
        }
        for failure in &report.failures {
            let row = source_quality.add_source(&failure.url);
            row.status = source_report::SourceStatus::FetchFailed;
        }
        let mut texts = report.texts;
        let unchanged_from_cache = report
            .successes
            .iter()
            .filter(|success| success.cache == fetch::CacheDisposition::NotModified)
            .count();
        let stale_from_cache = report
            .successes
            .iter()
            .filter(|success| success.cache == fetch::CacheDisposition::StaleFallback)
            .count();
        sum.fetch_loaded = texts.len();
        sum.fetch_failed = report.failures.len();
        sum.fetch_failure_examples = report
            .failures
            .iter()
            .take(5)
            .map(|failure| {
                format!(
                    "{} — {} (попыток: {})",
                    source_report::redact_url(&failure.url),
                    failure.reason,
                    failure.attempts
                )
            })
            .collect();
        sum.fetch_secs = t.elapsed().as_secs_f64();
        info!(
            "Загружено подписок: {}, ошибок: {}, без изменений в кэше: {}, использовано устаревших копий: {}",
            texts.len(),
            sum.fetch_failed,
            unchanged_from_cache,
            stale_from_cache
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

        let raw_links_for_checkpoint = links.clone();
        checkpoint.raw_links = raw_links_for_checkpoint.clone();
        checkpoint.source_quality = source_quality.clone();
        checkpoint.source_owners = source_owner.clone();
        checkpoint.record_stage(
            checkpoint::PipelineStage::Fetched,
            raw_links_for_checkpoint.clone(),
            Vec::new(),
        );
        save_checkpoint(&cli, &checkpoint)?;
        let (parsed_proxies, stats) = parse::parse_all(links);
        proxies = parsed_proxies;
        sum.parse_secs = t.elapsed().as_secs_f64();
        sum.links = stats.links;
        sum.unique = stats.parsed;
        sum.duplicates = stats.duplicates;
        sum.invalid = stats.invalid;
        sum.unsupported = stats.unsupported.values().sum();
        checkpoint.parse_stats = checkpoint::ParseSnapshotStats {
            links: stats.links,
            parsed: stats.parsed,
            duplicates: stats.duplicates,
            invalid: stats.invalid,
            unsupported: stats.unsupported.values().sum(),
        };
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
        checkpoint.raw_links = raw_links_for_checkpoint.clone();
        checkpoint.record_stage(
            checkpoint::PipelineStage::Parsed,
            raw_links_for_checkpoint,
            proxies
                .iter()
                .map(checkpoint::ProxySnapshot::from_proxy)
                .collect(),
        );
        save_checkpoint(&cli, &checkpoint)?;
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
        if resume_stage < checkpoint::PipelineStage::GeoIp {
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
        }
    } else if resume_stage < checkpoint::PipelineStage::GeoIp {
        for p in proxies.iter_mut() {
            p.display = p.name.clone();
        }
    }
    if resume_stage < checkpoint::PipelineStage::GeoIp && cli.only_new {
        let existing = match std::fs::read_to_string(&cli.output) {
            Ok(contents) => contents,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => String::new(),
            Err(error) => return Err(error.into()),
        };
        let removed = filter_previously_seen(&mut proxies, &existing);
        info!("--only-new: удалено уже известных прокси: {removed}");
    }
    if resume_stage < checkpoint::PipelineStage::GeoIp
        && (!cli.protocol.is_empty() || !cli.country.is_empty())
    {
        let removed = filter_proxy_protocol_country(&mut proxies, &cli.protocol, &cli.country);
        info!("Фильтры протокол/страна: удалено {removed} прокси");
    }
    checkpoint.record_stage(
        checkpoint::PipelineStage::GeoIp,
        checkpoint.processed_links.clone(),
        proxies
            .iter()
            .map(checkpoint::ProxySnapshot::from_proxy)
            .collect(),
    );
    checkpoint.source_quality = source_quality.clone();
    save_checkpoint(&cli, &checkpoint)?;

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
    if !cli.no_tcp_ping && resume_stage < checkpoint::PipelineStage::TcpPing {
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
        for proxy in &alive {
            if let Some(index) = source_owner.get(&proxy.key()) {
                source_quality.sources[*index].counts.tcp_live += 1;
            }
        }
        for row in &mut source_quality.sources {
            if row.status == source_report::SourceStatus::Parsed {
                row.status = source_report::SourceStatus::TcpChecked;
            }
        }
        candidates = alive;
    }

    if candidates.is_empty() {
        info!("Нет живых прокси после TCP ping.");
        if let Some(path) = &cli.dead {
            output::write_lines_atomic(path, &dead_urls)?;
            sum.dead_file = Some((path.clone(), dead_urls.len()));
        }
        if let Some(path) = &cli.json_output {
            output::write_proxy_json(path, &[])?;
        }
        checkpoint.proxies.clear();
        checkpoint.complete(Vec::new());
        save_checkpoint(&cli, &checkpoint)?;
        write_source_report(&cli, &source_quality)?;
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

    checkpoint.record_stage(
        checkpoint::PipelineStage::TcpPing,
        checkpoint.processed_links.clone(),
        candidates
            .iter()
            .map(checkpoint::ProxySnapshot::from_proxy)
            .collect(),
    );
    checkpoint.source_quality = source_quality.clone();
    save_checkpoint(&cli, &checkpoint)?;

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
    } else if resume_stage >= checkpoint::PipelineStage::Speed {
        good = candidates.clone();
        good.sort_by(|a, b| {
            b.speed_kbs
                .partial_cmp(&a.speed_kbs)
                .unwrap_or(std::cmp::Ordering::Equal)
        });
        sum.speed_passed = good.len();
        sum.speed_total = good.len();
        output::write_lines_atomic(
            &cli.good_output,
            &good
                .iter()
                .map(|proxy| output::line(proxy, rename))
                .collect::<Vec<_>>(),
        )?;
        sum.good_file = Some((cli.good_output.clone(), good.len()));
        alive = candidates;
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
        for proxy in &passed {
            if let Some(index) = source_owner.get(&proxy.key()) {
                source_quality.sources[*index].counts.speed_qualified += 1;
            }
        }
        for row in &mut source_quality.sources {
            if matches!(
                row.status,
                source_report::SourceStatus::Parsed | source_report::SourceStatus::TcpChecked
            ) {
                row.status = source_report::SourceStatus::SpeedChecked;
            }
        }
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
        checkpoint.record_stage(
            checkpoint::PipelineStage::Speed,
            checkpoint.processed_links.clone(),
            good.iter()
                .map(checkpoint::ProxySnapshot::from_proxy)
                .collect(),
        );
        checkpoint.source_quality = source_quality.clone();
        save_checkpoint(&cli, &checkpoint)?;
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
    if let Some(path) = &cli.json_output {
        output::write_proxy_json(path, &alive)?;
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

    write_source_report(&cli, &source_quality)?;

    checkpoint.proxies = alive
        .iter()
        .map(checkpoint::ProxySnapshot::from_proxy)
        .collect();
    checkpoint.complete(
        alive
            .iter()
            .map(|proxy| output::line(proxy, rename))
            .collect(),
    );
    checkpoint.source_quality = source_quality.clone();
    save_checkpoint(&cli, &checkpoint)?;

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
    use super::*;
    use clap::Parser;
    use std::sync::atomic::{AtomicUsize, Ordering};

    static NEXT_DIR: AtomicUsize = AtomicUsize::new(0);

    struct TestDir(PathBuf);

    impl TestDir {
        fn new() -> Self {
            let id = NEXT_DIR.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("proxy-rs-pipeline-{}-{id}", std::process::id()));
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

    #[cfg(unix)]
    fn write_fake_xray(path: &Path) {
        use std::os::unix::fs::PermissionsExt;

        let script = r##"#!/bin/sh
if [ "$1" = version ]; then
  echo 'Xray local unit fixture'
  exit 0
fi
exec python3 - "$3" <<'PY'
import ipaddress, json, select, socket, sys, threading
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
    remote = None
    try:
        _, methods = exact(client, 2)
        exact(client, methods)
        client.sendall(b"\x05\x00")
        _, command, _, atyp = exact(client, 4)
        if command != 1:
            raise ValueError("expected CONNECT")
        if atyp == 1:
            host = socket.inet_ntoa(exact(client, 4))
        elif atyp == 3:
            length = exact(client, 1)[0]
            host = exact(client, length).decode()
        elif atyp == 4:
            host = socket.inet_ntop(socket.AF_INET6, exact(client, 16))
        else:
            raise ValueError("unsupported address type")
        destination_port = int.from_bytes(exact(client, 2), "big")
        if not ipaddress.ip_address(host).is_loopback:
            raise ValueError("non-loopback destination blocked")
        remote = socket.create_connection((host, destination_port), timeout=3)
        client.sendall(b"\x05\x00\x00\x01\x7f\x00\x00\x01\x00\x00")
        while True:
            ready, _, _ = select.select([client, remote], [], [], 5)
            if not ready:
                break
            for source in ready:
                data = source.recv(65536)
                if not data:
                    return
                (remote if source is client else client).sendall(data)
    except Exception:
        pass
    finally:
        client.close()
        if remote:
            remote.close()
while True:
    client, _ = server.accept()
    threading.Thread(target=handle, args=(client,), daemon=True).start()
PY
"##;
        let mut contents = script.to_string();
        if contents.len() < 1_000_100 {
            contents.push_str(&"#".repeat(1_000_100 - contents.len()));
        }
        std::fs::write(path, contents).expect("write local fake Xray");
        let mut permissions = std::fs::metadata(path).unwrap().permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(path, permissions).unwrap();
    }

    #[cfg(unix)]
    async fn serve_speed_fixture() -> (String, tokio::task::JoinHandle<()>) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let listener = tokio::net::TcpListener::bind(("127.0.0.1", 0))
            .await
            .expect("bind local speed fixture");
        let address = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let (mut stream, _) = listener.accept().await.expect("accept speed request");
            let mut request = [0_u8; 2048];
            let _ = stream.read(&mut request).await.expect("read speed request");
            let body = vec![b'x'; 16 * 1024];
            let headers = format!(
                "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
                body.len()
            );
            stream
                .write_all(headers.as_bytes())
                .await
                .expect("write speed response headers");
            stream
                .write_all(&body)
                .await
                .expect("write speed response body");
        });
        (format!("http://{address}/speed-fixture"), task)
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn run_completes_local_speed_good_and_clash_pipeline() {
        let temp = TestDir::new();
        let links = temp.path("links.txt");
        std::fs::write(
            &links,
            "vless://01234567-89ab-cdef-0123-456789abcdef@127.0.0.1:443#local-unit-speed-node\n",
        )
        .unwrap();
        let core = temp.path("fake-xray");
        write_fake_xray(&core);
        let (speed_url, speed_server) = serve_speed_fixture().await;

        let mut cli = cli();
        cli.file = Some(links.display().to_string());
        cli.sources = temp.path("missing-sources.json").display().to_string();
        cli.output = temp.path("alive.txt").display().to_string();
        cli.raw_output = temp.path("raw.txt").display().to_string();
        cli.parsed_output = temp.path("parsed.txt").display().to_string();
        cli.good_output = temp.path("good.txt").display().to_string();
        cli.clash = Some(temp.path("clash.yaml").display().to_string());
        cli.core = core.display().to_string();
        cli.workdir = temp.path("work").display().to_string();
        cli.speed_url = Some(speed_url);
        cli.speed_timeout = 3.0;
        cli.speed_connect_timeout = 2.0;
        cli.min_kb = 0.0;
        cli.batch = 1;
        cli.batch_workers = 1;
        cli.speed_threads = 1;
        cli.no_geoip = true;
        cli.no_tcp_ping = true;
        cli.no_xray_download = true;

        let summary = run(cli).await.unwrap();
        speed_server.await.unwrap();

        assert_eq!(summary.speed_total, 1);
        assert_eq!(summary.speed_passed, 1);
        assert_eq!(summary.good, 1);
        assert_eq!(
            summary.clash_file.as_ref().map(|(_, count)| *count),
            Some(1)
        );
        assert!(std::fs::read_to_string(temp.path("good.txt"))
            .unwrap()
            .contains("local-unit-speed-node"));
        let clash = std::fs::read_to_string(temp.path("clash.yaml")).unwrap();
        assert!(clash.contains("local-unit-speed-node"), "{clash}");
        assert!(clash.contains("proxy-groups:"), "{clash}");
    }

    fn proxy(id: &str, host: &str, country: &str) -> Proxy {
        parse::parse_link(&format!(
            "vless://{id}@{host}:443?type=tcp&security=none#{id}"
        ))
        .map(|mut proxy| {
            proxy.country = country.into();
            proxy
        })
        .unwrap()
    }

    fn cli() -> Cli {
        Cli::try_parse_from(["proxy-rs"]).unwrap()
    }

    #[test]
    fn resolve_default_from_checks_original_executable_and_bin_paths_in_order() {
        let dir = TestDir::new();
        let exe_dir = dir.path("exe");
        let bin_dir = dir.path("bin");
        std::fs::create_dir_all(&exe_dir).unwrap();
        std::fs::create_dir_all(&bin_dir).unwrap();
        let executable = exe_dir.join("proxy-rs");
        let name = Path::new("geoip.dat");

        let original = dir.path("geoip.dat");
        std::fs::write(&original, b"original").unwrap();
        let executable_copy = exe_dir.join(name);
        std::fs::write(&executable_copy, b"exe").unwrap();
        let bin_copy = bin_dir.join(name);
        std::fs::write(&bin_copy, b"bin").unwrap();
        assert_eq!(
            resolve_default_from(&original, Some(&executable), &bin_dir),
            original
        );

        std::fs::remove_file(&original).unwrap();
        assert_eq!(
            resolve_default_from(name, Some(&executable), &bin_dir),
            executable_copy
        );

        std::fs::remove_file(&executable_copy).unwrap();
        assert_eq!(
            resolve_default_from(name, Some(&executable), &bin_dir),
            bin_copy
        );

        std::fs::remove_file(&bin_copy).unwrap();
        assert_eq!(
            resolve_default_from(name, Some(&executable), &bin_dir),
            name
        );
        assert_eq!(resolve_default_from(name, None, &bin_dir), name);
    }

    #[test]
    fn default_runtime_paths_are_next_to_the_current_executable() {
        let exe_dir = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .to_path_buf();
        assert_eq!(default_geoip_path().unwrap(), exe_dir.join("geoip.dat"));
        assert_eq!(
            default_core_path().unwrap(),
            exe_dir.join(xray::core_bin_name())
        );
    }

    #[test]
    fn source_report_writer_is_optional_and_writes_pretty_report_when_configured() {
        let dir = TestDir::new();
        let mut report = source_report::SourceQualityReport::new();
        report.add_source("https://example.test/sub");

        assert!(write_source_report(&cli(), &report).is_ok());

        let path = dir.path("report.json");
        let mut configured = cli();
        configured.source_report = Some(path.display().to_string());
        write_source_report(&configured, &report).unwrap();
        let contents = std::fs::read_to_string(path).unwrap();
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&contents).unwrap()["sources"][0]["url"],
            "https://example.test/[redacted]"
        );
        assert!(contents.contains('\n'));
    }

    #[test]
    fn checkpoint_writer_is_optional_and_persists_configured_checkpoint() {
        let dir = TestDir::new();
        let mut checkpoint = checkpoint::Checkpoint::new();
        checkpoint.raw_links.push("local input".into());

        save_checkpoint(&cli(), &checkpoint).unwrap();

        let path = dir.path("state.json");
        let mut configured = cli();
        configured.checkpoint = Some(path.display().to_string());
        save_checkpoint(&configured, &checkpoint).unwrap();
        assert_eq!(
            checkpoint::Checkpoint::load(path).unwrap(),
            Some(checkpoint)
        );
    }

    #[test]
    fn protocol_and_country_filters_are_case_insensitive_and_composable() {
        let mut proxies = vec![
            proxy("01234567-89ab-cdef-0123-456789abcdef", "de.example", " de "),
            proxy("11234567-89ab-cdef-0123-456789abcdef", "us.example", "US"),
            proxy("21234567-89ab-cdef-0123-456789abcdef", "de2.example", "FR"),
        ];

        let removed =
            filter_proxy_protocol_country(&mut proxies, &["VLESS".into()], &[" de ".into()]);

        assert_eq!(removed, 2);
        assert_eq!(proxies.len(), 1);
        assert_eq!(proxies[0].host, "de.example");
    }

    #[test]
    fn protocol_and_country_filters_allow_empty_lists_and_report_removals() {
        let mut proxies = vec![
            proxy("01234567-89ab-cdef-0123-456789abcdef", "one.example", "DE"),
            proxy("11234567-89ab-cdef-0123-456789abcdef", "two.example", "US"),
        ];
        assert_eq!(filter_proxy_protocol_country(&mut proxies, &[], &[]), 0);
        assert_eq!(
            filter_proxy_protocol_country(&mut proxies, &["trojan".into()], &[]),
            2
        );
        assert!(proxies.is_empty());
    }

    #[test]
    fn previously_seen_filter_ignores_invalid_lines_and_matches_proxy_identity() {
        let known = proxy(
            "01234567-89ab-cdef-0123-456789abcdef",
            "known.example",
            "DE",
        );
        let same_identity_different_name =
            "vless://01234567-89ab-cdef-0123-456789abcdef@known.example:443?type=tcp&security=none#renamed";
        let mut candidates = vec![
            known.clone(),
            proxy("11234567-89ab-cdef-0123-456789abcdef", "new.example", "DE"),
        ];

        let removed = filter_previously_seen(
            &mut candidates,
            &format!("not a proxy\n{same_identity_different_name}\n"),
        );

        assert_eq!(removed, 1);
        assert_eq!(candidates.len(), 1);
        assert_eq!(candidates[0].host, "new.example");
        assert_eq!(
            known.key(),
            parse::parse_link(same_identity_different_name)
                .unwrap()
                .key()
        );
    }

    #[test]
    fn max_ping_zero_disables_filter_and_positive_limit_keeps_boundary_only() {
        let mut unlimited = vec![
            proxy(
                "01234567-89ab-cdef-0123-456789abcdef",
                "unknown.example",
                "DE",
            ),
            proxy("11234567-89ab-cdef-0123-456789abcdef", "slow.example", "DE"),
        ];
        unlimited[0].ping = None;
        unlimited[1].ping = Some(101);
        assert!(retain_within_max_ping(&mut unlimited, 0).is_empty());
        assert_eq!(unlimited.len(), 2);

        let mut limited = vec![
            proxy("21234567-89ab-cdef-0123-456789abcdef", "fast.example", "DE"),
            proxy(
                "31234567-89ab-cdef-0123-456789abcdef",
                "boundary.example",
                "DE",
            ),
            proxy("41234567-89ab-cdef-0123-456789abcdef", "slow.example", "DE"),
            proxy(
                "51234567-89ab-cdef-0123-456789abcdef",
                "unknown.example",
                "DE",
            ),
        ];
        limited[0].ping = Some(20);
        limited[1].ping = Some(100);
        limited[2].ping = Some(101);
        limited[3].ping = None;

        let rejected = retain_within_max_ping(&mut limited, 100);

        assert_eq!(
            rejected,
            [
                "vless://41234567-89ab-cdef-0123-456789abcdef@slow.example:443?type=tcp&security=none",
                "vless://51234567-89ab-cdef-0123-456789abcdef@unknown.example:443?type=tcp&security=none"
            ]
        );
        assert_eq!(
            limited.iter().map(|proxy| proxy.ping).collect::<Vec<_>>(),
            [Some(20), Some(100)]
        );
    }
}
