# codeg-relay-agent

## 中文

独立 Rust 家庭连接器，与**官方、未修改的 codeg 桌面应用**并行运行，无 src-tauri 依赖。它主动建立固定 relay 证书的 WSS 隧道，在家庭电脑终止手机 TLS，验证 web token 后代理 HTTP/1.1、SSE 和 WebSocket 到本地 codeg。不开入站监听，不支持 legacy plaintext relay。

操作指南：[Windows](../../docs/home-windows.md)、[macOS/Linux](../../docs/home-macos-linux.md)、[手机配对](../../docs/pair-phone.md)。端到端自动化见根 [AGENTS.md](../../AGENTS.md)，信任边界见 [SECURITY.md](../../SECURITY.md)。一个 relay 只允许一个 home agent，新连接以 `4001 replaced` 替换旧连接。

### 构建与 CLI

在仓库根目录使用 Rust stable；每个 crate 有独立 Cargo.lock，不使用根 workspace：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
cargo build --locked --release --bins --manifest-path crates/codeg-relay-agent/Cargo.toml
crates/codeg-relay-agent/target/release/codeg-relay-agent --help
```

| 命令 | 行为 / 参数 |
| --- | --- |
| `init` | 必需 `--relay-url`、`--agent-key-file`、`--relay-fingerprint`；只创建 agent 配置及 home 身份，不读取 token、不启动服务，已有配置拒绝覆盖。 |
| `run` | 前台运行，Ctrl-C 正常停止；`--log-dir` 改为轮转文件日志，默认 stderr。 |
| `pair` | `--name`、`--png`、`--json-only`、`--json-file`；默认会输出含 token 的 JSON。自动化必须同时使用 `--png --json-only --json-file`，见配对指南。拒绝覆盖现有导出件。 |
| `status` | 私有本地 JSON 快照，每秒更新；超过5秒显示 `stopped_or_stale`。无 token/key，含 PID、状态、计数、指纹，Windows 还显示 state/log路径。 |
| `install-service` / `uninstall-service` | 支持 `--dry-run`；`--platform macos\|linux\|windows` 仅可用于 dry-run。卸载保留 state/身份/日志。 |
| `rotate-cert` | 轮换 home cert/key，运行实例两秒内重载，现有连接自然结束；每台手机须重新配对。 |
| `help` / `--help` / `--version` | 帮助与版本；例如 `codeg-relay-agent init --help`。 |

全局 `--state-dir` 可用于任何子命令，使用绝对路径最清楚。Windows 发布包包含两个 exe：`codeg-relay-agent.exe` 用于 CLI；同目录 `codeg-relay-agentw.exe` 用于无控制台后台运行，后者只有 `--state-dir`、`--log-dir`、帮助和版本选项，无子命令。安装正式用户服务前先将 binary 放到稳定路径，不指向下载临时目录或开发 worktree。

### 路径与 token 来源

| 平台 | 默认 state | 默认只读 DB | 默认服务日志目录 |
| --- | --- | --- | --- |
| Windows | `%LOCALAPPDATA%\codeg-relay-agent` | `%APPDATA%\app.codeg\codeg.db` | `%LOCALAPPDATA%\codeg-relay-agent\logs` |
| macOS | `~/Library/Application Support/codeg-relay-agent` | `~/Library/Application Support/app.codeg/codeg.db` | `~/Library/Logs/codeg-relay-agent` |
| Linux | `$XDG_STATE_HOME/codeg-relay-agent`，否则 `~/.local/state/codeg-relay-agent` | `~/.local/share/app.codeg/codeg.db` | `~/.local/state/codeg-relay-agent/logs` |

配置为 state 的 `config.json`，home 身份为 `relay/home-tls/{cert.pem,key.pem}`。自定义 state 不必然改变服务日志目录。Windows 缺环境变量时使用明确 `--state-dir`/`init --db-path`/`init --token-file`，不要猜其他用户位置。Unix 凭据0600、state0700，文件属于当前用户；Windows 继承私有 profile ACL，自定义导入/导出目录同样必须私有，不自动重写 ACL。拒绝相关 symlink/reparse point。

DB 使用 SQLite `mode=ro`、只读标志，仅查询 `app_metadata.web_service_token`，不创建 DB、不迁移、不写 token。`init --token-file` 与 `--db-path` 互斥；使用环境 token 的特殊 codeg 实例应提供匹配的私有 token 文件。默认每60秒刷新；失败清空缓存并拒绝受保护请求，恢复来源后自动恢复。配置改变需重启；token 刷新和 home 证书轮换无需重启。

### init 参数参考

| 参数 | 默认/语义 |
| --- | --- |
| `--relay-url` | 必填，HTTPS/WSS relay origin；使用公网映射端口。 |
| `--agent-key-file` | 必填，私有文件；不是 web token。 |
| `--relay-fingerprint` | 必填，relay leaf DER SHA-256；示例占位 `<fingerprint>` 必须替换为可信获取值。 |
| `--public-url` | 手机配对 URL，默认 relay URL；特殊路由场景才覆盖。 |
| `--upstream` | `http://127.0.0.1:3080`，HTTP(S) origin，不带 path prefix。 |
| `--upstream-fingerprint` | 可选 HTTPS leaf pin；无 pin 时正常 WebPKI。 |
| `--db-path` / `--token-file` | 二选一覆盖 token 来源；路径含空格时整体引用。 |
| `--buffer-bytes` | `8388608`，每流每方向8 MiB，范围1024–134217728。 |
| `--stall-timeout-secs` | `30`，范围1–3600秒；写入/缓冲停滞超时关闭两端。 |
| `--token-refresh-secs` | `60`，范围1–60秒。 |
| `--auth-failures` | `10`，范围1–10000。 |
| `--auth-window-secs` | `600`，范围1–86400秒。 |
| `--auth-block-secs` | `900`，范围1–86400秒。 |

### 服务与协议边界

- Windows：当前用户 Scheduled Task `app.codeg.relay-agent-<SID>`，登录触发、InteractiveToken、LeastPrivilege，无密码；调用同目录后台 exe。XML 位于 state，UTF-16LE/BOM；dry-run 不创建文件。`/Create` 后立即 `/Run`，卸载 `/End` 后 `/Delete`。重复卸载已不存在任务可能返回错误，查询确认即可，不能屏蔽其他错误。
- macOS：用户 LaunchAgent `app.codeg.relay-agent`，RunAtLoad/KeepAlive，10秒重启节流。
- Linux：`app.codeg.relay-agent.service`，仅 systemd user，5秒重启；安装器不启用 linger。
- 三平台文件日志 `agent.log`，5 MiB + 3个归档。Windows 用户必须登录，其他平台遵循用户会话生命周期，睡眠停止访问。桌面应用/Web 服务仍需运行。

手机 TLS 仅 HTTP/1.1；agent TLS ALPN 为 `codeg-relay-agent/1`。默认最多256 streams、每IP64；8 MiB是每流每方向上限，不是进程总内存，还有 TLS/mux/系统开销。流式 body/SSE 不整包缓存；WebSocket 101 后双向转发。有限缓冲满后可能短暂 TCP 回压，超过30秒默认 stall timeout 关闭该连接，其他流继续。

心跳15秒、45秒空闲断开；重连退避1/2/4/8/16/30秒，±20% jitter、硬上限30秒；稳定60秒后重置；relay 401/403 退避5分钟。业务鉴权和公开路由语义、10次/10分钟→15分钟限流及“正确 token 始终放行但不清空失败状态”见 SECURITY.md。

### 开发验证

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
cargo test --locked --manifest-path crates/codeg-relay-agent/Cargo.toml
cargo clippy --locked --manifest-path crates/codeg-relay-agent/Cargo.toml --all-targets -- -D warnings
cargo fmt --manifest-path crates/codeg-relay-agent/Cargo.toml -- --check
```

集成测试使用临时凭据/DB、loopback 临时端口和 fake upstream，不使用用户桌面实例。Windows 专属测试只能在 Windows 执行；macOS 的 Windows 服务预览不能证明登录自启/关机行为。

## English

The standalone Rust connector keeps official codeg intact, has no src-tauri dependency and opens no inbound listener. It dials pinned WSS, terminates phone TLS locally, authenticates protected requests and proxies HTTP/1.1/SSE/WebSocket traffic. There is no legacy plaintext fallback. A relay serves one home; another agent replaces it with `4001 replaced`. Follow the [Windows](../../docs/home-windows.md), [Unix](../../docs/home-macos-linux.md), [phone](../../docs/pair-phone.md), [AI runbook](../../AGENTS.md) and [security](../../SECURITY.md) guides.

Build from the repository root using the command above; each crate has its own lockfile. The CLI commands are `init`, `run`, `pair`, `status`, `install-service`, `uninstall-service`, `rotate-cert`, and `help`. All support global `--state-dir`; use `init --help` for exact options. Init requires relay URL, key-file path and relay fingerprint, writes identity/config only and refuses existing config. Run reads the token and logs to stderr unless `--log-dir` is set. Status is a local snapshot, stale after five seconds, containing metadata/fingerprints but no secrets. Rotate-cert reloads within two seconds and requires re-pairing all phones.

Pairing prints secret JSON by default: automation must combine `--png --json-only --json-file` and delete exports after use. Services support `--dry-run`; `--platform macos|linux|windows` is preview-only. Windows needs both sibling executables; `codeg-relay-agentw.exe` has only state/log options plus help/version, and no subcommands. Install from a stable path.

The path table above is authoritative. Windows uses LOCALAPPDATA state/logs and APPDATA DB; macOS uses Application Support state/DB and Library/Logs; Linux state honors an absolute XDG_STATE_HOME, while its service logs use the listed fixed default. Custom state does not necessarily move logs. Unix files require owner-only 0600/0700; Windows preserves private profile ACLs. Imported/exported paths must also be private and avoid redirects.

SQLite is opened read-only and only reads `app_metadata.web_service_token`, without creating/migrating the DB. `--db-path` and `--token-file` are mutually exclusive; explicit files also support codeg instances using an environment token. Default refresh is sixty seconds; failure clears the cache until recovery. Restart after config changes; token/home-cert refresh is automatic.

The init table lists exact flag names, defaults and bounds: required relay URL/key file/relay pin; optional public phone URL and HTTPS upstream pin; default loopback upstream; 8 MiB per-direction buffer, 30-second stall timeout, sixty-second refresh, and ten failures in 600 seconds with a 900-second invalid-credential block. Fingerprints are SHA-256 leaf pins, acquired through a trusted channel.

Windows registers a per-user SID-suffixed task using InteractiveToken/LeastPrivilege, runs the background exe immediately and at logon, and uses UTF-16LE/BOM XML. Uninstall ends/deletes the task but retains state/logs; a missing task can make repeated uninstall fail, so verify absence. macOS uses a KeepAlive LaunchAgent with ten-second throttle; Linux uses a systemd user unit with five-second restart and no automatic linger. Logs are 5 MiB plus three archives. Login/user-manager lifecycle and sleep still apply; codeg must remain running.

Phone TLS negotiates HTTP/1.1; agent ALPN is `codeg-relay-agent/1`. Up to 256 streams/64 per IP share the tunnel. Buffer limits are per stream/direction plus protocol/socket overhead, not a total process limit. Bodies/SSE stream and upgraded WebSockets splice bytes; stalled writes close both endpoints after the configured timeout without blocking other streams. Heartbeats are 15 seconds with a 45-second idle limit. Backoff is 1/2/4/8/16/30 seconds, ±20% jitter capped at 30; reset after sixty stable seconds; relay auth rejection waits five minutes. See SECURITY.md for precise public-route exceptions and valid-token limiter behavior.

Use the test/clippy/fmt commands above. Tests use temporary credentials, loopback ports and fake upstreams. Windows-only tests require Windows; cross-platform dry-runs do not prove actual logon/shutdown behavior.
