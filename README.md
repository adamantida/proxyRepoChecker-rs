# proxy-rs

`proxy-rs` — cross-platform Rust CLI for Windows, macOS, and Linux. It parses proxy links from subscription aggregators, tags them by country (GeoIP), prefilters by TCP ping, then measures real speed through `xray` as a local SOCKS client.

## Commands
- Build on Windows: `cargo build --release` → `target\release\proxy-rs.exe`.
- Build on macOS/Linux: `cargo build --release` → `target/release/proxy-rs`. Builds should produce **0 warnings**.
- The `proxy-clash` crate has unit tests: `cargo test -p proxy-clash`. There are no integration tests for the full network pipeline.
- Network smoke runs are limited to `proxy-rs --set 3 -n 200` (add `./` on macOS/Linux or use the `.exe` path on Windows). A full run requires an explicit request.
- `-h/--help` is the source of truth for options (grouped, colored, with examples).

## Assets & paths
- `bin\sources.json` — source sets. Keys `"1"` (70 urls), `"2"` (87), `"3"` (2 test urls). Default `--set 3`, default `--sources sources.json`.
- `build.rs` copies `bin\sources.json` into the build profile dir (`target\<profile>`, works for `--target` too) on every build. It does **not** copy xray anymore.
- `xray.exe` (Windows) or `xray` (macOS/Linux) is resolved in order: CWD → next to the executable → `bin/`. If missing (and not `--no-speed`), the matching XTLS/Xray-core archive is downloaded automatically for the current OS and architecture. `--xray-url` overrides the URL; `--no-xray-download` disables downloading. Linux and macOS binaries are installed with executable permissions.
- `geoip.dat` is auto-downloaded next to the exe. It is **v2ray protobuf format** (custom parser in `geoip.rs`), **not** MaxMind/mmdb.

## Pipeline (`src/main.rs`)
`fetch → parse+dedup → GeoIP rename "[XX] - NNN" → TCP ping (prefilter) → xray speed` → outputs.
- Liveness = passes speed threshold (`--min-kb`, default 100 KB/s). `--no-speed` skips the xray phase (alive = TCP-alive, `good.txt` not written).
- Outputs: `alive.txt` (ping asc), `good.txt` (KB/s desc), optional `--dead`.
- Incremental atomic saves after **each** stage, not just at the end: `raw.txt` (fetch), `parsed.txt` (parse, rewritten with GeoIP names), `alive.txt`+`dead.txt` (after TCP), `good.txt` updated after each completed speed batch. Replacement works on Windows, macOS, and Linux. New flags: `--raw-output`, `--parsed-output`.

## xray integration (`src/xray.rs`) — easy to get wrong
- Outbound uses **flat `settings`** (`{"address","port","id",...}`), NOT nested `vnext`/`servers`. `outboundTag` is a string, `pcs` is a string, `allowInsecure` used when insecure.
- One xray process per batch; each proxy gets its own local SOCKS inbound + a routing rule (inboundTag→outboundTag).
- Port allocation (`alloc_port`) uses atomic CAS plus a real `TcpListener::bind` probe to skip ports unavailable on the current OS. Removing that probe causes bind failures → exponential repair/bisection storms (major slowdown).
- Batch repair: on `failed to build outbound config with tag out_N`, drop that outbound and rerun; otherwise bisection.

## Editing gotchas
- Source and logs are **Russian**. Do **not** edit sources with PowerShell `Get-Content`/`Set-Content` (corrupts Cyrillic and may add a BOM) — use the `edit`/`write` tools.
- Console output uses `indicatif` progress bars only + a `>> [+Ns]` prefix (macro in `main.rs`) + a colored final summary (`colored` crate). Do not add periodic `println!` progress lines.

## Connect
### Url
`https://raw.githubusercontent.com/adamantida/proxyRepoChecker-rs/dist/clash.yaml`
happ and other `https://raw.githubusercontent.com/adamantida/proxyRepoChecker-rs/dist/good.txt`
### Qr
<img width="256" height="256" alt="qr-code" src="https://github.com/user-attachments/assets/7b86669d-715d-4f39-a396-3c2cda4ae266" />


