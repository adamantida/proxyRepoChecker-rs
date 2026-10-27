# proxy-rs 项目手册

> 一个用 Rust 编写的跨平台命令行工具：收集代理订阅和分享链接，解析并去重，按需执行 GeoIP、TCP 连通性和 Xray 速度检查，最后保存结果并生成 Clash/mihomo 配置。

[Русская версия](../../README.md) · [English version](../../README.en.md) · [简体中文项目主页](../../README.zh-CN.md)

## 目录

- [项目概览](#项目概览)
- [环境要求](#环境要求)
- [构建与运行](#构建与运行)
- [输入文件与 sourcesjson](#输入文件与-sourcesjson)
- [处理流程与结果语义](#处理流程与结果语义)
- [命令行参数](#命令行参数)
- [输出文件与路径](#输出文件与路径)
- [Xray 与 GeoIP](#xray-与-geoip)
- [Clash/mihomo 配置](#clashmihomo-配置)
- [Workspace 与模块架构](#workspace-与模块架构)
- [开发、测试与 CI](#开发测试与-ci)
- [常见问题排查](#常见问题排查)

## 项目概览

`proxy-rs` 是一个 Rust CLI，主要用于从订阅聚合器收集代理节点并筛选可用节点。程序支持解析 VLESS、VMess、Trojan 和 Shadowsocks 链接；不支持的协议会计数并跳过，格式错误的链接也不会进入后续检查。

主要能力：

- 从内置来源集合、额外 URL 和本地文件读取订阅或代理链接。
- 提取链接、解析节点、去重，并可随机打乱或限制检查数量。
- 使用 `geoip.dat` 为节点分配国家或特殊标签，并生成 `[XX] - NNN` 显示名。
- 先用 TCP 连接延迟筛选，再通过 Xray 和 SOCKS 入站实际下载数据来测量速度。
- 原子写入原始数据、解析结果、存活节点、合格节点和可选的 Clash/mihomo YAML。
- 在 Windows、macOS 和 Linux 上构建；自动下载 Xray 的平台支持范围见 [Xray 与 GeoIP](#xray-与-geoip)。

## 环境要求

- Rust 稳定版工具链及 Cargo，建议使用当前稳定版。
- Windows、macOS 或 Linux。Xray 自动下载支持 x86_64 和 aarch64 的 Windows、Linux、macOS。
- 若要检查速度，需要可运行的 Xray 核心；默认会按当前平台下载。若禁用自动下载，必须自行提供核心。
- 默认启用 GeoIP；首次运行需要有效的本地 `geoip.dat`，或者能访问配置的 GeoIP 下载 URL。可通过 `--no-geoip` 禁用。
- 正常运行需要访问所选订阅 URL。速度检查还需要目标代理能够连接测速 URL。

`reqwest` 使用 rustls TLS，不要求系统安装 OpenSSL。下载文件、输出文件和工作目录需要有读写权限。

## 构建与运行

在仓库根目录执行：

```sh
cargo build --release
```

二进制文件位于 `target/release/`：Unix 系统为 `proxy-rs`，Windows 为 `proxy-rs.exe`。

### 构建脚本与交叉编译

macOS/Linux 使用 Bash 脚本，Windows 在 `cmd.exe` 中运行 BAT 脚本。不指定平台时构建当前主机；可选 `linux`、`macos`、`windows` 或 `all`，架构可选 `x86_64`、`aarch64`：

```sh
scripts/build.sh                         # 当前平台
scripts/build.sh linux --arch aarch64 --install-target
scripts/build.sh all --arch x86_64
```

```bat
scripts\build.bat windows --arch x86_64 --install-target
scripts\build.bat all --arch aarch64 --install-target
```

`--install-target` 只通过 rustup 安装 Rust target，不会安装 linker 或 SDK。Windows GNU 需要兼容的 MinGW linker，Windows ARM64 需要 MSVC 工具链，macOS 构建需要 Apple SDK/toolchain；交叉编译还可能需要配置 Cargo linker。本机产物位于 `target/release/`，交叉编译产物位于 `target/<rust-target>/release/`。若某个目标失败，脚本仍会尝试其余目标，最终只要有一个失败就返回非零状态。

常见运行示例：

```sh
# 默认使用来源集合 3；只检查前 200 个节点
cargo run --release -- --set 3 -n 200

# 使用集合 1，随机打乱顺序，并将无法通过 TCP 检查的节点写入 dead.txt
cargo run --release -- --set 1 --shuffle --dead dead.txt

# 只执行 TCP 检查，不测速度
cargo run --release -- --no-speed

# 加载额外订阅；提高最低速度门槛并限制数量
cargo run --release -- -u 'https://example.com/sub' --min-kb 500 -n 100

# 所有来源集合，启用 Clash 输出
cargo run --release -- --set all --clash
```

查看本机当前版本的完整帮助：

```sh
cargo run -- --help
```

参数表以当前 `--help` 为准；下文同时列出源码校验的取值范围。

## 输入文件与 `sources.json`

### 内置来源集合

仓库中的 `bin/sources.json` 是 JSON 对象，键是集合编号（当前为 `"1"`、`"2"`、`"3"`），值是订阅 URL 数组。构建脚本会将它复制到构建配置目录；程序默认寻找名为 `sources.json` 的文件。选择 `--set 3` 时，只读取键 `"3"`；`--set all`（大小写不敏感）会读取 JSON 对象里的所有集合。

简化格式如下：

```json
{
  "1": ["https://example.com/subscription-a"],
  "2": ["https://example.com/subscription-b"],
  "3": ["https://example.com/subscription-c"]
}
```

根值必须是 JSON 对象，所选集合必须存在且为数组。数组里的字符串会作为 URL 使用；非字符串项目会被忽略。程序不会在手册中固定列出上游清单，因为 `bin/sources.json` 会随项目更新。

### 额外输入

- `-u/--url` 接收额外订阅 URL；可与内置集合和文件输入合并。
- `-f/--file` 按行读取文本：空行和以 `#` 开头的注释行忽略；以 `http://` 或 `https://` 开头的行作为订阅 URL；含 `://` 的其他行作为代理分享链接交给解析器；不符合这两类格式的行会被忽略。
- 多个订阅来源的 URL 会去重，并保留首次出现顺序。直接分享链接在解析阶段按代理键去重。
- 如果 `sources.json` 不存在，只要通过 `-u` 或 `-f` 提供了输入，程序仍可继续；否则会提示找不到来源文件。

文件输入示例：

```text
# 一行一个订阅或分享链接
https://example.com/subscription
vless://uuid@example.net:443#node
```

## 处理流程与结果语义

主流程按以下顺序执行：

1. **收集输入**：选择 `sources.json` 集合，并合并 `--url`、`--file` 中的输入。
2. **下载订阅**：并发读取订阅文本；每个失败来源会被记录，成功读取的原始文本写入 `raw.txt`（或 `--raw-output` 指定的路径）。如果没有任何可解析的输入，会报错退出。
3. **提取、解析、去重**：从订阅文本提取代理 URI，与文件中的直接链接合并；解析支持的协议并统计重复、无效和不支持的项目。
4. **打乱与截断**：如指定 `--shuffle` 则随机打乱；`--limit N` 将候选数量截为前 N 个。
5. **写解析结果**：将唯一节点写入 `parsed.txt`。启用 GeoIP 后，该文件之后会再次写入带国家标签的显示名。
6. **GeoIP**：默认解析域名并查找 IP 所属国家或特殊标签，重命名显示名。`--no-geoip` 会保留原始节点名。
7. **TCP 预筛选**：默认尝试连接代理服务器地址。成功的节点记录最佳 TCP RTT；连接失败的节点视为 TCP 阶段的死节点。`--tcp-max-ms` 可在 TCP 阶段按延迟过滤。
8. **`--max-ping` 筛选**：在 TCP 阶段之后，按 TCP RTT 再排除超过指定毫秒数的节点。
9. **速度检查**：默认由 Xray 启动节点并经 SOCKS 下载测速数据。速度低于 `--min-kb` 的节点不进入合格集合；合格节点按速度写入 `good.txt`。
10. **生成最终文件**：`alive.txt`、可选的死节点文件，以及基于合格节点的 Clash/mihomo 配置。

### `alive`、`good` 与 `--no-speed`

- 默认启用速度检查时，**`alive` 是通过速度门槛的节点**；节点必须先通过启用的 TCP 和 ping 筛选。`alive.txt` 在有 ping 值时按 TCP ping 排序，`good.txt` 按速度排序。
- **`good` 是速度检查成功且速度达到 `--min-kb` 的节点**。Clash/mihomo 只从 `good` 生成。若没有合格节点，Clash 生成会跳过。
- 使用 `--no-speed` 时，**`alive` 表示通过已启用的 TCP/ping 筛选的候选节点**；程序不启动 Xray，不测速度，不写入或更新 `good.txt`，也不会生成 Clash 配置。
- 使用 `--no-tcp-ping` 时会跳过 TCP 预筛选；这不代表节点经过了连通性验证。若同时使用 `--no-speed`，候选节点不会经过连通性检查。仍启用速度检查时，后续 Xray 测速会决定是否合格。
- `--dead` 是可选输出。它收集 TCP 阶段失败、被 TCP 延迟或 `--max-ping` 淘汰，以及未通过速度检查的代理分享链接。

默认验证订阅 HTTPS 证书。每个来源响应上限为 16 MiB（`--max-source-bytes`）；首次请求之外默认重试两次（`--fetch-retries`，范围 `0..=8`）。`.proxy_cache` 保存成功响应及 `ETag`/`Last-Modified` 验证器；来源未变化时无需重新下载正文，临时错误时可回退到上次成功内容。使用 `--no-fetch-cache` 关闭缓存。`--insecure-source-tls` 会显式关闭 HTTPS 证书验证。

使用 `--source-report source-quality.json` 输出每个来源的状态和计数：已获取、有效、重复、TCP 可达及速度达标。报告会隐藏 URL 中的凭据、查询参数、fragment 和非根路径；不会包含响应正文或网络错误文本。

使用 `--checkpoint run.checkpoint` 保存已完成阶段，中断后运行以下命令续跑：

```sh
proxy-rs --resume --checkpoint run.checkpoint
```

`--resume` 必须同时指定 `--checkpoint FILE`。Checkpoint 包含代理链接和结果，请妥善保管；续跑时使用兼容的运行参数。

## 命令行参数

以下名称和默认值按当前 `cargo run -- --help` 列出。未标注默认值的选项没有固定默认输入；布尔开关默认关闭。范围是 CLI 校验允许的边界，时间单位为秒，速度单位为 KB/s。

### 帮助

| 参数 | 默认值 / 取值 | 说明 |
|---|---|---|
| `-h, --help` | — | 显示帮助并退出。 |
| `-V, --version` | — | 显示版本并退出。 |

### 来源与订阅下载

| 参数 | 默认值 / 范围 | 说明 |
|---|---|---|
| `--set <SET>` | `3`；内置键 `1`、`2`、`3` 或 `all` | 选择 `sources.json` 集合；`all` 不区分大小写。 |
| `--sources <SOURCES>` | `sources.json` | 指定来源 JSON 文件。 |
| `-f, --file <FILE>` | 未设置 | 读取额外订阅 URL 和代理分享链接的文本文件。 |
| `-u, --url <URL>` | 未设置 | 添加订阅 URL；可多次提供。 |
| `--fetch-concurrency <N>` | `32`；`1..=4096` | 并行下载来源的数量。 |
| `--fetch-timeout <SECONDS>` | `15`；`1..=3600` | 单个订阅请求的超时。 |
| `--fetch-retries <N>` | `2`；`0..=8` | 临时错误时的重试次数，不包括首次请求。 |
| `--max-source-bytes <N>` | `16777216`（16 MiB）；`1024..=1073741824` | 每个来源响应的最大字节数。 |
| `--insecure-source-tls` | 关闭 | 禁用订阅 HTTPS 证书验证；默认会验证证书。 |
| `--fetch-cache-dir <DIR>` | `.proxy_cache` | 成功订阅正文和 HTTP validators 的缓存目录。 |
| `--no-fetch-cache` | 关闭 | 禁用缓存读写和条件请求。 |
| `--resume` | 关闭 | 从 checkpoint 继续；必须指定 `--checkpoint FILE`。 |
| `--protocol <PROTOCOL>` | 未设置 | 仅保留指定协议；可重复该参数，或用逗号分隔多个值。匹配不区分大小写。 |
| `--country <COUNTRY>` | 未设置 | 仅保留指定 ISO 国家代码或 GeoIP 标签；可重复该参数，或用逗号分隔多个值。匹配不区分大小写。 |
| `--only-new` | 关闭 | 将解析后的代理 outbound key 与现有 `--output` 文件中的 key 比较，只保留新代理。 |

### 输出与数量控制

| 参数 | 默认值 / 范围 | 说明 |
|---|---|---|
| `-o, --output <OUTPUT>` | `alive.txt` | 存放最终存活节点的文件。 |
| `--good-output <GOOD_OUTPUT>` | `good.txt` | 存放通过速度门槛的节点。`--no-speed` 时不写入。 |
| `--dead <DEAD>` | 未设置 | 可选的死节点输出路径。 |
| `-n, --limit <N>` | 不限制；`N > 0` | 限制进入检查阶段的候选节点数。 |
| `-s, --shuffle` | 关闭 | 检查前随机打乱节点顺序。 |
| `--raw-output <RAW_OUTPUT>` | `raw.txt` | 成功下载的订阅原始文本；只有存在成功下载内容时写入。 |
| `--parsed-output <PARSED_OUTPUT>` | `parsed.txt` | 唯一解析节点；启用 GeoIP 后会更新为带国家显示名的版本。 |
| `--source-report <FILE>` | 未设置 | 写入 URL 已清理的来源质量 JSON 报告。 |
| `--checkpoint <FILE>` | 未设置 | 原子保存已完成阶段供后续 `--resume`；文件包含代理数据。 |
| `--json-output <FILE>` | 未设置 | 将最终存活代理写成 JSON 数组，仅包含摘要字段 `protocol`、`server`、`port`、`name`、`country`、`display`、`ping_ms`、`speed_kbs`；不包含代理 URI、密码、UUID 或连接设置。 |
| `--clash [<FILE>]` | 未启用；省略文件名时为 `clash.yaml` | 从 `good` 节点生成 Clash/mihomo YAML。指定文件名可更改路径。 |
| `--clash-no-country` | 关闭 | 不创建国家选择组和各国家组。 |
| `--clash-no-categories` | 关闭 | 不创建 Telegram、AI、YouTube、游戏分类组，并切换相应规则目标。 |

例如，筛选美国和德国的 VLESS/VMess 节点，与当前 `alive.txt` 比较后只保留新节点，并将最终结果写成 JSON 摘要：

```sh
proxy-rs --url 'https://example.org/sub' --protocol vless,vmess \
  --country US,DE --only-new --json-output new-proxies.json
```

### Xray 与速度检查

| 参数 | 默认值 / 范围 | 说明 |
|---|---|---|
| `--core <CORE>` | 当前平台默认名：Unix 为 `xray`，Windows 为 `xray.exe` | Xray 核心路径。相对路径依次检查当前工作目录、可执行文件所在目录和旁边的 `bin/` 目录。 |
| `--workdir <WORKDIR>` | `.proxy_runtime` | Xray 批次临时配置的工作目录。 |
| `--batch <N>` | `100`；`1..=10000` | 每个 Xray 批次包含的节点数。 |
| `--lport <PORT>` | `10000`；`1024..=65000` | SOCKS 入站的基础本地端口。 |
| `--batch-workers <N>` | `8`；`1..=1024` | 并行处理的批次数。 |
| `--core-start-timeout <SECONDS>` | `8`；`1..=3600` | 等待 Xray 启动的时间。 |
| `--xray-url <URL>` | 当前 OS/架构对应的 XTLS/Xray-core 最新发行包 | 自定义 Xray 下载地址。 |
| `--no-xray-download` | 关闭 | 禁止自动下载；若核心文件不存在或无效，运行会报错。 |
| `--no-speed` | 关闭 | 跳过 Xray 速度检查。 |
| `--speed-url <URL>` | 未设置 | 优先用于速度测量的下载 URL。 |
| `--speed-timeout <SECONDS>` | `6`；`1..=3600` | 下载测速数据的超时。 |
| `--speed-connect-timeout <SECONDS>` | `3`；`1..=3600` | 测速连接超时。 |
| `--speed-max-mb <MB>` | `5`；`0.5..=1024` | 单次测速最多下载的数据量。 |
| `--min-kb <KB/S>` | `100`；`0..=1,000,000,000` | 合格节点的最低速度门槛。 |
| `--speed-threads <N>` | `16`；`1..=4096` | 并行速度测量数。 |

### TCP ping 预筛选

| 参数 | 默认值 / 范围 | 说明 |
|---|---|---|
| `--no-tcp-ping` | 关闭 | 跳过 TCP 连接预筛选。 |
| `--tcp-timeout <SECONDS>` | `3`；`0.1..=3600` | 单次 TCP 连接超时。 |
| `--tcp-concurrency <N>` | `1000`；`1..=65536` | 并行 TCP 探测数。 |
| `--tcp-retries <N>` | `1`；`1..=100` | 每个代理地址的连接尝试次数；使用成功尝试中的最佳 RTT。 |
| `--tcp-max-ms <MS>` | `0`；无额外上限 | TCP 阶段允许的最大 RTT；`0` 表示不按此项过滤。 |
| `--max-ping <MS>` | `0`；无额外上限 | TCP 阶段之后的最大 RTT；`0` 表示关闭。 |

### GeoIP

| 参数 | 默认值 / 范围 | 说明 |
|---|---|---|
| `--no-geoip` | 关闭 | 禁用 GeoIP、国家赋值和 `[XX] - NNN` 重命名。 |
| `--geoip-db <FILE>` | 程序所在目录中的 `geoip.dat` | 自定义 GeoIP 数据库路径。 |
| `--geoip-url <URL>` | `https://raw.githubusercontent.com/runetfreedom/russia-v2ray-rules-dat/release/geoip.dat` | 缺少或无效数据库时使用的下载地址。 |

## 输出文件与路径

默认输出相对于启动程序时的当前工作目录；可以用对应参数指定其他路径。文本输出通过临时文件和重命名原子更新，减少中途失败导致的半写文件。

| 文件 | 默认路径 | 内容与写入时机 |
|---|---|---|
| 原始订阅 | `raw.txt` | 成功下载的订阅原文；下载阶段结束且有内容时写入。直接链接文件本身不复制到此文件。 |
| 解析结果 | `parsed.txt` | 解析并去重后的代理；解析后写入，GeoIP 阶段完成后再次写入更新的显示名。 |
| 存活结果 | `alive.txt` | 最终存活节点；默认速度检查开启时为通过速度门槛的节点，`--no-speed` 时为剩余候选。若同时关闭 TCP 和速度检查，候选节点未经连通性验证；没有 TCP 存活节点时不会创建或更新该文件。 |
| 合格结果 | `good.txt` | 达到最低速度门槛的节点，按速度排序；`--no-speed` 不写入。 |
| 死节点 | 未创建 | 只有提供 `--dead <FILE>` 才写入；包含筛选失败节点的分享链接。 |
| Clash/mihomo | 未创建；`--clash` 无文件参数时为 `clash.yaml` | 只从 `good` 集合生成。没有合格节点时跳过生成。 |
| Xray 临时文件 | `.proxy_runtime/` | 运行期间的批次配置；通常在批次结束后清理。 |
| Xray 核心 | 默认与可执行文件同目录 | 文件名按平台为 `xray` 或 `xray.exe`。 |
| GeoIP 数据库 | 默认与可执行文件同目录下的 `geoip.dat` | 无效或缺失时尝试从 `--geoip-url` 下载。 |

对 `--sources` 和 `--core` 的相对路径，会先检查当前工作目录中的路径，再检查可执行文件所在目录，最后检查可执行文件旁的 `bin/` 目录。显式提供的绝对路径按指定位置使用。自定义输出路径不会自动重定向到程序目录。

## Xray 与 GeoIP

### Xray

启用速度检查时，程序确认 Xray 核心存在且可执行；无效或缺失时，默认下载对应的 ZIP 发行包、提取核心并验证。可用 `--xray-url` 指定其他归档地址，或用 `--no-xray-download` 要求只使用本地核心。

自动下载支持以下 OS/架构组合：

| 系统 | 架构 |
|---|---|
| Windows | x86_64、aarch64 |
| Linux | x86_64、aarch64 |
| macOS | x86_64、aarch64（Apple Silicon） |

速度阶段将候选节点分批写入 Xray 配置，经本地 SOCKS 入站下载测速数据。`--batch` 控制每批节点数，`--batch-workers` 控制批次并发，`--speed-threads` 控制速度检查并发。无法启动或配置无效的 outbound 会被跳过并计入汇总。

### GeoIP

默认启用。程序读取 `geoip.dat`，使用项目内的 protobuf 解析器读取 IPv4/IPv6 网络段，并为代理主机解析域名后分配国家或数据库中的标签。显示名按国家及序号改为 `[XX] - NNN`。本地/无法解析或未匹配的主机可能归入未识别标签。

GeoIP 数据库缺失或不能解析时，程序会尝试下载并验证数据库。可以用 `--geoip-db` 指向自有数据库、用 `--geoip-url` 更换下载地址，或使用 `--no-geoip` 完全关闭该阶段。

## Clash/mihomo 配置

通过 `--clash [FILE]` 生成 YAML。输入只包含通过 `--min-kb` 的 `good` 节点，因此必须启用速度检查且至少有一个节点合格。

支持的协议映射：

| Xray 协议 | Clash 类型 | 说明 |
|---|---|---|
| VLESS | `vless` | UUID、flow 及可用的 TLS/Reality 参数。 |
| VMess | `vmess` | UUID、cipher 与传输设置。 |
| Trojan | `trojan` | 密码；TLS SNI 使用 `sni` 字段。 |
| Shadowsocks | `ss` | method/cipher 和密码；对 method 做规范化。 |

传输网络支持 `raw`/`tcp`、`ws`、`grpc`、`httpupgrade`、`xhttp` 和尽力映射的 `kcp`。`httpupgrade` 转换为 WebSocket 形式并设置 `v2ray-http-upgrade`。Reality 的公钥从 Xray `realitySettings.password` 映射到 Clash `reality-opts.public-key`。VLESS/VMess 的 TLS 服务器名使用 `servername`；Trojan 使用 `sni`。未知协议会跳过并在生成报告中计数。

默认分组按以下大致顺序组织：

1. `🚀 Прокси`：顶层手动选择。
2. `🌍 Страны`：汇总国家选择器（存在国家分组时）。
3. `🔓 Заблокированные` 和 `🟢 Напрямую`：受限服务/直接路由手动选择组；直接组以 `DIRECT` 为首选。
4. `💬 Telegram`、`🤖 AI`、`📺 YouTube`、`🎮 Игры`：分类选择组，可用 `--clash-no-categories` 关闭。
5. `♻️ Fallback`、`⚡ Авто`、`⚖️ Баланс`：自动回退、延迟测试和负载均衡组。
6. 单独的国家或特殊标签组位于配置靠后位置；`🌐 Прочие` 收纳其他/未识别节点。`--clash-no-country` 关闭国家相关分组。

配置还包含项目模板中的 DNS、sniffer、profile 等全局段和规则。规则将私有网络直连、广告拒绝、俄罗斯站点/IP 路由到直接组；分类开启时，YouTube、Telegram 和 AI 服务进入对应分类。通用匹配规则最终指向 `🚀 Прокси`。没有 PROCESS-NAME 规则。

## Workspace 与模块架构

仓库是 Cargo workspace：根包 `proxy-rs` 是 CLI，成员 `proxy-clash` 是独立的 Clash/mihomo 生成库。

### 根包 `src/`

| 模块 | 职责 |
|---|---|
| `main.rs` | CLI 入口、Ctrl-C 处理和最终汇总显示。 |
| `pipeline.rs` | 阶段编排、checkpoint/resume、筛选和输出文件生成。 |
| `cli.rs` | Clap 参数定义、帮助文本和参数范围校验。 |
| `sources.rs` | 读取 `sources.json`、额外 URL 和文件输入并去重订阅地址。 |
| `fetch.rs` | 并发获取订阅文本并处理 Base64 订阅内容。 |
| `parse.rs` | 从文本提取链接、解析协议字段、规范化并对代理去重。 |
| `geoip.rs` | 下载/读取 GeoIP 数据库、解析 IPv4/IPv6 段并分配国家。 |
| `tcp_ping.rs` | 并发 TCP 连接探测、重试、RTT 统计和预筛选。 |
| `outbound.rs` | 将解析到的节点生成 Xray outbound/stream 配置。 |
| `speed.rs` | 通过 SOCKS 流程下载数据并计算速度。 |
| `xray.rs` | 管理 Xray 核心、临时配置、批次运行与速度阶段并发。 |
| `check.rs` | 本地 SOCKS 客户端和测速连接辅助逻辑。 |
| `output.rs` | 格式化代理行、原子写文件及终端摘要。 |

### `proxy-clash/`

纯 Rust 库，将 `Source`（协议、名称、服务器、端口、国家、Xray settings/stream JSON）转换为 YAML 和报告，不负责网络或文件 I/O。

- `convert.rs`：协议、TLS、Reality 和传输参数映射。
- `groups.rs`：国家/特殊标签、手动选择器及自动代理组。
- `rules.rs`：全局 Clash 配置、DNS 和路由规则。
- `lib.rs`：公共数据结构与 `render()` 接口。

根包的 `build.rs` 会将 `bin/sources.json` 复制到对应 Cargo profile 输出目录。根包使用 `reqwest` 的 rustls TLS；`serde_yaml_ng` 仅由 `proxy-clash` 使用。

## 开发、测试与 CI

```sh
# 格式检查
cargo fmt --all -- --check

# 全 workspace 测试
cargo test --workspace

# Clash 生成库测试
cargo test -p proxy-clash

# Clippy；CI 将警告视为错误
cargo clippy --workspace --all-targets -- -D warnings

# Release 构建
cargo build --workspace --release
```

集成测试使用本地 fixtures/监听器验证 CLI、订阅读取、GeoIP、速度流程和 Clash 输出；普通测试不需要启动公共代理聚合流程。

测试还包含确定性的 fuzz-style 语料，用于检查损坏的代理 URI 和 GeoIP protobuf 输入。它是固定随机种子的回归测试，不依赖独立 fuzzing runtime。合成基准测试覆盖 parse+dedup、GeoIP 和 Clash 生成：

```sh
cargo bench --bench pipeline
```

CI 工作流：

- `.github/workflows/cross-platform.yml`：Ubuntu、macOS、Windows 矩阵；运行 Clippy、release 构建和 workspace 测试。另有 Ubuntu 覆盖率与 mutation testing job；覆盖率命令包含 `--fail-under-lines 97`。
- `.github/workflows/release.yml`：推送 `v*` 标签后运行，在 Linux、macOS 和 Windows 上构建程序与文档压缩包，生成 SHA-256 校验和，并创建供人工检查的 GitHub Release 草稿。
- `.github/workflows/clash.yml`：每小时运行或手动触发；手动触发默认使用全部来源集合和 25000 个节点上限。在 Ubuntu 上构建并生成 Clash 配置及结果文件，然后 force-push 到专用于生成产物的 `dist` 分支。这个工作流会请求公共订阅与代理网络。

## 常见问题排查

### 找不到 `sources.json`

确认文件存在且 JSON 根节点为对象；确认所选键存在且其值是数组。可以通过 `--sources /绝对/路径/sources.json` 指定文件，或只传 `-u`/`-f` 使用自己的输入。

### 没有找到合适的代理

检查文件中的协议前缀与 URI 格式；程序支持 VLESS、VMess、Trojan、Shadowsocks。确认订阅确实包含分享链接而不是 HTML 页面，并查看解析统计中的无效和不支持项目。

### 全部节点在 TCP 阶段被淘汰

检查服务器端口、DNS 和本机网络；尝试适当增加 `--tcp-timeout` 或 `--tcp-retries`，或暂时使用 `--no-tcp-ping` 分辨 TCP 预筛选问题。`--tcp-max-ms` 和 `--max-ping` 的 `0` 表示关闭对应延迟上限。

### `good.txt` 为空或没有生成

默认速度检查需要可运行的 Xray 和可达的代理/测速地址。降低 `--min-kb` 可放宽门槛；检查 Xray 启动错误、出口配置和 `--speed-url`。使用 `--no-speed` 时本来就不会写 `good.txt`，也不会生成 Clash 文件。

### Xray 下载或启动失败

检查网络是否可访问 XTLS/Xray-core release、当前 OS/架构是否在自动下载支持列表中，以及核心文件是否可执行。可以用 `--xray-url` 指定兼容发行包，或用 `--core /path/to/xray` 提供本地核心。`--no-xray-download` 下缺少有效核心会直接报错。

### GeoIP 数据库无法读取

确认 `geoip.dat` 是有效的 protobuf 数据库且程序有读取权限。可指定 `--geoip-db` 或 `--geoip-url`；若不需要国家命名，使用 `--no-geoip`。

### 输出文件没有出现在预期位置

相对输出路径相对于启动时的当前工作目录，而不是源码目录或可执行文件目录。检查当前目录和写入权限，或为 `--output`、`--good-output`、`--parsed-output`、`--raw-output`、`--dead`、`--clash` 提供绝对路径。

### 终端里看不到进度条

`indicatif` 进度条只在交互式 TTY 中显示。通过管道或 CI 运行时不会绘制进度条；速度阶段会周期性输出文本进度。

### YAML 中少了某些代理或分组

Clash 仅从达到速度门槛的 `good` 集合生成。未知代理协议会跳过；`--clash-no-country` 和 `--clash-no-categories` 会分别关闭对应分组。检查生成汇总中的代理数、跳过数和分组数。
