# История изменений · Changelog · 更新日志

## v0.1.2 — Стабильный релизный CI / Stable release CI / 稳定发布 CI

### 🛠 Исправления · Fixes · 修复

- **RU:** Релизный CI запускает workspace-тесты последовательно, чтобы Unix-тесты с локальными сетевыми fixture не конфликтовали друг с другом. Для каждой платформенной сборки установлен предел времени в 30 минут.
- **EN:** Release CI runs workspace tests sequentially so Unix tests using local network fixtures cannot interfere with each other. Each platform build now has a 30-minute timeout.
- **中文：** 发布 CI 现在按顺序运行 workspace 测试，避免使用本地网络 fixture 的 Unix 测试互相干扰。每个平台构建任务均设置了 30 分钟超时。

## v0.1.1 — Исправление сборки Windows / Windows packaging fix / Windows 打包修复

### 🛠 Исправления · Fixes · 修复

- **RU:** Исправлено формирование Windows-архива: вывод Rust-инструментов теперь читается как UTF-8 независимо от системной кодовой страницы. Добавлен тест упаковщика в релизный CI.
- **EN:** Fixed Windows archive packaging by decoding Rust tool output as UTF-8 regardless of the system code page. Added a packaging-helper test to release CI.
- **中文：** 修复了 Windows 压缩包构建问题：Rust 工具输出现在始终按 UTF-8 解码，不受系统代码页影响。发布 CI 中新增了打包辅助脚本测试。

## v0.1.0 — Первый публичный выпуск / First public release / 首个公开版本

Первый выпуск `proxy-rs` — кроссплатформенной Rust CLI для сбора, проверки и удобного экспорта прокси.

### ✨ Возможности · Русский

- Загружайте подписки и читайте локальные списки; разбирайте VLESS, VMess, Trojan, Shadowsocks, Hysteria 1 и Hysteria 2.
- Проверяйте доступность и скорость через подходящее ядро. Для UDP-протоколов программа не подменяет проверку TCP ping; варианты, которые нельзя проверить, отмечаются как непроверенные.
- Получайте GeoIP-метки, фильтруйте прокси по протоколу и стране, выбирайте только новые записи.
- Создавайте конфигурации Clash/mihomo и sing-box; отчёты объясняют, какие узлы и параметры не удалось экспортировать.
- Смотрите подробные отчёты по источникам и пробам скорости, диагностируйте отдельный узел командой `--check-one`.
- Используйте повторные попытки, кэш HTTP validators, безопасные лимиты загрузки и checkpoint для продолжения прерванного прогона.
- Собирайте проект скриптами Bash и Windows CMD. Полная документация доступна на русском, английском и китайском.

### ✨ Highlights · English

- Fetch subscriptions and read local lists. Parse VLESS, VMess, Trojan, Shadowsocks, Hysteria 1, and Hysteria 2 links.
- Check connectivity and speed with the applicable core. UDP protocols do not receive a misleading TCP ping result; variants the selected core cannot check are reported as unchecked.
- Add GeoIP labels, filter by protocol or country, and keep only newly discovered proxies.
- Export Clash/mihomo and sing-box configurations, with reports explaining skipped nodes and unsupported settings.
- Inspect per-source quality and individual speed probes, or diagnose one proxy with `--check-one`.
- Use bounded downloads, TLS verification, retries, HTTP validator caching, and checkpoints to resume interrupted runs.
- Build with Bash or Windows CMD scripts. Full documentation is available in Russian, English, and Chinese.

### ✨ 功能亮点 · 简体中文

- 下载订阅并读取本地列表，解析 VLESS、VMess、Trojan、Shadowsocks、Hysteria 1 和 Hysteria 2 链接。
- 使用适用的核心检查连通性和速度。UDP 协议不会显示误导性的 TCP ping 结果；所选核心无法检测的变体会标记为“未检测”。
- 添加 GeoIP 信息，按协议或国家筛选，并仅保留新发现的代理。
- 导出 Clash/mihomo 和 sing-box 配置；报告会说明被跳过的节点和不支持的设置。
- 查看来源质量和每次测速的详细结果，也可使用 `--check-one` 单独诊断代理。
- 提供 TLS 证书验证、下载大小限制、重试、HTTP validator 缓存，以及中断后续跑所需的 checkpoint。
- 使用 Bash 或 Windows CMD 脚本构建。完整文档提供俄语、英语和中文版本。

### 📦 Загрузки · Downloads · 下载

Release workflow проверяет workspace-тесты и собирает архивы для Linux, macOS и Windows. В архив входят бинарный файл, каталог источников, лицензия, changelog и документация на трёх языках. После сборки GitHub Actions прикладывает архивы и `SHA256SUMS` к черновику релиза.

The release workflow runs workspace tests and builds archives for Linux, macOS, and Windows. Each archive includes the binary, source catalog, license, changelog, and documentation in all three languages. GitHub Actions attaches the archives and `SHA256SUMS` to the draft release.

Release workflow 会运行 workspace 测试，并为 Linux、macOS 和 Windows 构建压缩包。每个压缩包包含二进制文件、来源目录、许可证、更新日志和三种语言的文档。GitHub Actions 会将压缩包和 `SHA256SUMS` 添加到 Release 草稿中。
