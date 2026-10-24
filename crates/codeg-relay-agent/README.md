# codeg-relay-agent

独立的 codeg E2EE connector，像 cloudflared 一样从家庭电脑主动连出。
继续使用**官方、未修改的 codeg desktop**：不重新签名、不替换应用、不接管更新。
Agent 只连接其本地 Web 服务（默认 `http://127.0.0.1:3080`）。

```text
phone -- TLS (pin home certificate) --> blind relay -- multiplexed WSS --> agent
                                                                        |
                                                              HTTP/1.1 / WebSocket
                                                                        |
                                                         official codeg :3080
```

## 中文

### 构建与初始化

需要 Rust stable；本 crate 有独立 Cargo.lock/workspace，无 src-tauri 依赖。
SQLite 随二进制构建。release 启用 strip、thin LTO、codegen-units=1。

每个新 shell 先清理 codeg 继承环境；以下命令从仓库根目录执行：

```sh
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
cargo build --locked --release --manifest-path crates/codeg-relay-agent/Cargo.toml
crates/codeg-relay-agent/target/release/codeg-relay-agent --help
```

将 binary 放在稳定的绝对路径后再安装 service；不要把正式 service 指向
可能删除的开发 worktree。准备 relay 的 agent key 文件（当前用户拥有、0600），
以及通过可信渠道获得的 **relay certificate SHA-256 fingerprint**。
Agent key 与 codeg web token 是两个不同的凭据。

```sh
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
codeg-relay-agent init \
  --relay-url https://relay.example:18443 \
  --agent-key-file /absolute/private/agent.key \
  --relay-fingerprint '<64-hex-relay-fingerprint>'
codeg-relay-agent run
```

`init` 只写 agent 配置、生成 home certificate，不读取 codeg token，不启动服务。
默认 state：macOS `~/Library/Application Support/codeg-relay-agent/`；Linux
`$XDG_STATE_HOME/codeg-relay-agent/` 或 `~/.local/state/codeg-relay-agent/`。
所有命令均可加全局 `--state-dir /absolute/agent-state`。

`run` 默认用 `mode=ro` 打开 macOS 官方 DB
`~/Library/Application Support/app.codeg/codeg.db`，仅查询
`app_metadata.web_service_token`，不创建 DB、不做迁移、不修改 token。
Linux 默认 DB 为 `~/.local/share/app.codeg/codeg.db`，可用 `init --db-path` 指定。
也可用 `init --token-file /absolute/private/web.token`（0600，当前用户拥有）。
若 upstream 使用 `CODEG_TOKEN` 覆盖 DB，使用对应的 `--token-file`。
本项目的测试始终使用临时 DB、token 和 fake upstream。

Token 默认每 60 秒重新读取；刷新失败即清空缓存并拒绝受保护请求，恢复文件/DB 后自动恢复。
配置与凭据文件要求私有权限；不会通过日志、status 或命令参数展示 token。
正常前台停止用 Ctrl-C；service 停止使用 SIGTERM。

### 配对与证书轮换

```sh
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
codeg-relay-agent pair --name Home --png /private/path/pair.png
# 仅私有文件，不把 JSON/QR 显示在终端：
codeg-relay-agent pair --json-only --json-file /private/path/pair.json
codeg-relay-agent status
codeg-relay-agent rotate-cert
```

`pair` 默认 stdout 输出 §4.4 JSON，stderr 输出 ANSI terminal QR；PNG/JSON 文件为0600，
拒绝覆盖现有文件。JSON 为 `v=1, mode=relay, url, token, fp, name?`；
`url` 是 relay public URL（可 `init --public-url`），**`fp` 是 home certificate**。
配对内容包含 web token，按凭据保管；只在需要配对时调用 `pair`。

Home certificate 为 ECDSA P-256、自签、CN/SAN `codeg-home`、10年有效期；
state 中 `relay/home-tls/{cert.pem,key.pem}` 为0600，目录0700。
轮换使用 staged directory + rename，运行中的 agent 在两秒内载入新证书。
所有手机必须重新配对；现有 TLS 连接可自然结束。
需要保留旧 pin 时，应私有备份/成对恢复 cert.pem + key.pem。
通用浏览器需另行信任 home certificate；relay 的证书不能替代 home pin。

### 服务

```sh
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
codeg-relay-agent install-service --dry-run
codeg-relay-agent install-service
codeg-relay-agent uninstall-service --dry-run
codeg-relay-agent uninstall-service
```

安装/卸载会打印实际文件与服务管理命令；dry-run 不创建文件、不执行服务命令。
可 `--dry-run --platform macos|linux` 预览另一平台。

- macOS：`~/Library/LaunchAgents/app.codeg.relay-agent.plist`，用户登录会话，
  KeepAlive、RunAtLoad、10秒 restart throttle。日志在
  `~/Library/Logs/codeg-relay-agent/agent.log`。
- Linux：`~/.config/systemd/user/app.codeg.relay-agent.service`，仅
  `systemctl --user`，5秒 restart delay。日志在
  `~/.local/state/codeg-relay-agent/logs/agent.log`。
- 两者均由 agent 直接轮换日志：每文件5 MiB、3个归档（最多20 MiB），文件0600。
  不把无限 stdout/stderr 交给服务管理器。前台 `run` 默认记录到 stderr，
  也支持 `run --log-dir /private/logs`。
- 卸载仅停止/移除该 service；保留 agent state、身份与日志。
  Linux user service 是否在退出登录后继续运行取决于系统的 user manager/linger 设置；
  安装器不改变系统级设置。macOS LaunchAgent 在登录后运行。

### 配置、鉴权与边界

配置在 state 的 `config.json`（0600）；调整后重启 agent，token/cert 轮换无需重启。
主要 init 参数：

| 参数 | 默认 | 含义 |
|---|---|---|
| `--upstream` | `http://127.0.0.1:3080` | HTTP(S) origin，无 path prefix |
| `--upstream-fingerprint` | 无 | HTTPS leaf pin；无 pin 时正常 WebPKI |
| `--buffer-bytes` | 8388608 | 每连接每方向8 MiB |
| `--stall-timeout-secs` | 30 | 缓冲/写入无进展达到期限即关闭两端 |
| `--auth-failures` | 10 | 一个失败窗口内达到次数后返回429 |
| `--auth-window-secs` | 600 | 失败统计窗口 |
| `--auth-block-secs` | 900 | 错误凭据封锁时长 |
| `--token-refresh-secs` | 60 | 可设1–60秒 |

上游 v0.39.0（`bed89314`）行为：

- 受保护的 `/api` 与 `/ws/events`、`/ws/browser-tunnel` 在连接 upstream **之前**检查 token。
  接受精确的 `Authorization: Bearer <t>`，或第一个
  `codeg-token.<base64url-no-padding(t)>` WebSocket subprotocol。
  不接受 URL query/cookie token；空配置 fail closed。
- 候选与预期 token 做 SHA-256 后以 `subtle` 比较固定32字节摘要。
  IPv4-mapped IPv6 合并计数；合法 token 始终放行，包括同 NAT。
  成功鉴权不清除或减少该 IP 的失败计数，也不解除或缩短封锁；
  错误凭据在封锁自然到期前仍返回429，失败记录按原统计窗口过期。
  IP 来自 OPEN frame，忽略客户端伪造的 X-Forwarded-For。
- 与 upstream 保持一致：401 是 **text/plain**（非 JSON），内容为
  `Invalid or missing token` 或 `Server token is not configured`，并有
  `WWW-Authenticate: Bearer realm="codeg"`；429 带 Retry-After。
- 静态资源与 upstream 的 public exceptions 保持公开：
  `/api/get_system_language_settings`、`/api/cli_hello`、`/api/forge_webhook`、
  单段 `/api/workspace_download/{ticket}`、`/api/backup_download/{ticket}`、
  `/api/office-watch-proxy/{port}` 及子路径。HMAC、ticket、capability 由 upstream 校验。
  OPTIONS 交给 upstream CORS。可选 server proxy-JWT 不能绕过 agent 的 web-token 检查。
- HTTP/1.1 keep-alive 每个 phone connection 复用一个 upstream connection；
  body/SSE/chunked streaming，不整包缓冲。WebSocket 101 后双向 byte splice。
  删除标准及 Connection 指名的 hop headers；WS upgrade 重新构造。
- 默认最多256 streams、每IP64。8 MiB/direction 的按需缓冲另有固定32 KiB pump块、
  hyper/TLS/mux 窗口与操作系统 socket 缓冲。有限无损缓冲满后 TCP 可能暂时回压，
  最多持续 stall timeout，随后关闭 phone 与其 upstream；不会无限等待或阻塞其他流。
  上述内存界限是每连接的，不是整个进程的8 MiB总上限。
- 新 relay TLS 协议专用 ALPN `codeg-relay-agent/1`；phone TLS 仅 `http/1.1`。
  **不支持 legacy plaintext relay**。心跳15秒、45秒无响应断开；重连1/2/4/8/16/30秒，
  ±20% jitter 且硬上限30秒；稳定60秒后reset；401/403退避5分钟。

### 威胁模型与排障

Relay 对 phone 流量不可见，只能观察 IP/时间/大小/在线状态，或中断连接。
手机 pin home certificate，agent pin relay certificate；agent 只在家庭机器终止 phone TLS。
Agent 和本机 codeg 属于可信端点。默认 agent→codeg 是 loopback 明文 HTTP；跨机器
upstream 应使用 HTTPS。Relay 提供的 IP 元数据不是端到端认证身份，不能替代 web token。

日志不记录请求路径/query、body、Authorization、WS protocol 或 key/token。
`status` 是每秒更新的私有本地快照，超过5秒显示 stopped_or_stale。

- 401：核对 token 来源、DB 路径、0600 权限及 token 是否旋转；最多等60秒或重新配对。
- 429：错误凭据达到阈值；合法 token 仍放行但不重置限流状态，错误凭据须等封锁到期。
- 502：确认官方 codeg 已开启 Web 服务，upstream URL、HTTPS信任/pin 正确。
- relay backoff：检查 relay地址、relay pin、agent key文件权限、网络；旧 relay 不支持本 agent。
- certificate changed：重新执行 pair；不要用 relay fingerprint 给手机配对。
- buffer stalled：phone 消费速度不足；该连接已释放，其他连接继续运行。可调整缓冲/期限。
- service 无新日志/status：在前台 `run` 查看配置/文件权限错误，核对 service binary 路径。

## English

This standalone connector keeps the official, signed, unmodified codeg desktop
app in place. It dials the existing relay over pinned WSS, terminates each
phone's TLS stream locally with a persistent home certificate, authenticates
protected requests, then proxies HTTP/1.1 and WebSockets to codeg. It has no
src-tauri dependency and opens no inbound listener.

Build with the Cargo command above, place the release binary at a stable path,
and run `init` with the relay URL, a private agent-key file, and its SHA-256
certificate fingerprint. All commands accept `--state-dir`. `init` creates
only agent state; `run` reads the codeg token. macOS defaults to the official
`~/Library/Application Support/app.codeg/codeg.db`, opened with `mode=ro`;
use `--db-path` or a 0600 `--token-file` for another installation. Token files
are also appropriate when codeg-server uses an environment token. Values are
reread every 60 seconds; failures revoke the cached token until recovery.

Run `pair` for pairing JSON and an ANSI terminal QR, with optional `--png`.
`--json-only --json-file /private/pair.json` avoids displaying the credential.
The JSON uses the relay public URL and **home** fingerprint, with the existing
v1 relay schema. Pairing files are credentials and are saved 0600 without
overwriting existing files. `rotate-cert` requires re-pairing all phones;
running agents reload within two seconds. `status` displays a private runtime
snapshot. Ordinary browsers must separately trust the self-signed home cert.

`install-service --dry-run` previews a user LaunchAgent on macOS or a systemd
user unit on Linux; omit `--dry-run` to write/start it. `uninstall-service`
stops/removes only this service and preserves state. `--platform macos|linux`
is available for dry runs. Logging is capped at 5 MiB per file with three
archives, under `~/Library/Logs/codeg-relay-agent/` on macOS or
`~/.local/state/codeg-relay-agent/logs/` on Linux. Foreground logging goes to
stderr unless `run --log-dir` is supplied. User-manager/login lifecycle rules
still apply; the installer does not enable system-wide lingering.

Protected API/WS traffic is authenticated before any upstream connection.
Extraction matches upstream v0.39.0: exact Bearer syntax or the first
`codeg-token.` base64url subprotocol; no query/cookie tokens. Comparison uses
constant-time equality of SHA-256 digests. The per-IP limiter defaults to
10 failures in 10 minutes and a 15-minute block; correct credentials always
pass without clearing or reducing that IP's failure count or block. Invalid
credentials remain blocked until the original deadline, and failure history
still expires with the normal window. Upstream's public route exceptions,
static assets and CORS OPTIONS remain public; their ticket/HMAC/capability
checks remain upstream. The
observed upstream 401 is text/plain, with `WWW-Authenticate: Bearer realm="codeg"`.
Opt-in server proxy-JWT does not bypass the connector's token gate.

Each phone connection reuses an upstream HTTP/1.1 connection, streams bodies
and SSE, and supports WebSocket upgrades. Buffers default to 8 MiB per
direction; stalled writes close both endpoints after 30 seconds. A finite
lossless buffer can only absorb a bounded burst: once saturated, TCP can
apply backpressure until that deadline. Other streams continue independently.
Buffer limits are per connection, plus fixed protocol/socket overhead; up to
256 streams (64 per IP) are admitted. Adjust limits using the init flags in
the table or private config.json, then restart. HTTPS upstream supports normal
WebPKI or an optional leaf fingerprint pin.

The relay remains blind to phone plaintext. Home and relay pins are separate;
the agent and codeg host are trusted endpoints. Loopback HTTP is the default
last hop; use HTTPS for a remote upstream. Relay-supplied IP metadata is not
an authenticated user identity. No legacy plaintext mode is supported.
Logs omit paths, queries, payloads and credential headers. For failures, check
token source/permissions (401), Retry-After (429), the local Web service or
upstream TLS trust (502), relay key/pin/network (backoff), or re-pair after a
home certificate change. Services should use a durable binary path.

## Verification

```sh
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
export CODEG_DATA_DIR="$(mktemp -d)" CODEG_HOME="$(mktemp -d)"
cargo test --locked --manifest-path crates/codeg-relay-agent/Cargo.toml
cargo clippy --locked --manifest-path crates/codeg-relay-agent/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path crates/codeg-relay/Cargo.toml
cargo clippy --locked --manifest-path crates/codeg-relay/Cargo.toml --all-targets -- -D warnings
cargo test --locked --manifest-path crates/codeg-tunnel/Cargo.toml
cargo clippy --locked --manifest-path crates/codeg-tunnel/Cargo.toml --all-targets -- -D warnings
```

Integration tests embed the real relay, real agent, and a fake HTTP(S)/WS
upstream, using loopback ephemeral ports and temporary credentials only.
They include a 12 MiB download, chunked upload, SSE timing, WS echo/push,
intercepted auth failures, rotation, saturation/cancellation and relay restart.
The relay library entry point shares its production CLI server implementation;
the multiplexing protocol and codeg-tunnel are unchanged.
