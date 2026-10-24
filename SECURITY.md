# 安全 / Security

## 中文

### 信任与边界

手机到 home agent 的 TLS 穿过盲 relay，**手机固定 home 证书**；agent 在发送 agent key 前验证 **relay 证书固定值**。两份证书独立。被攻陷的 relay 可以观察 IP/时间/流量大小/在线状态、阻断或延迟连接，不能读取手机 TLS 明文或冒充已固定 home 身份。它提供的客户端 IP 不是端到端认证身份，只能辅助限流。公网 HTTP health 公开、未认证，不可用于建立证书信任。

手机、家庭 agent、官方 codeg 和家庭操作系统属于可信端点；本机恶意软件、管理员或泄漏的 home 私钥不在“盲 relay”保护范围。默认 agent → codeg 使用 `http://127.0.0.1:3080`，跨机器 upstream 应使用 HTTPS（可选 `--upstream-fingerprint`），不能把明文 HTTP 暴露到公网。当前 agent 不支持旧的 plaintext relay。

### Web token 与限流

所有**受保护**的业务 API 和 WebSocket 在连接上游前验证 codeg web token。接受精确 Bearer header，或 WebSocket 的第一个 `codeg-token.` base64url 子协议；不从 query/cookie 取 token，空 token 拒绝请求。比较 SHA-256 固定长度摘要；可选上游 proxy-JWT 不能绕过 agent 的 token 检查。

这不意味着每个 URL 都要求 token：静态资源、OPTIONS，以及上游兼容例外 `/api/get_system_language_settings`、`/api/cli_hello`、`/api/forge_webhook`、单段 ticket 下载路由 `/api/workspace_download/{ticket}`、`/api/backup_download/{ticket}`、`/api/office-watch-proxy/{port}` 及子路径保留公开语义；相应 ticket/HMAC/capability 由 codeg 校验。路由兼容基于上游 v0.39.0，升级上游应重新核验。

默认每 IP **10 分钟内 10 次失败 → 15 分钟封锁**，错误凭据返回 429/Retry-After；正确 token 始终通过，包括同 NAT 的合法用户，但成功不清空失败计数、不解除/缩短封锁。IP 来自 relay OPEN 元数据，不信任客户端 X-Forwarded-For。token 默认每 60 秒从只读 DB 或私有文件刷新；刷新失败清空缓存并拒绝受保护请求，来源恢复后自动恢复。

### 凭据保管与轮换

| 材料 | 用途与泄漏影响 |
| --- | --- |
| agent key | 占用 relay 唯一家庭端 slot。泄漏者可以替换合法 agent、造成拒绝服务，但仅凭此 key 无法解密或冒充手机固定的 home 身份。 |
| web token / 配对 PNG、JSON | 业务访问凭据；泄漏后按 codeg 的 token 轮换流程更新，再重新配对所有手机。 |
| home cert + private key | 手机信任的家庭端身份；成对私有备份，私钥泄漏时 `rotate-cert` 并重配所有手机。 |
| relay cert + private key | agent 信任的 relay 身份；更换后通过已核实 SSH 更新 agent 的 relay pin，勿从公开 health 建立信任。 |

Unix 凭据文件要求当前用户拥有、0600，state 目录0700。Windows 依赖私有 profile 的继承 ACL，不逐次改 ACL；自定义路径也必须私有。不要放在共享或同步目录，不使用 junction/symlink/reparse point。日志与状态不会输出 token；但 **`pair` 默认会打印配对 JSON**，在自动化中始终用 `pair --png ... --json-only --json-file ...`，扫码后删除 PNG/JSON。不要把凭据贴进聊天、工单、CI 日志或 shell 参数。

`install.sh` **保留已存在的 key**；重复安装并不轮换。发生 agent-key 泄漏时，在 VPS root shell 中主动生成替换文件，始终将 stdout 重定向到文件：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
umask 077
systemctl stop codeg-relay
/opt/codeg-relay/bin/codeg-relay gen-key > /etc/codeg-relay/agent.key.new
chown codeg-relay:codeg-relay /etc/codeg-relay/agent.key.new
chmod 0600 /etc/codeg-relay/agent.key.new
mv -f /etc/codeg-relay/agent.key.new /etc/codeg-relay/agent.key
systemctl start codeg-relay
```

用 scp 将新 key 复制到家庭端原私有 key 路径（Unix 恢复0600），重启家庭 agent，核对 status/health；删除临时旧副本。中途失败时保持服务停止并检查文件 owner/权限，不能把 key 打印出来排查。也可完整卸载再安装，但会删除 relay key/cert，需同时重新获取 relay pin。仅轮换 agent key 不改变 home 证书，无需因此重配手机。证书处理见 [配对指南](docs/pair-phone.md)。

### 日志与可见信息

Relay 日志包含连接 IP、分类、字节数、时长和固定关闭原因，不解析手机 HTTP 请求行；家庭 agent 日志包含连接状态、固定原因、错误/限流/缓冲统计，不记录路径/query、body、Authorization、WebSocket 凭据子协议或 key/token。依赖库原始报文日志被过滤。状态文件有 PID、时间、计数及两种指纹；指纹不是秘密，但公开报告使用占位符。进程日志仍含网络元数据，分享前应脱敏。Agent 日志上限 5 MiB/文件加三个归档；relay journal 保留由系统策略决定。

### 报告安全问题

在 [仓库 Security 页面](https://github.com/PanAndDuck/codeg-relay/security) 使用 **Report a vulnerability**（若维护者已开启私人漏洞报告）。若不可用，只提交不含漏洞细节/凭据的普通 issue 请求私密联络方式；不要在公开 issue 发布可利用细节或真实部署信息。提供版本、平台、影响、使用临时凭据的最小复现和脱敏日志。不要上传真实 key、token、配对文件、完整配置或私钥；不承诺尚未建立的响应时限。

## English

### Trust model

Phone TLS terminates at the home agent, across a blind relay. **Phones pin the home certificate; agents pin the relay certificate before sending the agent key.** A compromised relay can observe IP/timing/size/online metadata and disrupt service, but cannot decrypt phone traffic or impersonate the pinned home. Relay-provided IP metadata is not an authenticated user identity. Public HTTP health is unauthenticated and cannot bootstrap certificate trust.

The phone, home OS, agent and codeg are trusted endpoints. Local malware, administrators or a leaked home private key are outside this relay protection. The final hop defaults to loopback HTTP; use HTTPS for a remote upstream, optionally with `--upstream-fingerprint`. Never expose that plaintext hop publicly. No legacy plaintext relay fallback exists.

### Authentication and rate limits

All protected APIs and WebSockets require the codeg web token before an upstream connection. Accepted credentials are an exact Bearer header or the first `codeg-token.` base64url WebSocket subprotocol, not query/cookie tokens. Empty configuration fails closed; comparison uses constant-time equality of fixed SHA-256 digests. Upstream proxy-JWT cannot bypass this check.

Static resources, OPTIONS and the upstream public exceptions listed in the Chinese section retain their semantics: language settings, CLI hello, forge webhook, single-segment ticket downloads and office-watch proxy routes. Relevant ticket/HMAC/capability validation remains upstream. Compatibility follows upstream v0.39.0 and must be reviewed when codeg changes.

Defaults are **ten failures per IP in ten minutes, then a fifteen-minute block** with 429/Retry-After for invalid credentials. Valid tokens always pass, including behind the same NAT, without resetting counts or shortening blocks. IP comes from relay OPEN metadata, not client X-Forwarded-For. Tokens refresh from a read-only DB/private file every sixty seconds; failure clears the cache and protected requests fail closed until recovery.

### Credential handling and recovery

An agent key grants the relay slot: theft allows replacement/denial of service, but alone cannot decrypt or impersonate the pinned home. A web token or pairing PNG/JSON is an access credential: rotate the token in codeg and re-pair. Home private-key compromise requires `rotate-cert` and re-pairing all phones. Relay certificate replacement requires an updated agent pin acquired through verified SSH.

Unix private files require owner-only 0600 and state directories 0700. Windows preserves private profile ACLs; custom paths must be equally private. Avoid shared/synced paths and reparse points. **Pairing prints secret JSON by default**: automation must combine `--png`, `--json-only` and `--json-file`, then delete exports after scanning. Keep credentials out of chat, issues, CI output and command arguments.

Re-running `install.sh` preserves an existing agent key and does not rotate it. For a leak, execute the root-shell rotation block above: stop relay, generate directly into a private replacement file, set owner/mode, atomically replace, then start. Scp the new key into the home agent's existing private path, restore Unix 0600, restart the agent, verify status/health and delete obsolete copies. On failure keep the service stopped while checking permissions without printing secrets. Full uninstall/reinstall is another option, but also changes relay identity and requires a new relay pin. Agent-key rotation alone leaves the home pin unchanged and does not require phone re-pairing.

### Logs and reporting

Relay logs contain IP, classification, byte counts, duration and fixed close reasons, not phone HTTP request lines. Agent logs contain connection state and fixed diagnostic/statistical information, not URL paths/queries, bodies, Authorization, credential subprotocols or keys/tokens. Dependency wire logs are filtered. Status includes process/timing/counters and fingerprints. Fingerprints are public identifiers, but use placeholders in public reports; redact network metadata too. Agent logs are bounded to 5 MiB plus three archives; relay journal retention is an OS policy.

Use **Report a vulnerability** on the [Security page](https://github.com/PanAndDuck/codeg-relay/security) if enabled. Otherwise open a non-sensitive issue requesting a private contact channel, without exploit details or deployment secrets. Include version/platform/impact, a reproduction using temporary credentials and redacted logs. Never upload real keys, tokens, pairing exports, full private configs or private keys. No response-time guarantee is currently stated.
