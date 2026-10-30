# proxy-rs

**Collect, parse, and test proxy subscriptions from the command line.** `proxy-rs` turns subscription URLs and local proxy links into a deduplicated list, optionally looks up countries, checks TCP reachability where applicable and measures download speed through Xray or the dedicated Hysteria 1 core, and can generate a Clash/mihomo configuration from proxies that pass the speed threshold.

The repository is a Rust workspace. The root package contains the CLI; [`proxy-clash`](../../proxy-clash) renders Clash/mihomo YAML, and [`proxy-singbox`](../../proxy-singbox) converts supported nodes to sing-box JSON with a conversion report.

> The CLI help and diagnostics are currently in Russian. Run `proxy-rs --help` (or `cargo run -- --help`) for the authoritative options available in the binary you built.

## Contents

- [Features](#features)
- [Requirements and installation](#requirements-and-installation)
- [Quick start](#quick-start)
- [Inputs and source sets](#inputs-and-source-sets)
- [Processing pipeline and result semantics](#processing-pipeline-and-result-semantics)
- [Command-line reference](#command-line-reference)
- [Files and paths](#files-and-paths)
- [Xray and GeoIP data](#xray-and-geoip-data)
- [Clash/mihomo generation](#clashmihomo-generation)
- [Workspace architecture](#workspace-architecture)
- [Development, tests, and CI](#development-tests-and-ci)
- [Troubleshooting](#troubleshooting)

## Features

- Read subscription URLs from a JSON source catalog, add URLs on the command line, and read direct proxy links or more URLs from a text file.
- Extract and parse VLESS, VMess, Trojan, Shadowsocks (`ss`), Hysteria 1, and Hysteria 2 links; supported transports and speed-check eligibility depend on the selected core.
- Optionally map server IP addresses to countries using a local `geoip.dat` database.
- Apply a TCP prefilter where applicable, then measure speed through Xray or the dedicated Hysteria 1 core.
- Write intermediate and final result files using atomic replacement.
- Render a Clash/mihomo YAML configuration from speed-qualified proxies.
- Run on Windows, macOS, and Linux. Automatic Xray downloads are provided for x86_64 and aarch64 on those operating systems.

## Requirements and installation

### Build from source

Install a current stable Rust toolchain with Cargo, then build the workspace:

```sh
git clone <repository-url>
cd proxyRepoChecker-rs
cargo build --release --workspace
```

The CLI binary is `target/release/proxy-rs` on Unix-like systems and `target/release/proxy-rs.exe` on Windows. Cargo builds the bundled source catalog from `bin/sources.json` into the build profile.

To check the installed command-line interface:

```sh
cargo run -- --help
```

#### Build scripts and cross-compilation

Run the Bash script on macOS/Linux and the BAT script in Windows `cmd.exe`. With no platform argument they build for the host; choose `linux`, `macos`, `windows`, or `all`, and `x86_64` or `aarch64`:

```sh
scripts/build.sh                         # host platform
scripts/build.sh linux --arch aarch64 --install-target
scripts/build.sh all --arch x86_64
```

```bat
scripts\build.bat windows --arch x86_64 --install-target
scripts\build.bat all --arch aarch64 --install-target
```

`--install-target` uses rustup to install the Rust target only; it does not install a linker or SDK. Windows GNU needs a compatible MinGW linker, Windows ARM64 needs the MSVC toolchain, and macOS builds need the Apple SDK/toolchain. Cross-builds may need Cargo linker configuration. Native output is in `target/release/`; cross-target output is in `target/<rust-target>/release/`. The scripts continue with other requested targets after a failure and return non-zero if any build fails.

### Runtime requirements

- A supported OS and Rust target. The CI matrix builds and tests on Ubuntu, macOS, and Windows.
- Network access when fetching remote subscriptions or automatically downloading Xray or GeoIP data. Alternatively, supply local proxy links and local runtime/database files.
- Xray is required for speed checks. It is not required when using `--no-speed`.
- GeoIP is optional; disable its lookup with `--no-geoip`.

The HTTP client uses Rustls, so the project does not require system OpenSSL for its normal build.

## Quick start

Run the default source set, shuffle the proxies, and check at most 200 of them:

```sh
cargo run --release -- --set 3 --shuffle --limit 200
```

Use a subscription URL directly:

```sh
proxy-rs --url https://example.com/subscription --min-kb 500 --limit 100
```

Check direct links from a file without GeoIP, TCP prefiltering, or Xray speed tests:

```sh
proxy-rs --file links.txt --no-geoip --no-tcp-ping --no-speed
```

Generate a Clash/mihomo configuration from proxies that pass a 500 KB/s threshold:

```sh
proxy-rs --set 3 --shuffle-seed 42 --limit 200 --min-kb 500 --clash clash.yaml
```

These examples that use remote sources perform real network requests when run. A local-only smoke test can use a file containing a syntactically valid proxy URI together with `--no-geoip --no-tcp-ping --no-speed`.

## Inputs and source sets

### `sources.json`

The default catalog is `sources.json`. It is a JSON object whose keys are source-set names and whose values are arrays of subscription URL strings:

```json
{
  "1": ["https://example.com/subscriptions/one"],
  "2": ["https://example.com/subscriptions/two"],
  "3": ["https://example.com/subscriptions/three"]
}
```

`--set` selects one key; `--set all` selects every key present in the JSON object. The built-in catalog currently defines sets `1`, `2`, and `3`. A selected key must exist and contain an array. Non-string array entries are ignored. Duplicate subscription URLs are removed while preserving their first-seen order.

Use `--sources FILE` to select a different catalog. If the default catalog is not found, the program can still run when you provide `--url` or `--file` explicitly.

### Additional URLs and link files

- Repeat `--url URL` to add subscription URLs in addition to the selected catalog set.
- `--file FILE` reads UTF-8 text, one entry per line. Blank lines and lines beginning with `#` are ignored.
- Lines beginning with `http://` or `https://` are treated as subscription URLs and fetched.
- Other lines containing `://` are treated as direct proxy links and parsed locally.
- Other lines are ignored.

Subscription URLs from the catalog, `--url`, and `--file` are deduplicated. Direct links from a file are appended to the links extracted from fetched subscriptions.

The parser supports `vless://`, `vmess://` (including Base64-encoded VMess JSON), `trojan://`, `ss://`, `hysteria://` (Hysteria 1), and `hysteria2://`/`hy2://` (Hysteria 2). SS links with a `plugin` parameter are explicitly rejected so the app never checks a connection after silently dropping its required plugin. `ssr://`, `tuic://`, and `anytls://` remain unsupported; malformed links are counted separately from unsupported protocols.

For Shadowsocks, the parser accepts SIP002 `method:password@host:port` links (including Base64URL userinfo) and the legacy Base64-encoded `method:password@host:port` form. Invalid separators are reported separately; nonstandard cipher labels are summarized as `method=nonstandard` without exposing their original value. See the [Shadowsocks SIP002 specification](https://github.com/shadowsocks/shadowsocks-org/wiki/SIP002-URI-Scheme).

## Processing pipeline and result semantics

The normal pipeline is:

1. **Collect sources.** Read the selected catalog set, extra URLs, and optional link file.
2. **Fetch subscriptions.** Download URLs concurrently, using `--fetch-concurrency` and `--fetch-timeout`. Successful response bodies are saved to the raw output file.
3. **Extract, parse, and deduplicate links.** Direct links from the file are combined with links extracted from fetched text. `--shuffle` and `--limit` are applied before checks; `--shuffle-seed N` makes the order repeatable for the same input and binary version.
4. **Write the parsed list and perform GeoIP lookup.** Unless disabled, the database maps IP addresses to country codes and names are displayed in the `[XX] - NNN` style. The parsed output is then refreshed with the renamed display names.
5. **TCP prefilter.** Unless `--no-tcp-ping` is set, TCP-based proxy endpoints are probed and their TCP round-trip time is recorded. Hysteria 1 and 2 use UDP and skip this TCP probe, so no TCP RTT is recorded for them. `--tcp-max-ms` applies to completed TCP probes; `--max-ping` filters only endpoints with a measured TCP RTT and does not discard Hysteria solely because its TCP probe was skipped.
6. **Speed check.** Unless `--no-speed` is set, Xray checks supported proxy variants in batches through a local SOCKS inbound. Hysteria 1 is measured by the separate Hysteria v1 core; Hysteria 2 is measured through Xray. Hysteria 2 Gecko obfuscation and transport options outside the implemented Xray subset are skipped. Shadowsocks methods outside Xray's cipher allowlist are parsed and supported by the Mihomo converter, but skipped for speed checking. Since the CLI generates Clash/mihomo only from `good`, such entries do not appear in the normal CLI-generated YAML unless they pass a speed check. Results below `--min-kb` do not pass; each measurement is capped by `--speed-max-mb` where applicable.
7. **Write results and optionally render Clash YAML.** Clash generation uses only the proxies that passed the speed threshold.

### `alive` and `good`

- **`alive`** is the final set that remains usable under the selected checks. With speed checking enabled, it contains proxies that pass `--min-kb`; it is sorted by TCP ping when ping values are available. With `--no-speed`, it contains survivors of applicable TCP checks (or all candidates when TCP ping is also disabled), sorted by ping when a ping value exists.
- **`good`** is the set that passed the speed threshold. It is sorted by measured speed, highest first. `good.txt` is not written by the `--no-speed` path because no speed-qualified set was measured.
- **`dead`** is optional. When `--dead FILE` is supplied, it includes proxies that failed TCP, exceeded a configured ping limit, or failed an executed speed check. Proxies skipped because the available speed core does not support their transport or cipher are reported as unchecked, not dead.

With TCP ping enabled, the alive file is first written after TCP filtering and may be refreshed after speed checking. If no candidate remains after TCP filtering, the run writes the optional dead file and exits that pipeline early without creating or refreshing the alive output file (an older file can remain). If `--no-tcp-ping` is set, there is no TCP-stage output; the alive file is written at the final output stage.

Intermediate files are updated as the relevant stages complete. Writes use a temporary file followed by replacement so that readers do not see a partially written result.

Subscription HTTPS certificates are verified by default. Each response is capped at 16 MiB (`--max-source-bytes`); two retries after the initial attempt are the default (`--fetch-retries`, range `0..=8`). The `.proxy_cache` directory stores successful responses and `ETag`/`Last-Modified` validators. Validators avoid downloading unchanged bodies, and the last successful body can be used after a temporary failure. Disable caching with `--no-fetch-cache`. `--insecure-source-tls` explicitly disables HTTPS certificate verification.

Use `--source-report source-quality.json` for per-source fetch, parse, duplicate, TCP, speed-qualified, below-threshold, and Xray outbound rejection counts. Use `--speed-report speed.json` to tie individual probe outcomes to a `source_id`. URLs are sanitized and raw errors/bodies are omitted. The speed report includes proxy names and server addresses; keep it private.

Save completed stages with `--checkpoint run.checkpoint`, then resume after interruption:

```sh
proxy-rs --resume --checkpoint run.checkpoint
```

`--resume` requires `--checkpoint FILE`. Checkpoints contain proxy links and results, so keep them private. Use compatible run options when resuming.

## Command-line reference

The tables below reflect `cargo run -- --help` and the current CLI validation in `src/cli.rs`. Defaults are shown explicitly. Numeric ranges are inclusive where given. Concurrency and batch limits are validated as shown even though the short help output does not print those ranges.

### General and sources

| Option | Default | Meaning / accepted range |
|---|---:|---|
| `-h`, `--help` | — | Print help and exit. |
| `-V`, `--version` | — | Print version and exit. |
| `--set SET` | `3` | Catalog key, or `all` (case-insensitive) to select every key in the catalog. |
| `--sources SOURCES` | `sources.json` | Path to the JSON catalog. |
| `-f`, `--file FILE` | — | Text file with subscription URLs and/or direct proxy links. |
| `--check-one FILE` | — | Read exactly one proxy link from a file and report parsing, core startup, SOCKS readiness, download, and speed as JSON. Mutually exclusive with the regular pipeline and export options. |
| `-u`, `--url URL` | — | Add a subscription URL; may be supplied more than once. |
| `--fetch-concurrency N` | `32` | Concurrent source downloads; `1..=4096`. |
| `--fetch-timeout SECONDS` | `15` | Source request timeout; `1..=3600` seconds. |
| `--fetch-retries N` | `2` | Retries after transient failures; `0..=8`. |
| `--max-source-bytes N` | `16777216` (16 MiB) | Maximum response size per source; `1024..=1073741824`. |
| `--insecure-source-tls` | Off | Disable certificate verification for subscription HTTPS; verification is enabled by default. |
| `--fetch-cache-dir DIR` | `.proxy_cache` | Directory for successful subscription bodies and HTTP validators. |
| `--no-fetch-cache` | Off | Disable cache reads/writes and conditional requests. |
| `--resume` | Off | Resume from a checkpoint; requires `--checkpoint FILE`. |
| `--protocol PROTOCOL` | — | Keep only the selected protocols; repeat the option or use comma-separated values. Matching is case-insensitive. |
| `--country COUNTRY` | — | Keep only the selected ISO country codes or GeoIP tags; repeat the option or use comma-separated values. Matching is case-insensitive. |
| `--only-new` | Off | Compare parsed proxy outbound keys with keys from the existing `--output` file and keep only new proxies. |

### Output files and selection

| Option | Default | Meaning / accepted range |
|---|---:|---|
| `-o`, `--output OUTPUT` | `alive.txt` | Path for the alive proxy list. |
| `--good-output GOOD_OUTPUT` | `good.txt` | Path for proxies that pass speed checking. |
| `--dead DEAD` | — | Optional path for failed/filtered proxies. |
| `-n`, `--limit N` | No limit | Keep at most the first `N` parsed proxies before checks; `N > 0`. |
| `-s`, `--shuffle` | Off | Shuffle the parsed list before applying the limit and running checks. |
| `--shuffle-seed U64` | — | Reproducibly shuffle with this seed; enables shuffling. Repeatability requires the same input and binary version. |
| `--raw-output RAW_OUTPUT` | `raw.txt` | Path for successfully downloaded subscription response texts. |
| `--parsed-output PARSED_OUTPUT` | `parsed.txt` | Path for unique parsed proxies; refreshed after GeoIP naming. |
| `--source-report FILE` | — | Write per-source JSON counts for fetch (including failed-attempt count), parse, duplicates, TCP-live, speed-qualified, below-threshold, and Xray outbound rejection reasons. URLs are sanitized. |
| `--checkpoint FILE` | — | Atomically save completed stages for a later `--resume`; contains proxy data. |
| `--json-output FILE` | — | Write final alive proxies as a JSON array containing only the summary fields `protocol`, `server`, `port`, `name`, `country`, `display`, `ping_ms`, and `speed_kbs`. Proxy URIs, passwords, UUIDs, and connection settings are omitted. |
| `--speed-report FILE` | — | Write per-candidate JSON with `source_id`, median, successful count, range, and every probe's target URL, status, bytes, duration, speed, or classified failure. It includes proxy names and server addresses; keep it private. Older speed checkpoints do not retain probe details, so `probes` is `null`. |
| `--clash [FILE]` | Off; filename `clash.yaml` if flag has no value | Generate a Clash/mihomo file from speed-qualified proxies. |
| `--clash-no-country` | Off | Omit country and pseudo-country groups from the generated Clash file. |
| `--clash-no-categories` | Off | Omit Telegram, AI, YouTube, and Games groups and their category routing rules. |
| `--singbox [FILE]` | Off; filename `sing-box.json` if flag has no value | Generate a complete sing-box JSON config from `good`; nodes with unsupported settings are skipped. |
| `--singbox-report FILE` | — | Write a JSON report listing converted nodes and exact reasons why other nodes were skipped. |
| `--singbox-check` | Off | Validate the generated config with `sing-box check`; requires `--singbox` and at least one speed-qualified node. A validation failure exits with an error. |
| `--singbox-bin FILE` | `sing-box` from `PATH` | Use this sing-box executable for `version` and `check`; requires `--singbox-check`. |

### Xray

| Option | Default | Meaning / accepted range |
|---|---:|---|
| `--core CORE` | `xray` (`xray.exe` on Windows) | Path to the Xray executable. |
| `--workdir WORKDIR` | `.proxy_runtime` | Directory for temporary batch configurations. |
| `--batch N` | `100` | Proxies per Xray batch; `1..=10000`. |
| `--lport PORT` | `10000` | Base local SOCKS port; `1024..=65000`. |
| `--batch-workers N` | `8` | Number of Xray batches checked concurrently; `1..=1024`. |
| `--core-start-timeout SECONDS` | `8` | How long to wait for an Xray process to start; `1..=3600` seconds. |
| `--xray-url URL` | Platform-specific latest Xray release archive | Override the URL used to download Xray. |
| `--no-xray-download` | Off | Do not download Xray automatically; fail if a valid local executable is unavailable. |
| `--no-hysteria1-download` | Off | Do not download the Hysteria 1 core; Hysteria 1 speed checks are skipped if no valid local core is available. The automatically downloaded Hysteria v1.3.5 binary is SHA-256 verified. |

### TCP prefilter

| Option | Default | Meaning / accepted range |
|---|---:|---|
| `--no-tcp-ping` | Off | Skip TCP reachability/RTT filtering. |
| `--tcp-timeout SECONDS` | `3` | Per-connection timeout; `0.1..=3600` seconds. |
| `--tcp-concurrency N` | `1000` | Concurrent TCP probes; `1..=65536`. |
| `--tcp-retries N` | `1` | Attempts per endpoint; `1..=100`. |
| `--tcp-max-ms MS` | `0` | Maximum TCP RTT accepted by the prefilter; `0` disables this filter. |
| `--max-ping MS` | `0` | Discard successful TCP probes above this RTT; `0` disables this filter. |

`--tcp-max-ms` is applied by the TCP filter. `--max-ping` is a later filter on the surviving candidates. Both thresholds use the TCP probe time, not the speed test.

### Speed checks

| Option | Default | Meaning / accepted range |
|---|---:|---|
| `--no-speed` | Off | Skip all speed checks. In this mode `good.txt` is not produced by the speed stage. |
| `--speed-url URL` | Built-in target pool | Try this URL first for a speed measurement; fallback targets are available when needed. |
| `--speed-timeout SECONDS` | `6` | Overall download measurement timeout; `1..=3600` seconds. |
| `--speed-connect-timeout SECONDS` | `3` | Connection timeout for a speed target; `1..=3600` seconds. |
| `--speed-max-mb MB` | `5` | Maximum bytes read per speed measurement; `0.5..=1024` MB. |
| `--min-kb KB_PER_SECOND` | `100` | Minimum measured speed for a proxy to pass; `0..=1,000,000,000` KB/s. |
| `--speed-threads N` | `16` | Concurrent speed measurements; `1..=4096`. |
| `--speed-probes N` | `3` | Number of independent test targets per proxy; successful results are summarized by their median and min/max range; `1..=9`. |

### GeoIP

| Option | Default | Meaning |
|---|---|---|
| `--no-geoip` | Off | Skip country lookup and `[XX] - NNN` display-name generation. |
| `--geoip-db FILE` | `geoip.dat` beside the executable | Path to a local GeoIP database. |
| `--geoip-url URL` | `https://raw.githubusercontent.com/runetfreedom/russia-v2ray-rules-dat/release/geoip.dat` | URL used to download the database when the selected local file is unavailable. |

For numeric values, non-finite floating-point values are rejected. Integer options are also constrained by their Rust integer types in addition to the listed validation ranges.

For example, keep only new VLESS/VMess proxies in the US and Germany, comparing them with the current `alive.txt`, and write the final result as JSON:

```sh
proxy-rs --url 'https://example.org/sub' --protocol vless,vmess \
  --country US,DE --only-new --json-output new-proxies.json
```

## Files and paths

| File / directory | Purpose |
|---|---|
| `sources.json` | Default catalog of subscription URL sets. |
| `raw.txt` | Fetched subscription response bodies; written when at least one response succeeds. |
| `parsed.txt` | Parsed and deduplicated proxies, updated after GeoIP naming when enabled. |
| `alive.txt` | Final alive list, sorted by TCP ping when available. |
| `good.txt` | Speed-qualified proxies, sorted by speed; not generated by `--no-speed`. |
| `dead.txt` | Optional failed/filtered proxy list, enabled with `--dead`. |
| `clash.yaml` | Optional generated configuration; default name when `--clash` is passed without a filename. |
| `.proxy_runtime/` | Temporary Xray batch configuration and runtime files. |
| `xray` / `xray.exe` | Downloaded or user-provided Xray core, normally beside the executable when using the default path. |
| `geoip.dat` | Downloaded or user-provided GeoIP database, normally beside the executable when using the default path. |

Output paths are relative to the process working directory unless an absolute path is supplied. The default asset lookup also checks beside the running executable and the bundled `bin` directory where applicable. Generated text files are UTF-8 and written atomically.

## Xray and GeoIP data

### Xray

When speed checks are enabled, `proxy-rs` requires a working Xray core for Xray-supported protocols. If the configured executable is missing or invalid, the CLI downloads the matching archive from the latest XTLS/Xray-core release unless `--no-xray-download` is set. Hysteria 1 uses its separately managed Hysteria v1.3.5 core. Automatic Xray archive selection currently supports:

| Operating system | x86_64 | aarch64 |
|---|---|---|
| Windows | `Xray-windows-64.zip` | `Xray-windows-arm64-v8a.zip` |
| Linux | `Xray-linux-64.zip` | `Xray-linux-arm64-v8a.zip` |
| macOS | `Xray-macos-64.zip` | `Xray-macos-arm64-v8a.zip` |

Use `--core PATH` for a preinstalled Xray core or `--xray-url URL` for a compatible archive. Xray is checked by running its `version` command and uses local SOCKS inbounds to test proxy outbounds. Hysteria 1 speed checks use a separate Hysteria v1.3.5 core, downloaded with SHA-256 verification when needed unless `--no-hysteria1-download` is set. Temporary Xray batch configs are placed in `--workdir` and cleaned up after processing.

### GeoIP

GeoIP lookup is enabled by default. If no explicit `--geoip-db` path is provided, the application uses `geoip.dat` beside the executable; if needed, it downloads the database from `--geoip-url`. The project parses the database locally and supports IPv4 and IPv6 ranges. Use `--no-geoip` to avoid downloading or reading the database.

## Clash/mihomo generation

Pass `--clash [FILE]` to generate YAML. The input is **only** the `good` set: proxies that passed the applicable core's speed check at or above `--min-kb`. If no proxy passes, generation is skipped. The writer reports unsupported protocols and excludes those entries from the generated proxy list.

### Proxy and transport mapping

The generator maps VLESS, VMess, Trojan, Shadowsocks, Hysteria 1, and Hysteria 2 to Mihomo proxy entries. For VLESS/VMess/Trojan, supported network transports include raw/TCP, WebSocket, gRPC, HTTP Upgrade (emitted as WebSocket with `v2ray-http-upgrade`), XHTTP, and best-effort KCP. TLS and Reality settings are translated when compatible; Reality's Xray `realitySettings.password` becomes Clash `reality-opts.public-key`. Trojan uses `sni`; VLESS and VMess use `servername`. Hysteria 1/2 map to their Mihomo types. For Hysteria 2, Salamander and URI port hopping are translated into Xray's implemented UDP finalmask settings; Gecko can be exported to Mihomo but is skipped by Xray speed checks. Other Hysteria 2 options are speed-checkable only where the current Xray configuration implements them; parsing a URI alone does not establish support. Shadowsocks ciphers outside Xray's allowlist remain parseable and representable by the Mihomo converter but are not speed-checked. The CLI's `--clash` output is limited to `good`, so those skipped entries are absent from normal CLI-generated YAML.

Unsupported protocols are skipped and counted in the generation report. Clash output is written atomically.

### Groups and routing

With defaults, the generated configuration includes:

- `🚀 Прокси`: top-level manual selector.
- `🌍 Страны`: manual selector containing country and pseudo-country groups, when country grouping is enabled and entries exist.
- `🔓 Заблокированные` and `🟢 Напрямую`: manual routing selectors; the direct selector starts with `DIRECT`.
- `💬 Telegram`, `🤖 AI`, `📺 YouTube`, and `🎮 Игры`: optional category selectors.
- `♻️ Fallback`, `⚡ Авто`, and `⚖️ Баланс`: fallback, URL-test, and consistent-hashing load-balance groups.
- Per-country and pseudo-country URL-test groups, placed after the main selectors and automatic groups.

Known pseudo-country labels include `CLOUDFLARE` and `RU-BLOCKED`; each pseudo-country code gets its own group. Missing or `XX` country values go to `🌐 Прочие`. `--clash-no-country` removes country groups. `--clash-no-categories` removes category groups and changes the blocked-service rule destination to the main proxy selector. Russian services and country rules are configured in `proxy-clash/src/rules.rs`; there are no `PROCESS-NAME` rules. Russian category/bank and `GEOIP,RU` rules route through `🟢 Напрямую`.

The generated YAML also contains global DNS, profile, and sniffer settings defined by the Clash library.

## Workspace architecture

The repository root is both the `proxy-rs` package and the Cargo workspace root (`resolver = "2"`). `build.rs` copies `bin/sources.json` into the active Cargo build profile.

| Module | Responsibility |
|---|---|
| `src/main.rs` | CLI entry point, Ctrl-C handling, and final summary display. |
| `src/pipeline.rs` | Pipeline orchestration, checkpoint/resume, filtering, and output generation. |
| `src/cli.rs` | Argument declarations, help text, defaults, and numeric validation. |
| `src/sources.rs` | Source catalog, URL, and direct-link collection and deduplication. |
| `src/fetch.rs` | Concurrent subscription HTTP requests and response decoding. |
| `src/parse.rs` | Link extraction, URI parsing, normalization, and proxy deduplication. |
| `src/geoip.rs` | GeoIP download, protobuf parsing, address lookup, and country naming. |
| `src/tcp_ping.rs` | Concurrent TCP endpoint checks and RTT filtering. |
| `src/xray.rs` | Xray download/validation, batch process management, proxy checks, and incremental good output. |
| `src/speed.rs` | Download measurement through SOCKS and target fallback logic. |
| `src/outbound.rs` | Conversion of parsed proxy settings into Xray stream/outbound settings. |
| `src/output.rs` | Output line formatting, atomic file replacement, and result display. |
| `src/check.rs` | SOCKS client support used by speed checks. |
| `proxy-clash/` | Pure library: Xray-derived proxy data → Clash/mihomo YAML and a generation report; no file I/O. |
| `proxy-singbox/` | Pure library: supported Xray-derived proxy data → sing-box JSON and a report of converted/skipped nodes; no file I/O. |
| `tests/cli_entrypoint.rs` | End-to-end CLI tests using local fixtures and local test servers. |

The root crate owns network, process, and filesystem work. The `proxy-clash` crate owns YAML conversion, groups, and routing rules; `serde_yaml_ng` is confined to that crate. The `proxy-singbox` crate owns sing-box JSON conversion and explicit unsupported-node reporting.

## Development, tests, and CI

Format and check the whole workspace:

```sh
cargo fmt --all -- --check
cargo clippy --workspace --all-targets -- -D warnings
cargo build --workspace --release
cargo test --workspace
```

Run just the Clash library tests:

```sh
cargo test -p proxy-clash
```

The tests include a deterministic fuzz-style corpus of malformed proxy URIs and GeoIP protobuf inputs. It exercises seeded regression cases without a separate fuzzing runtime. Synthetic benchmarks cover parse+dedup, GeoIP, and Clash generation:

```sh
cargo bench --bench pipeline
```

Coverage and mutation testing are configured in `.github/workflows/cross-platform.yml`:

```sh
cargo +nightly-2026-10-26 llvm-cov --remap-path-prefix --workspace --lib --bins --tests --branch --summary-only --ignore-filename-regex '(^/|.cargo/registry|/benches/)' --fail-under-lines 96
CARGO_PROFILE_DEV_DEBUG=0 cargo +stable mutants --workspace --jobs 2 --timeout 120 --minimum-test-timeout 30
```

The coverage workflow pins a nightly toolchain for LLVM branch instrumentation and enforces at least 96% line coverage (the current Ubuntu measurement was 96.70% lines and 84.86% branches). Mutation testing runs on Ubuntu with two jobs. These tools need to be installed separately for local runs.

The cross-platform workflow is available through manual `workflow_dispatch`. It runs Clippy with warnings denied, a release build, and workspace tests on `ubuntu-latest`, `macos-latest`, and `windows-latest`; automatic push and pull-request triggers are disabled.

The `.github/workflows/release.yml` workflow runs when a `v*` tag is pushed. It builds application and documentation archives on Linux, macOS, and Windows, generates SHA-256 checksums, and creates a draft GitHub Release for manual review.

The separate `.github/workflows/proxy-collection.yml` workflow runs every 30 minutes (at :11 and :41 UTC) or on manual dispatch. It performs a real network run using the public source catalog, generates a Clash configuration, and publishes artifacts to the `dist` branch. Scheduled runs do not cancel an active run. Do not invoke an equivalent full-source run locally unless you intentionally want the large network workload.

## Troubleshooting

### `no sources to load` or missing `sources.json`

Confirm that the selected set exists in the JSON catalog. Provide an explicit catalog with `--sources FILE`, add `--url URL`, or provide a `--file FILE` containing subscription URLs or direct proxy URIs. A missing default catalog is allowed when explicit URLs or a link file are supplied.

### `sources.json` is invalid or a set is missing

The file must be a JSON object with the selected set name mapped to an array. Use `--set all` to select every available key. Verify that the file is valid UTF-8 JSON and that URL entries are strings.

### No compatible proxies found

Check that the input contains supported proxy URI schemes and valid credentials, host names, and ports. The CLI summary separates invalid links, duplicates, and unsupported protocols. If input is Base64-encoded subscription text, ensure the fetched or local text contains supported encoded content.

### All subscription downloads failed

Check URL reachability, network access, and the configured `--fetch-timeout`. Lower `--fetch-concurrency` if the source hosts or local network cannot handle many simultaneous requests. The summary shows a small number of failure examples.

### No proxies remain after TCP filtering

Try a larger `--tcp-timeout`, additional `--tcp-retries`, or a less restrictive `--tcp-max-ms`/`--max-ping`. `--no-tcp-ping` disables the prefilter when you want Xray to test the candidates directly.

### Xray is missing or speed checks fail

Confirm that the Xray binary matches the OS and architecture and is executable. Use `--core PATH` for an existing core, `--xray-url URL` for a compatible archive, or `--no-xray-download` when downloads must be disabled. If Xray cannot start, adjust `--core-start-timeout` and check that the local SOCKS port range beginning at `--lport` is available.

### No proxy passes the speed threshold

Lower `--min-kb`, increase `--speed-timeout` or `--speed-max-mb`, or specify a reachable `--speed-url`. A proxy that is TCP-reachable can still fail to establish an Xray tunnel or deliver enough data before the measurement deadline.

### GeoIP database errors

Check that the database path is writable for downloads and that the file is a valid supported `geoip.dat`. Use `--geoip-db FILE` for a known local database, `--geoip-url URL` to select another source, or `--no-geoip` to disable country lookup.

### Clash output is missing or has fewer proxies than expected

Clash generation is skipped when no proxy passes speed checking. It includes only supported Mihomo protocols from the `good` set; Hysteria 1/2 are eligible after their applicable speed check. Check the speed threshold and generation summary for skipped protocols. Use `--clash-no-country` or `--clash-no-categories` to disable optional groups.

### Output path cannot be written

Create the destination directory first and check filesystem permissions. Output paths are relative to the current working directory unless absolute paths are used.

## License

This project is distributed under the [MIT License](../../LICENSE).
