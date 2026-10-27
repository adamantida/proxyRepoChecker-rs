use clap::builder::styling::{AnsiColor, Effects, Styles};
use clap::Parser;

const STYLES: Styles = Styles::styled()
    .header(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .usage(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .literal(AnsiColor::Cyan.on_default().effects(Effects::BOLD))
    .placeholder(AnsiColor::Yellow.on_default())
    .error(AnsiColor::Red.on_default().effects(Effects::BOLD))
    .valid(AnsiColor::Green.on_default().effects(Effects::BOLD))
    .invalid(AnsiColor::Yellow.on_default().effects(Effects::BOLD));

const EXAMPLES: &str = "\
Примеры:
  proxy-rs --set 3 -n 200                       быстрый тест (набор 3, первые 200)
  proxy-rs --set 1 --shuffle --dead dead.txt    весь набор 1, вперемешку, с мёртвыми
  proxy-rs --no-speed                           только TCP ping (без замера скорости)
  proxy-rs -u https://example.com/sub --min-kb 500 -n 100
  proxy-rs --set all --speed-threads 32 --min-kb 1000";

#[derive(Parser, Debug)]
#[command(
    name = "proxy-rs",
    version,
    about = "Парсинг прокси из агрегаторов, проверка TCP ping и скорости через xray",
    after_help = EXAMPLES,
    styles = STYLES,
    help_template = "\
{before-help}{name} {version}
{about}

{usage-heading} {usage}

{all-args}{after-help}"
)]
pub struct Cli {
    // ---------------------------- Источники ----------------------------
    /// Набор источников из sources.json: 1, 2, 3 или all
    #[arg(long = "set", default_value = "3", help_heading = "Источники")]
    pub set: String,

    /// Путь к sources.json
    #[arg(long, default_value = "sources.json", help_heading = "Источники")]
    pub sources: String,

    /// Файл с доп. ссылками (http-строки = подписки, остальные = готовые прокси)
    #[arg(short = 'f', long, value_name = "FILE", help_heading = "Источники")]
    pub file: Option<String>,

    /// Доп. URL подписок для загрузки
    #[arg(short = 'u', long, value_name = "URL", help_heading = "Источники")]
    pub url: Vec<String>,

    /// Параллельных загрузок источников
    #[arg(
        long = "fetch-concurrency",
        default_value_t = 32,
        help_heading = "Источники"
    )]
    pub fetch_concurrency: usize,

    /// Таймаут загрузки источника, сек (1..=3600)
    #[arg(
        long = "fetch-timeout",
        default_value_t = 15.0,
        help_heading = "Источники"
    )]
    pub fetch_timeout: f64,

    /// Повторные попытки загрузки после временной ошибки (0..=8)
    #[arg(
        long = "fetch-retries",
        default_value_t = 2,
        help_heading = "Источники"
    )]
    pub fetch_retries: usize,

    /// Максимальный размер ответа одного источника в байтах (1 KiB..=1 GiB)
    #[arg(long = "max-source-bytes", default_value_t = crate::fetch::DEFAULT_MAX_SOURCE_BYTES, help_heading = "Источники")]
    pub max_source_bytes: usize,

    /// Разрешить недоверенные TLS-сертификаты источников (небезопасно)
    #[arg(long = "insecure-source-tls", help_heading = "Источники")]
    pub insecure_source_tls: bool,

    /// Каталог кэша подписок; по умолчанию .proxy_cache
    #[arg(
        long = "fetch-cache-dir",
        default_value = ".proxy_cache",
        help_heading = "Источники"
    )]
    pub fetch_cache_dir: String,

    /// Отключить кэш подписок
    #[arg(long = "no-fetch-cache", help_heading = "Источники")]
    pub no_fetch_cache: bool,

    /// Сохранить безопасный JSON-отчёт качества источников
    #[arg(long = "source-report", value_name = "FILE", help_heading = "Вывод")]
    pub source_report: Option<String>,

    /// Сохранять checkpoint каждого завершённого этапа в этот файл
    #[arg(long, value_name = "FILE", help_heading = "Вывод")]
    pub checkpoint: Option<String>,

    /// Продолжить обработку из checkpoint (--checkpoint обязателен)
    #[arg(long, help_heading = "Источники")]
    pub resume: bool,

    /// Фильтровать по протоколу; можно указать несколько раз или через запятую
    #[arg(long, value_delimiter = ',', action = clap::ArgAction::Append, help_heading = "Фильтры")]
    pub protocol: Vec<String>,

    /// Фильтровать по ISO-коду страны или GeoIP-тегу; можно повторять
    #[arg(long, value_delimiter = ',', action = clap::ArgAction::Append, help_heading = "Фильтры")]
    pub country: Vec<String>,

    /// Оставить только прокси, которых нет в текущем --output файле
    #[arg(long, help_heading = "Фильтры")]
    pub only_new: bool,

    /// Записать JSON-массив результатов без URI-учётных данных
    #[arg(long, value_name = "FILE", help_heading = "Вывод")]
    pub json_output: Option<String>,

    // ------------------------------ Вывод ------------------------------
    /// Файл с живыми (сортировка по ping)
    #[arg(short = 'o', long, default_value = "alive.txt", help_heading = "Вывод")]
    pub output: String,

    /// Файл с прошедшими проверку скорости
    #[arg(long, default_value = "good.txt", help_heading = "Вывод")]
    pub good_output: String,

    /// Файл с мёртвыми
    #[arg(long, help_heading = "Вывод")]
    pub dead: Option<String>,

    /// Проверять только первые N прокси (N > 0)
    #[arg(short = 'n', long = "limit", value_name = "N", help_heading = "Вывод")]
    pub limit: Option<usize>,

    /// Перемешать список перед проверкой
    #[arg(short = 's', long, help_heading = "Вывод")]
    pub shuffle: bool,

    /// Сырые тексты загруженных источников (сохраняется после загрузки)
    #[arg(long = "raw-output", default_value = "raw.txt", help_heading = "Вывод")]
    pub raw_output: String,

    /// Уникальные прокси до проверок (сохраняется после парсинга и GeoIP)
    #[arg(
        long = "parsed-output",
        default_value = "parsed.txt",
        help_heading = "Вывод"
    )]
    pub parsed_output: String,

    /// Сгенерировать Clash/mihomo конфиг из good-прокси (без значения — clash.yaml)
    #[arg(
        long,
        value_name = "FILE",
        num_args = 0..=1,
        default_missing_value = "clash.yaml",
        help_heading = "Вывод"
    )]
    pub clash: Option<String>,

    /// В Clash-конфиге не создавать группы по странам
    #[arg(long = "clash-no-country", help_heading = "Вывод")]
    pub clash_no_country: bool,

    /// В Clash-конфиге не создавать группы-категории (Telegram/AI/YouTube/Игры)
    #[arg(long = "clash-no-categories", help_heading = "Вывод")]
    pub clash_no_categories: bool,

    // --------------------------- Ядро (xray) ---------------------------
    /// Путь к ядру (xray)
    #[arg(long, default_value_t = crate::xray::core_bin_name().to_string(), help_heading = "Ядро (xray)")]
    pub core: String,

    /// Рабочая папка для батч-конфигов
    #[arg(long, default_value = ".proxy_runtime", help_heading = "Ядро (xray)")]
    pub workdir: String,

    /// Прокси в одном батче xray (1..=10000)
    #[arg(long, default_value_t = 100, help_heading = "Ядро (xray)")]
    pub batch: usize,

    /// Базовый локальный порт для socks-inbound'ов (1024..=65000)
    #[arg(long = "lport", default_value_t = 10000, help_heading = "Ядро (xray)")]
    pub lport: u16,

    /// Сколько батчей проверять параллельно
    #[arg(long, default_value_t = 8, help_heading = "Ядро (xray)")]
    pub batch_workers: usize,

    /// Сколько секунд ждать старт ядра (1..=3600)
    #[arg(
        long = "core-start-timeout",
        default_value_t = 8.0,
        help_heading = "Ядро (xray)"
    )]
    pub core_start_timeout: f64,

    /// URL для скачивания ядра (по умолчанию — релиз XTLS/Xray-core под текущую ОС)
    #[arg(long = "xray-url", value_name = "URL", help_heading = "Ядро (xray)")]
    pub xray_url: Option<String>,

    /// Не скачивать ядро автоматически (ошибка, если его нет)
    #[arg(long = "no-xray-download", help_heading = "Ядро (xray)")]
    pub no_xray_download: bool,

    // ---------------------- TCP ping (префильтр) -----------------------
    /// Выключить TCP ping префильтр
    #[arg(long = "no-tcp-ping", help_heading = "TCP ping (префильтр)")]
    pub no_tcp_ping: bool,

    /// Таймаут TCP-коннекта, сек (0.1..=3600)
    #[arg(
        long = "tcp-timeout",
        default_value_t = 3.0,
        help_heading = "TCP ping (префильтр)"
    )]
    pub tcp_timeout: f64,

    /// Параллельных TCP-проб
    #[arg(
        long = "tcp-concurrency",
        default_value_t = 1000,
        help_heading = "TCP ping (префильтр)"
    )]
    pub tcp_concurrency: usize,

    /// Попыток TCP-пробы на адрес (1..=100)
    #[arg(
        long = "tcp-retries",
        default_value_t = 1,
        help_heading = "TCP ping (префильтр)"
    )]
    pub tcp_retries: u32,

    /// Максимальный TCP RTT, мс (0 = не фильтровать)
    #[arg(
        long = "tcp-max-ms",
        default_value_t = 0,
        help_heading = "TCP ping (префильтр)"
    )]
    pub tcp_max_ms: u64,

    /// Отбрасывать живые с TCP ping выше этого (0 = выключено)
    #[arg(
        long = "max-ping",
        default_value_t = 0,
        help_heading = "TCP ping (префильтр)"
    )]
    pub max_ping: u64,

    // ------------------------ Проверка скорости ------------------------
    /// Выключить проверку скорости
    #[arg(long = "no-speed", help_heading = "Проверка скорости")]
    pub no_speed: bool,

    /// Приоритетный URL для замера скорости
    #[arg(
        long = "speed-url",
        value_name = "URL",
        help_heading = "Проверка скорости"
    )]
    pub speed_url: Option<String>,

    /// Таймаут скачивания при замере скорости, сек (1..=3600)
    #[arg(
        long = "speed-timeout",
        default_value_t = 6.0,
        help_heading = "Проверка скорости"
    )]
    pub speed_timeout: f64,

    /// Таймаут соединения при замере скорости, сек (1..=3600)
    #[arg(
        long = "speed-connect-timeout",
        default_value_t = 3.0,
        help_heading = "Проверка скорости"
    )]
    pub speed_connect_timeout: f64,

    /// Максимум скачиваемого МБ на замер (0.5..=1024)
    #[arg(
        long = "speed-max-mb",
        default_value_t = 5.0,
        help_heading = "Проверка скорости"
    )]
    pub speed_max_mb: f64,

    /// Минимальная скорость, KB/s (меньше = мусор)
    #[arg(
        long = "min-kb",
        default_value_t = 100.0,
        help_heading = "Проверка скорости"
    )]
    pub min_kb: f64,

    /// Параллельных замеров скорости
    #[arg(
        long = "speed-threads",
        default_value_t = 16,
        help_heading = "Проверка скорости"
    )]
    pub speed_threads: usize,

    // ------------------------------- GeoIP -----------------------------
    /// Выключить GeoIP (без переименования в [XX] - NNN)
    #[arg(long = "no-geoip", help_heading = "GeoIP")]
    pub no_geoip: bool,

    /// Путь к geoip.dat
    #[arg(long = "geoip-db", value_name = "FILE", help_heading = "GeoIP")]
    pub geoip_db: Option<String>,

    /// URL для скачивания geoip.dat
    #[arg(
        long = "geoip-url",
        value_name = "URL",
        help_heading = "GeoIP",
        default_value = "https://raw.githubusercontent.com/runetfreedom/russia-v2ray-rules-dat/release/geoip.dat"
    )]
    pub geoip_url: String,
}

impl Cli {
    pub fn validate(&self) -> Result<(), String> {
        for (name, value, min, max) in [
            ("--fetch-timeout", self.fetch_timeout, 1.0, 3600.0),
            ("--core-start-timeout", self.core_start_timeout, 1.0, 3600.0),
            ("--tcp-timeout", self.tcp_timeout, 0.1, 3600.0),
            ("--speed-timeout", self.speed_timeout, 1.0, 3600.0),
            (
                "--speed-connect-timeout",
                self.speed_connect_timeout,
                1.0,
                3600.0,
            ),
        ] {
            if !value.is_finite() || !(min <= value && value <= max) {
                return Err(format!(
                    "{name} должен быть конечным числом в диапазоне {min}..={max}"
                ));
            }
        }
        for (name, value, min, max) in [
            ("--speed-max-mb", self.speed_max_mb, 0.5, 1024.0),
            ("--min-kb", self.min_kb, 0.0, 1_000_000_000.0),
        ] {
            if !value.is_finite() || value < min || value > max {
                return Err(format!(
                    "{name} должен быть конечным числом в диапазоне {min}..={max}"
                ));
            }
        }
        for (name, value, max) in [
            ("--fetch-concurrency", self.fetch_concurrency, 4096),
            ("--batch", self.batch, 10000),
            ("--batch-workers", self.batch_workers, 1024),
            ("--speed-threads", self.speed_threads, 4096),
            ("--tcp-concurrency", self.tcp_concurrency, 65536),
        ] {
            if value == 0 || value > max {
                return Err(format!("{name} должен быть в диапазоне 1..={max}"));
            }
        }
        if self.fetch_retries > 8 {
            return Err("--fetch-retries должен быть в диапазоне 0..=8".into());
        }
        if !(1024..=1_073_741_824).contains(&self.max_source_bytes) {
            return Err("--max-source-bytes должен быть в диапазоне 1024..=1073741824".into());
        }
        if !(1024..=65000).contains(&self.lport) {
            return Err("--lport должен быть в диапазоне 1024..=65000".into());
        }
        if !(1..=100).contains(&self.tcp_retries) {
            return Err("--tcp-retries должен быть в диапазоне 1..=100".into());
        }
        if self.limit == Some(0) {
            return Err("--limit должен быть больше 0".into());
        }
        if self.resume && self.checkpoint.is_none() {
            return Err("--resume требует указать --checkpoint FILE".into());
        }
        for protocol in &self.protocol {
            if !["vless", "vmess", "trojan", "shadowsocks"]
                .contains(&protocol.to_ascii_lowercase().as_str())
            {
                return Err(format!("неподдерживаемый фильтр --protocol: {protocol}"));
            }
        }
        if self.country.iter().any(|country| country.trim().is_empty()) {
            return Err("--country не может быть пустым".into());
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::Cli;
    use clap::Parser;

    fn defaults() -> Cli {
        Cli::try_parse_from(["proxy-rs"]).unwrap()
    }

    #[test]
    fn defaults_are_valid_and_use_platform_core_name() {
        let cli = defaults();
        assert!(cli.validate().is_ok());
        assert_eq!(cli.core, crate::xray::core_bin_name());
    }

    #[test]
    fn rejects_non_finite_and_out_of_range_timeouts() {
        for (field, min) in [
            ("fetch", 1.0),
            ("core", 1.0),
            ("tcp", 0.1),
            ("speed", 1.0),
            ("speed-connect", 1.0),
        ] {
            for value in [f64::NAN, f64::INFINITY, min - 0.01, 3600.1] {
                let mut cli = defaults();
                let option = match field {
                    "fetch" => {
                        cli.fetch_timeout = value;
                        "--fetch-timeout"
                    }
                    "core" => {
                        cli.core_start_timeout = value;
                        "--core-start-timeout"
                    }
                    "tcp" => {
                        cli.tcp_timeout = value;
                        "--tcp-timeout"
                    }
                    "speed" => {
                        cli.speed_timeout = value;
                        "--speed-timeout"
                    }
                    _ => {
                        cli.speed_connect_timeout = value;
                        "--speed-connect-timeout"
                    }
                };
                assert!(cli.validate().unwrap_err().contains(option));
            }
        }
    }

    #[test]
    fn rejects_invalid_speed_limits() {
        for value in [f64::NAN, f64::INFINITY, 0.49, -1.0, 1024.1] {
            let mut cli = defaults();
            cli.speed_max_mb = value;
            assert!(cli.validate().unwrap_err().contains("--speed-max-mb"));
        }
        let mut cli = defaults();
        cli.min_kb = f64::NAN;
        assert!(cli.validate().unwrap_err().contains("--min-kb"));
    }

    #[test]
    fn rejects_zero_or_excessive_concurrency_and_batches() {
        let mut cli = defaults();
        cli.fetch_concurrency = 0;
        assert!(cli.validate().unwrap_err().contains("--fetch-concurrency"));
        let mut cli = defaults();
        cli.batch = 10001;
        assert!(cli.validate().unwrap_err().contains("--batch"));
    }

    #[test]
    fn validates_port_retries_and_limit() {
        let mut cli = defaults();
        cli.lport = 1023;
        assert!(cli.validate().unwrap_err().contains("--lport"));
        let mut cli = defaults();
        cli.tcp_retries = 0;
        assert!(cli.validate().unwrap_err().contains("--tcp-retries"));
        let mut cli = defaults();
        cli.limit = Some(0);
        assert!(cli.validate().unwrap_err().contains("--limit"));
    }

    #[test]
    fn rejects_invalid_fetch_options_resume_filters_and_empty_countries() {
        let mut cli = defaults();
        cli.fetch_retries = 9;
        assert!(cli.validate().unwrap_err().contains("--fetch-retries"));

        for value in [1023, 1_073_741_825] {
            let mut cli = defaults();
            cli.max_source_bytes = value;
            assert!(cli.validate().unwrap_err().contains("--max-source-bytes"));
        }

        let mut cli = defaults();
        cli.resume = true;
        assert!(cli.validate().unwrap_err().contains("--resume"));

        let mut cli = defaults();
        cli.protocol = vec!["http".into()];
        assert!(cli.validate().unwrap_err().contains("--protocol"));

        let mut cli = defaults();
        cli.protocol = vec!["VLESS".into(), "Shadowsocks".into()];
        cli.country = vec!["DE".into(), "  ".into()];
        assert!(cli.validate().unwrap_err().contains("--country"));
    }

    #[test]
    fn accepts_lower_fetch_boundaries_and_supported_case_insensitive_protocols() {
        let mut cli = defaults();
        cli.fetch_retries = 0;
        cli.max_source_bytes = 1024;
        cli.lport = 1024;
        cli.protocol = vec!["VLESS".into(), "Shadowsocks".into()];
        cli.country = vec!["DE".into()];
        assert!(cli.validate().is_ok());
    }

    #[test]
    fn accepts_resume_when_checkpoint_path_is_supplied() {
        let cli = Cli::try_parse_from([
            "proxy-rs",
            "--resume",
            "--checkpoint",
            "run.checkpoint.json",
        ])
        .unwrap();

        assert!(cli.validate().is_ok());
        assert_eq!(cli.checkpoint.as_deref(), Some("run.checkpoint.json"));
    }

    #[test]
    fn accepts_documented_upper_boundaries() {
        let mut cli = defaults();
        cli.fetch_timeout = 3600.0;
        cli.tcp_timeout = 3600.0;
        cli.speed_timeout = 3600.0;
        cli.speed_connect_timeout = 3600.0;
        cli.core_start_timeout = 3600.0;
        cli.speed_max_mb = 1024.0;
        cli.lport = 65000;
        cli.tcp_retries = 100;
        assert!(cli.validate().is_ok());
    }
}
