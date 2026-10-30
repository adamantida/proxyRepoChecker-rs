<div align="center">

# proxy-rs

**Найдите рабочие прокси без лишней рутины.**

`proxy-rs` собирает прокси из подписок, проверяет доступность и скорость, а затем помогает подготовить конфигурацию для Clash/mihomo. Работает на Windows, macOS и Linux.

[![CI checks](https://img.shields.io/badge/CI-manual%20checks-blue?logo=githubactions&logoColor=white)](https://github.com/tnAnGel/proxyRepoChecker-rs/actions/workflows/cross-platform.yml)
![Rust 2021](https://img.shields.io/badge/Rust-2021-orange)
![Windows · macOS · Linux](https://img.shields.io/badge/platform-Windows%20%7C%20macOS%20%7C%20Linux-64748b)

[Русский](README.md) · [English](README.en.md) · [简体中文](README.zh-CN.md)

🚀 [Быстрый запуск](#quick-start) · 🌐 [Готовые результаты](#published-results) · 📚 [Документация](#documentation)

</div>

## ✨ Что умеет `proxy-rs`

```mermaid
flowchart LR
    A[Подписки и файлы] --> B[Загрузка]
    B --> C[Парсинг и дедупликация]
    C --> D[GeoIP и имена]
    D --> E[TCP-проверка для TCP-прокси]
    E --> F[Замер скорости подходящим ядром]
    F --> G[Текстовые результаты]
    F --> H[Clash / mihomo]
```

- Разбирает `vless`, `vmess`, `trojan`, `ss` (Shadowsocks), Hysteria 1 и Hysteria 2; проверка скорости и экспорт зависят от возможностей соответствующего ядра.
- Поддерживает локальные файлы, отдельные URL подписок и наборы источников из JSON.
- Сохраняет промежуточные результаты атомарно, чтобы их можно было изучить после каждого этапа.
- Генерирует Clash/mihomo из прокси, прошедших порог скорости.
- Генерирует sing-box JSON с отдельным отчётом о неподдерживаемых параметрах; замеряет скорость по нескольким адресам и может показать медиану и разброс.
- Диагностирует одну ссылку через `--check-one FILE`, включая запуск ядра, SOCKS-подключение и загрузку.
- Собирается на Windows, macOS и Linux; Xray подбирается под ОС и архитектуру.

<a id="quick-start"></a>

## 🚀 Быстрый запуск

Нужны Rust stable и Cargo. Склонируйте репозиторий и выполните команды из его корня:

```sh
cargo build --workspace --release
cargo run --release -- --set 3 -n 200 --shuffle-seed 20261028
```

Вторая команда запускает короткую проверку на небольшом наборе источников (до 200 прокси). Подробную справку можно открыть так:

```sh
cargo run -- --help
```

Для оценки стабильности замера и экспорта в sing-box можно запустить:

```sh
proxy-rs --file proxies.txt --speed-probes 5 --speed-report speed.json \
  --singbox sing-box.json --singbox-report sing-box-skipped.json --singbox-check
proxy-rs --check-one one-proxy.txt
```

`--singbox-check` валидирует созданный конфиг установленным sing-box (`sing-box check`). Если бинарник не находится в `PATH`, укажите его через `--singbox-bin FILE`. Отчёт скорости содержит результат каждой цели с очищенным URL, а отчёт источников показывает по каждой подписке причины ошибок и числа уникальных, TCP-живых и прошедших порог прокси.

### 🛠 Сборка скриптом

`scripts/build.sh` предназначен для Bash на macOS/Linux, `scripts/build.bat` — для Windows `cmd.exe`. Оба собирают текущую платформу по умолчанию или выбранную платформу/архитектуру:

```sh
# macOS/Linux: текущая платформа, Linux ARM64 или все платформы для x86_64
scripts/build.sh
scripts/build.sh linux --arch aarch64 --install-target
scripts/build.sh all --arch x86_64
```

```bat
rem Windows cmd.exe: Windows x86_64 или все платформы для ARM64
scripts\build.bat windows --arch x86_64 --install-target
scripts\build.bat all --arch aarch64 --install-target
```

`--install-target` устанавливает только Rust target через rustup. Для cross-compile дополнительно нужны подходящие linker и системные SDK: Windows GNU требует совместимый MinGW linker, Windows ARM64 — MSVC toolchain, а сборка macOS — Apple SDK/toolchain. Скрипт на одной ОС не может сам предоставить эти инструменты; не-native сборки могут потребовать настройки Cargo linker. Нативный бинарник находится в `target/release/`, cross-build — в `target/<rust-target>/release/`.

### 🔒 Безопасная загрузка и возобновление

TLS-сертификаты подписок проверяются стандартным способом. По умолчанию ответ ограничен 16 MiB на источник, выполняются две повторные попытки с задержкой; кэш `.proxy_cache` использует `ETag`/`Last-Modified` и может отдать последний успешный ответ при временной ошибке. Каталог меняется через `--fetch-cache-dir DIR`, кэш отключается через `--no-fetch-cache`; `--insecure-source-tls` отключает проверку сертификата и применять его следует только осознанно.

```sh
proxy-rs --url 'https://example.org/sub' --fetch-retries 4 --max-source-bytes 33554432 \
  --source-report source-quality.json --checkpoint run.checkpoint
# После остановки/сбоя продолжить с сохранённого этапа:
proxy-rs --resume --checkpoint run.checkpoint
```

`--resume` требует `--checkpoint FILE`. JSON-отчёт содержит статусы, этапные счётчики и точные причины неподдержки по каждой подписке; URL в нём очищаются от учётных данных, query, fragment и пути. Checkpoint хранит ссылки и результаты для продолжения, поэтому держите его как приватный файл.

### 🎛️ Фильтры и JSON-результаты

`--protocol` и `--country` оставляют выбранные протоколы и страны (ISO-коды или GeoIP-теги); параметры можно повторять или перечислять через запятую. `--only-new` разбирает ссылки из существующего файла `--output` и удаляет прокси с уже известным outbound-ключом. `--json-output FILE` дополнительно записывает итоговые прокси в JSON-массив с полями `protocol`, `server`, `port`, `name`, `country`, `display`, `ping_ms` и `speed_kbs`; URI, пароли, UUID и настройки подключений в JSON не попадают.

```sh
cargo run --release -- --url 'https://example.org/sub' \
  --protocol vless,vmess --country US,DE --only-new \
  --json-output new-proxies.json
```

## 📂 Файлы результатов

| Файл | Содержимое |
|---|---|
| `raw.txt` | Тексты, загруженные из подписок |
| `parsed.txt` | Уникальные разобранные ссылки; после GeoIP — с отображаемыми именами |
| `alive.txt` | Итоговый список после включённых проверок; при отключённой TCP-проверке и скорости это ещё не проверенные кандидаты |
| `good.txt` | Прокси, прошедшие порог `--min-kb` |
| `dead.txt` | Отброшенные фильтрами или скоростной проверкой узлы, если задан `--dead` |
| `clash.yaml` | Конфиг Clash/mihomo, если задан `--clash` и есть подходящие прокси |
| `sing-box.json` | Конфиг sing-box, если задан `--singbox` и есть подходящие прокси |

Пути меняются флагами CLI. Файлы результатов и локальные логи исключены из Git.

<a id="documentation"></a>

## 📚 Документация

- [Полное руководство на русском](docs/ru/README.md)
- [Full documentation in English](docs/en/README.md)
- [简体中文完整文档](docs/zh-CN/README.md)
- [История изменений и релизов / Changelog / 更新日志](CHANGELOG.md)

В руководствах описаны все аргументы CLI, входные данные, этапы обработки, форматы результатов, Xray, GeoIP, Clash/mihomo, архитектура, тестирование и CI.

## 👩‍💻 Разработка

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo test --workspace
cargo build --workspace --release
cargo bench --bench pipeline
```

`cargo test --workspace` также запускает детерминированный fuzz-style корпус повреждённых URI и GeoIP protobuf-данных. Это тестирование на фиксированном seeded-корпусе, а не отдельный coverage/fuzzing runtime.

Проверка сборки и тестов на Ubuntu, macOS и Windows запускается вручную через workflow `cross-platform`. Автоматически по расписанию работает только сборщик прокси.

## 📦 Выпуски

Push тега вида `vX.Y.Z`, совпадающего с версией в `Cargo.toml`, запускает сборку архивов для Linux, macOS и Windows. GitHub Actions прикладывает SHA-256 суммы и создаёт **черновик** Release с документацией на трёх языках; перед публикацией его можно проверить и отредактировать.

<a id="published-results"></a>

## 🌐 Готовые результаты

Можно сразу открыть RAW-файл или скопировать ссылку в приложение:

| Что нужно | Файл |
|---|---|
| Готовый конфиг для Clash/mihomo | [📥 clash.yaml](https://raw.githubusercontent.com/tnAnGel/proxyRepoChecker-rs/dist/clash.yaml) |
| Прокси, прошедшие проверку скорости | [⚡ good.txt](https://raw.githubusercontent.com/tnAnGel/proxyRepoChecker-rs/dist/good.txt) |
| Все прокси после включённых проверок | [✅ alive.txt](https://raw.githubusercontent.com/tnAnGel/proxyRepoChecker-rs/dist/alive.txt) |
| Прокси, отклонённые проверками | [🧾 dead.txt](https://raw.githubusercontent.com/tnAnGel/proxyRepoChecker-rs/dist/dead.txt) |

Публикация запускается каждые 30 минут — в **11 и 41 минуту UTC** — и вручную через [GitHub Actions](https://github.com/tnAnGel/proxyRepoChecker-rs/actions/workflows/proxy-collection.yml). Новые файлы появляются после успешного завершения проверки; если запуск завершится ошибкой, текущие ссылки продолжат работать с предыдущими результатами. GitHub может немного задержать запуск при высокой нагрузке.

## 💬 uSphere VPN

Нужен готовый VPN-сервис для повседневного использования? Попробуйте **uSphere VPN** — платный сервис, который мы рекомендуем за качество обслуживания. Откройте Telegram-бота, ознакомьтесь с условиями и выберите подходящий вариант:

[Перейти к uSphere VPN в Telegram →](https://t.me/uSphereVPN_bot)

## 📄 Лицензия

Проект распространяется по лицензии [MIT](LICENSE).
