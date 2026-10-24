# codeg-relay

## 中文

让 **Codeg-Fork iPhone app** 通过端到端加密连接家里的官方 codeg 桌面应用。独立家庭 agent 主动连出，不替换、不重新签名桌面应用，也不需要给家庭电脑开放入站端口。

```text
iPhone ⇄ blind relay（Linux VPS）⇄ home agent（Windows/macOS/Linux）⇄ codeg :3080
       └──────── 手机 TLS 在 home agent 终止，relay 只转发密文 ────────┘
                                 agent 主动建立固定 relay 证书的 WSS 隧道
```

| 组件 | 职责 |
| --- | --- |
| [codeg-relay](crates/codeg-relay/README.md) | VPS 上的单端口 TLS 分流与盲转发服务。 |
| [codeg-relay-agent](crates/codeg-relay-agent/README.md) | 家庭端连接器，终止手机 TLS、验证 codeg web token、代理本地 HTTP/WebSocket。Windows 包另含无控制台入口 `codeg-relay-agentw.exe`。 |
| [codeg-tunnel](crates/codeg-tunnel/README.md) | 两端共享的多路复用、流控与心跳库。 |

**一个 relay 实例只服务一个家庭端。** 新 agent 会以 WebSocket close `4001 replaced` 替换旧连接。Mac + Windows 需要两个 VPS relay，或两个独立配置的 relay 实例；手机为每台家庭电脑保存单独条目。本仓库安装器管理固定的一套路径和服务，一个 VPS 安装一个实例；同主机多实例需自行设计隔离，不能重复运行安装器来实现。

手机固定 **home 证书指纹**，agent 固定 **relay 证书指纹**，两者不可互换。Relay 能看到 IP、流量大小、时间及在线状态，能阻断连接；不能读取手机 TLS 明文。Web token 保护业务 API/WS，兼容上游的公开资源与接口例外。家庭电脑和手机是可信端点，最后一跳默认是 loopback HTTP。详见 [SECURITY.md](SECURITY.md)。

### 快速开始

1. 在新 VPS 按 [部署指南](docs/relay-vps.md) 安装 relay，保留 URL、relay 指纹和私有 agent key 文件。
2. 官方桌面应用打开 **设置 → Web 服务**，启用“Codeg 启动时自动开启 Web 服务”；默认端口 `3080`。同页显示 web token，勿粘贴到聊天。
3. 按 [Windows](docs/home-windows.md) 或 [macOS/Linux](docs/home-macos-linux.md) 安装独立 agent，先前台验证，再安装用户服务。
4. 按 [iPhone 配对指南](docs/pair-phone.md) 扫描 agent 生成的 QR，完成后删除配对文件。

AI 编码代理从 Windows 执行整个流程时，使用 [AGENTS.md](AGENTS.md)：含输入、命令、逐阶段检查点、失败恢复与卸载。

### 发布文件

[Releases](https://github.com/PanAndDuck/codeg-relay/releases) 的首个计划标签为 `v0.1.0`，后续使用 `vX.Y.Z`；`-rc`/`-beta` 为预发布。首次发布完成前下载链接可能返回 404，不能把未下载的文件当作已安装。

| 平台/用途 | 精确文件名 |
| --- | --- |
| Linux VPS x64 | `codeg-relay-x86_64-unknown-linux-musl` |
| Linux VPS ARM64 | `codeg-relay-aarch64-unknown-linux-musl` |
| VPS 安装脚本包 | `codeg-relay-deploy.tar.gz` |
| Windows 10/11 x64 | `codeg-relay-agent-x86_64-pc-windows-msvc.zip` |
| macOS Apple Silicon | `codeg-relay-agent-aarch64-apple-darwin.tar.gz` |
| macOS Intel | `codeg-relay-agent-x86_64-apple-darwin.tar.gz` |
| Linux 家庭端 x64 | `codeg-relay-agent-x86_64-unknown-linux-musl.tar.gz` |
| Linux 家庭端 ARM64 | `codeg-relay-agent-aarch64-unknown-linux-musl.tar.gz` |

每个文件都有同名 `.sha256`；`SHA256SUMS` 覆盖八个文件及其八个校验文件。下载格式：`https://github.com/PanAndDuck/codeg-relay/releases/latest/download/<asset>`；固定版本：`https://github.com/PanAndDuck/codeg-relay/releases/download/v0.1.0/<asset>`。同次安装应使用同一个版本，先验证摘要再解压或执行。

本地验证包含 macOS 构建和 CLI、Windows 服务 dry-run；Windows 原生运行与完整发布矩阵由 CI 验证，真实登录自启、VPS NAT 和手机配对需部署时验收。

### 许可

[Apache-2.0](LICENSE)。组件来源与上游归属见 [NOTICE](NOTICE)。

## English

This repository connects the **Codeg-Fork iPhone app** to the official codeg desktop app through end-to-end encryption. The standalone home agent makes outbound connections, keeps the official app intact, and needs no inbound port on the home computer. In the diagram above, phone TLS terminates at the home agent; the relay forwards ciphertext through the agent's pinned WSS tunnel. The final hop defaults to `http://127.0.0.1:3080`.

The three components are the Linux VPS relay, the Windows/macOS/Linux home agent, and the shared multiplexing library. The Windows ZIP includes both the console CLI and `codeg-relay-agentw.exe`, its background entry point.

**One relay instance serves one home.** A second agent replaces the first with WebSocket close `4001 replaced`. Use two VPS relays, or independently configured instances, for a Mac and a Windows PC; keep separate phone server entries. The supplied installer manages fixed paths and one service per VPS. Running it twice does not create a second instance.

Phones pin the **home** certificate; agents pin the **relay** certificate. The relay sees connection metadata and can deny service, but cannot decrypt phone traffic. The codeg web token gates protected API/WS requests, with upstream public-route exceptions. Read the full [security model](SECURITY.md).

1. Follow [VPS deployment](docs/relay-vps.md), retaining the relay URL, relay fingerprint and private agent-key file.
2. Enable **设置 → Web 服务** in official codeg, including **Codeg 启动时自动开启 Web 服务**. The default port is `3080`; the same page shows the web token. Keep it out of conversations.
3. Install the agent using the [Windows](docs/home-windows.md) or [macOS/Linux](docs/home-macos-linux.md) guide; verify foreground operation before installing its user service.
4. Follow [phone pairing](docs/pair-phone.md), then delete the pairing files. An AI agent operating from Windows can follow the complete [runbook](AGENTS.md).

The release table above lists all eight exact asset names. Each has a `.sha256` sidecar; `SHA256SUMS` covers the eight assets and eight sidecars. The first planned tag is `v0.1.0`, followed by `vX.Y.Z`; `-rc`/`-beta` tags are prereleases. Use the `latest/download/<asset>` or version-pinned `download/v0.1.0/<asset>` URL shown above. A 404 before the first release means the asset is unavailable. Download matching versions and verify before extraction or execution.

macOS builds, CLI output and cross-platform service previews have local validation. Native Windows behavior and the full release matrix require CI; actual login startup, VPS NAT and phone pairing require deployment acceptance. Licensed under [Apache-2.0](LICENSE); see [NOTICE](NOTICE) for attribution.
