# История изменений · Changelog · 更新日志

## v0.1.0 — первый выпуск / Initial release / 首个版本

### Русский

- Первый публичный выпуск `proxy-rs` — кроссплатформенного CLI на Rust для сбора и проверки прокси.
- Загрузка подписок и локальных списков; разбор VLESS, VMess, Trojan, Shadowsocks, Hysteria 1 и Hysteria 2.
- GeoIP-разметка, TCP-предфильтр и проверка скорости через Xray или checksum-проверенное ядро Hysteria 1; UDP-протоколы не получают ложный TCP ping.
- Генерация Clash/mihomo для поддерживаемых вариантов всех шести протоколов. Варианты, которые нельзя проверить выбранным ядром, отмечаются как непроверенные, а не как нерабочие.
- Генерация конфигураций Clash/mihomo в отдельном crate `proxy-clash`.
- Атомарная запись промежуточных и итоговых файлов.
- Проверка TLS-сертификатов по умолчанию, запрет HTTPS→HTTP редиректов, лимит тела подписки, повторы и условный кэш с резервным использованием последнего ответа.
- Обработка Ctrl-C с остановкой задач и Xray, контрольные точки с возобновлением, отчёт качества источников и JSON-вывод без секретов.
- Фильтры по протоколу/стране и режим вывода только новых прокси; сборочные скрипты для Bash и Windows CMD с выбором платформы.
- Главная страница и полные руководства на русском, английском и китайском языках.
- Релизные архивы для Linux, macOS и Windows формируются GitHub Actions; публикация создаётся как черновик для ручной проверки.

### English

- First public release of `proxy-rs`, a cross-platform Rust CLI for collecting and checking proxies.
- Fetch subscriptions and read local lists; parse VLESS, VMess, Trojan, and Shadowsocks links.
- GeoIP labeling, a TCP prefilter, and Xray-based speed checks.
- Parse VLESS, VMess, Trojan, Shadowsocks, Hysteria 1, and Hysteria 2 subscriptions and local lists.
- Measure speed with Xray or a checksum-verified Hysteria 1 core; UDP protocols skip the TCP ping prefilter.
- Generate Clash/mihomo configuration for supported variants of all six protocol families. Variants that the selected speed-check core cannot handle are reported as unchecked, not dead.
- Write intermediate and final files atomically.
- Verify TLS certificates by default, reject HTTPS-to-HTTP redirects, cap subscription bodies, retry temporary failures, and conditionally cache responses with stale fallback.
- Handle Ctrl-C by stopping tasks and Xray, resume from checkpoints, report source quality, and emit a secret-safe JSON summary.
- Filter by protocol/country, output only new proxies, and build with Bash or Windows CMD scripts for a selected platform.
- Landing pages and full manuals in Russian, English, and Chinese.
- GitHub Actions builds release archives for Linux, macOS, and Windows and opens a draft release for manual review.

### 简体中文

- `proxy-rs` 首个公开版本：用于收集和检测代理的跨平台 Rust 命令行工具。
- 下载订阅并读取本地列表；解析 VLESS、VMess、Trojan、Shadowsocks、Hysteria 1 和 Hysteria 2 链接。
- 提供 GeoIP 标记、TCP 预筛选，以及基于 Xray 或经过校验和验证的 Hysteria 1 核心的速度检测；UDP 协议会跳过 TCP ping。
- 为六类协议中受支持的变体生成 Clash/mihomo 配置。所选测速核心无法检测的变体会标记为“未检测”，而不是“不可用”。
- 以原子方式写入中间结果和最终文件。
- 默认验证 TLS 证书，拒绝 HTTPS 降级到 HTTP 的重定向，限制订阅响应大小，并支持临时错误重试、条件缓存和过期缓存回退。
- Ctrl-C 时停止任务和 Xray；支持检查点续跑、来源质量报告以及不包含敏感连接信息的 JSON 摘要。
- 支持按协议/国家筛选、仅输出新增代理，并提供 Bash 和 Windows CMD 多平台构建脚本。
- 提供俄语、英语和中文的项目主页与完整手册。
- GitHub Actions 为 Linux、macOS 和 Windows 构建发布压缩包，并创建供人工检查的草稿 Release。
