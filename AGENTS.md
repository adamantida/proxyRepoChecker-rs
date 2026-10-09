# AGENTS.md

## Проект
`proxy-rs` — Rust CLI (Windows): парсинг прокси из агрегаторов → GeoIP-переименование
(`[XX] - NNN`) → TCP ping (префильтр) → замер скорости через xray → выгрузка файлов.
Генерация Clash/mihomo конфига вынесена в отдельный crate `proxy-clash` (workspace-member).

## Команды
- Сборка: `cargo build --release` (бинарник `target/release/proxy-rs.exe`)
- Все сборки должны давать **0 warnings**
- Тесты генератора: `cargo test -p proxy-clash`
- Помощь: `proxy-rs --help`

## Правило тестирования
- Все проверочные прогоны — **только** `--set 3 -n 200` (быстро, интернет-набор).
- Полный прогон (`--set all`/большие лимиты) — только по прямой команде пользователя.
- `--set 3` даёт ~10k ссылок / ~200 после `-n`; `--shuffle` резко повышает шанс живых.

## Workspace
- Корневой `Cargo.toml` — и `[workspace]` (members = `["proxy-clash"]`, `resolver = "2"`),
  и `[package] proxy-rs`. `build.rs` копирует `bin/sources.json` в профиль сборки.
- `proxy-clash/` — чистая библиотека (`proxy_clash`): `Source` (proto/name/server/port/
  country + xray `settings`/`stream` как `serde_json::Value`) → `render()` → YAML + `Report`.
  Никакого I/O; зависимость `serde_yaml_ng` живёт только здесь.

## Clash-генерация
- Флаги (`src/cli.rs`, раздел «Вывод»): `--clash [FILE]` (без значения = `clash.yaml`),
  `--clash-no-country`, `--clash-no-categories`.
- Источник: `good` (прошли порог `--min-kb`). Если `good` пуст — генерация пропускается.
- Встраивание: `src/main.rs`, блок «8. Clash/mihomo конфиг», после расчёта `good`;
  запись атомарная через `output::write_text_atomic`.
- Маппинг xray→Clash: `vless|vmess|trojan|shadowsocks`; сети `raw/tcp`, `ws`, `grpc`,
  `httpupgrade` (как `ws`+`v2ray-http-upgrade`), `xhttp`, `kcp` (best-effort).
  Reality: `reality-opts.public-key` = xray `realitySettings.password`.
  **trojan** → ключ `sni`; **vmess/vless** → `servername`. Неизвестный proto — `skipped`.
- Группы: `🚀 Прокси`(select), `⚡ Авто`(url-test), `⚖️ Баланс`(load-balance),
  `🌍 Страны`(select — ручная группа со всеми странами), `🔓 Заблокированные`(select),
  страны `🇽🇽 XX`(url-test), `🌐 Прочие` (XX/неопределённые). Псевдо-страны из geoip.dat
  (типа `CLOUDFLARE` → `☁️`, `RU-BLOCKED` → `🔒`, прочие теги → `🏳️`) получают
  собственные группы наравне со странами. Категории: `💬 Telegram`/`🤖 AI`/`📺 YouTube`/`🎮 Игры`.
- Правила и глобальные секции (dns/sniffer/profile/…) — `proxy-clash/src/rules.rs`.

## Пайплайн и файлы
- `fetch → parse+dedup → GeoIP → TCP ping → xray speed → outputs`.
- `raw.txt`, `parsed.txt`, `alive.txt`, `dead.txt`, `good.txt` пишутся **после каждого этапа**
  (атомарно: tmp+rename); `good.txt` обновляется по завершённым батчам.
- `alive` = прошедшие `--min-kb` (при `--no-speed` — только TCP-живые, `good.txt` не пишется).
- Автозагрузка: xray (`XTLS/Xray-core`, zip в памяти) и `geoip.dat` (protobuf, свой парсер
  в `src/geoip.rs`) — рядом с exe. Отключение: `--no-xray-download` (xray), `--no-geoip`.

## Квирки
- **Кириллица**: НЕ редактировать исходники через PowerShell `Get-Content`/`Set-Content`
  (портит UTF-8, добавляет BOM). Только инструменты edit/write.
- Прогресс-бары (`indicatif`) видны только в реальном TTY; в файл/через pipe их нет.
- Файлы валидный UTF-8; в PowerShell-консоли с ограниченной кодировкой видны как mojibake —
  это артефакт вывода, не порча файла.
- `.proxy_runtime/` — временная папка xray (батч-конфиги); `target/release/{xray.exe,geoip.dat}`
  — кэш рантайма.
