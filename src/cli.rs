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
    #[arg(long = "fetch-concurrency", default_value_t = 32, help_heading = "Источники")]
    pub fetch_concurrency: usize,

    /// Таймаут загрузки источника, сек
    #[arg(long = "fetch-timeout", default_value_t = 15.0, help_heading = "Источники")]
    pub fetch_timeout: f64,

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

    /// Проверять только первые N прокси
    #[arg(short = 'n', long = "limit", value_name = "N", help_heading = "Вывод")]
    pub limit: Option<usize>,

    /// Перемешать список перед проверкой
    #[arg(short = 's', long, help_heading = "Вывод")]
    pub shuffle: bool,

    /// Сырые тексты загруженных источников (сохраняется после загрузки)
    #[arg(long = "raw-output", default_value = "raw.txt", help_heading = "Вывод")]
    pub raw_output: String,

    /// Уникальные прокси до проверок (сохраняется после парсинга и GeoIP)
    #[arg(long = "parsed-output", default_value = "parsed.txt", help_heading = "Вывод")]
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
    #[arg(long, default_value = "xray.exe", help_heading = "Ядро (xray)")]
    pub core: String,

    /// Рабочая папка для батч-конфигов
    #[arg(long, default_value = ".proxy_runtime", help_heading = "Ядро (xray)")]
    pub workdir: String,

    /// Прокси в одном батче xray
    #[arg(long, default_value_t = 100, help_heading = "Ядро (xray)")]
    pub batch: usize,

    /// Базовый локальный порт для socks-inbound'ов
    #[arg(long = "lport", default_value_t = 10000, help_heading = "Ядро (xray)")]
    pub lport: u16,

    /// Сколько батчей проверять параллельно
    #[arg(long, default_value_t = 8, help_heading = "Ядро (xray)")]
    pub batch_workers: usize,

    /// Сколько секунд ждать старт ядра
    #[arg(long = "core-start-timeout", default_value_t = 8.0, help_heading = "Ядро (xray)")]
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

    /// Таймаут TCP-коннекта, сек
    #[arg(long = "tcp-timeout", default_value_t = 3.0, help_heading = "TCP ping (префильтр)")]
    pub tcp_timeout: f64,

    /// Параллельных TCP-проб
    #[arg(long = "tcp-concurrency", default_value_t = 1000, help_heading = "TCP ping (префильтр)")]
    pub tcp_concurrency: usize,

    /// Попыток TCP-пробы на адрес
    #[arg(long = "tcp-retries", default_value_t = 1, help_heading = "TCP ping (префильтр)")]
    pub tcp_retries: u32,

    /// Максимальный TCP RTT, мс (0 = не фильтровать)
    #[arg(long = "tcp-max-ms", default_value_t = 0, help_heading = "TCP ping (префильтр)")]
    pub tcp_max_ms: u64,

    /// Отбрасывать живые с TCP ping выше этого (0 = выключено)
    #[arg(long = "max-ping", default_value_t = 0, help_heading = "TCP ping (префильтр)")]
    pub max_ping: u64,

    // ------------------------ Проверка скорости ------------------------
    /// Выключить проверку скорости
    #[arg(long = "no-speed", help_heading = "Проверка скорости")]
    pub no_speed: bool,

    /// Приоритетный URL для замера скорости
    #[arg(long = "speed-url", value_name = "URL", help_heading = "Проверка скорости")]
    pub speed_url: Option<String>,

    /// Таймаут скачивания при замере скорости, сек
    #[arg(long = "speed-timeout", default_value_t = 6.0, help_heading = "Проверка скорости")]
    pub speed_timeout: f64,

    /// Таймаут соединения при замере скорости, сек
    #[arg(long = "speed-connect-timeout", default_value_t = 3.0, help_heading = "Проверка скорости")]
    pub speed_connect_timeout: f64,

    /// Максимум скачиваемого МБ на замер
    #[arg(long = "speed-max-mb", default_value_t = 5.0, help_heading = "Проверка скорости")]
    pub speed_max_mb: f64,

    /// Минимальная скорость, KB/s (меньше = мусор)
    #[arg(long = "min-kb", default_value_t = 100.0, help_heading = "Проверка скорости")]
    pub min_kb: f64,

    /// Параллельных замеров скорости
    #[arg(long = "speed-threads", default_value_t = 16, help_heading = "Проверка скорости")]
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
