# codeg-relay

## 中文

Linux VPS 单端口盲 relay。独立 `codeg-relay-agent` 从家庭电脑主动连出；relay 将手机 TLS 字节原样通过 `codeg-tunnel` 转发，TLS 在家庭 agent 终止。一个实例一个家庭端，新 agent 以 `4001 replaced` 替换旧连接。

部署使用 [VPS 指南](../../docs/relay-vps.md) 与完整 [deploy kit](deploy/README.md)。家庭端用 [Windows](../../docs/home-windows.md) / [macOS/Linux](../../docs/home-macos-linux.md) 指南，不在桌面应用里配置“内置 relay”。完整威胁模型见 [SECURITY.md](../../SECURITY.md)。

### 构建与 CLI

从仓库根目录：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
cargo build --locked --release --manifest-path crates/codeg-relay/Cargo.toml
crates/codeg-relay/target/release/codeg-relay --help
```

| 子命令 | 参数与说明 |
| --- | --- |
| `serve` | `--listen` 默认 `0.0.0.0:18443`；`--agent-key-file` 选择文件，否则读取 `CODEG_RELAY_AGENT_KEY`。 |
| `serve` TLS | `--self-signed --public-host IP[,DNS]`，或 `--tls-cert PATH --tls-key PATH`；自签状态由 `--data-dir` 指定，默认 `/var/lib/codeg-relay`。 |
| `serve` 限制 | `--max-conns` 默认/最大512，`--max-conns-per-ip` 默认/最大64，可调小。`--insecure-plaintext` 仅 loopback 测试，不能用于生产 E2EE agent。 |
| `fingerprint` | `--data-dir` 默认 `/var/lib/codeg-relay`，或 `--tls-cert PATH`；输出 leaf DER SHA-256，第一行64位小写hex，第二行冒号格式。 |
| `gen-key` | 无子选项，生成32随机 bytes 的 base64url key；stdout 必须重定向到私有文件，禁止贴聊天。 |
| `help` / `--help` / `--version` | 帮助与版本；例如 `codeg-relay serve --help`。 |

自签证书/私钥在 data-dir 的 `tls/{cert,key}.pem`，重启复用；改变 public-host 不轮换既有证书。Relay 证书变化只需更新 agent 的 relay pin，手机 pin 的是独立 home 证书。安装器的公网端口选项与 binary 的 `serve --listen` 是两层配置，不能把 `--public-port` 传给 binary。

### 网络与运维契约

单 listener 在 TLS 前检查 ClientHello（10秒、16 KiB，支持跨 record）。ALPN `codeg-relay-agent/1` 进入 relay 终止的 agent TLS/WSS；其他 TLS 原样转发到 home，手机与 home 协商 HTTP/1.1。不是 HTTPS 反向代理，不在 nginx 上终止手机 TLS。

`GET http://relay.example.com:18443/__codeg_relay/v1/health` 公开报告 status/version/mode=e2ee/agent_connected。其他明文 GET/HEAD 为404；家庭端离线或超限时 TLS 连接直接关闭，不返回 HTTP 503 JSON。公共 health 未认证，不是身份验证。最多256 mux streams；15秒心跳、45秒空闲；SIGINT/SIGTERM 停止接收，最多等5秒再取消剩余连接。

日志仅记录 IP、分类、字节数、时长、固定原因，不解析手机请求行。`RUST_LOG` 可控制本项目日志，依赖库原始报文日志仍过滤。安装器资源限制和安全边界见部署指南；有限内存不保证最大 streams 同时满速。

## English

The single-port Linux VPS relay forwards opaque phone TLS through `codeg-tunnel` to the standalone home agent. One relay serves one home; a new agent replaces the old with `4001 replaced`. Use the [VPS guide](../../docs/relay-vps.md), [deployment kit](deploy/README.md) and separate [Windows](../../docs/home-windows.md)/[Unix](../../docs/home-macos-linux.md) agents. No built-in desktop relay setup is required.

Build with the locked Cargo command above. The CLI table gives exact subcommands/options: `serve` binds by default to `0.0.0.0:18443`, reads a key file or CODEG_RELAY_AGENT_KEY, and uses either self-signed public-host certificates or a supplied TLS pair. Data defaults to `/var/lib/codeg-relay`; connection limits default/cap at 512 total and 64/IP. `--insecure-plaintext` is loopback-test-only. `fingerprint` reads the data directory or explicit certificate and prints SHA-256 in raw/colon formats. `gen-key` emits a random base64url key; redirect stdout into a private file. The installer-only `--public-port` is not a binary serve option.

Self-signed identity lives in `tls/{cert,key}.pem` and persists across restarts; changing public-host does not rotate it. Updating a relay pin affects agents; phones pin the separate home identity. The listener classifies ClientHello within ten seconds/16 KiB, including split records. Agent ALPN terminates WSS at relay; other TLS is passed unchanged to home. Do not terminate phone TLS at an HTTP reverse proxy.

Public plaintext HTTP health returns status/version/mode/agent_connected without authentication. Other plaintext GET/HEAD requests return 404; offline/over-limit TLS closes without HTTP JSON. Mux allows 256 streams with 15-second heartbeat/45-second idle deadline. Shutdown waits up to five seconds. Logs contain connection metadata and fixed reasons, not phone request lines; dependency wire logs remain filtered even with RUST_LOG overrides. See [SECURITY.md](../../SECURITY.md) for the full trust model.
