# 新 VPS 部署 / New VPS deployment

## 中文

### 1. 前提与检查

需要 Linux x86_64/aarch64、运行中的 systemd、root SSH，以及可用公网 TCP 端口。下例内部端口 `18443`，公网端口 `24443`；直连 VPS 可让两者均为 `18443`。NAT VPS 先在供应商面板配置 **公网 TCP 端口 → VPS 内部 TCP 端口**，开放安全组/供应商防火墙。SSH 端口单独填写。

在 VPS root shell 中执行；非 root SSH 用户先通过已有授权的 `sudo -i` 进入 root shell：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
id -u
uname -srm
cat /etc/os-release
ps -p 1 -o comm=
systemctl --version
systemd-detect-virt --container || true
ss -ltnp
systemctl --failed --no-pager
for command in bash curl tar sha256sum systemctl install useradd userdel groupdel getent flock ss; do command -v "$command" || exit 1; done
```

**检查点：** UID `0`，PID 1 为 systemd，架构受支持，内部端口空闲。缺工具时用发行版包管理器安装；脚本不装包。记录既有监听/失败服务，后续对照，不停止其他服务。容器检测仅作线索，不能据此直接关闭 sandbox。

### 2. 下载并验证

以下固定首个计划版本。若尚未发布而返回 404，停止并等待发布。需要最新稳定版时将 `release_base` 改为 `https://github.com/PanAndDuck/codeg-relay/releases/latest/download`。

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
umask 077
mkdir -p /root/codeg-relay-setup/v0.1.0
cd /root/codeg-relay-setup/v0.1.0
release_base=https://github.com/PanAndDuck/codeg-relay/releases/download/v0.1.0
case "$(uname -m)" in
  x86_64|amd64) relay_asset=codeg-relay-x86_64-unknown-linux-musl ;;
  aarch64|arm64) relay_asset=codeg-relay-aarch64-unknown-linux-musl ;;
  *) printf 'Unsupported architecture\n' >&2; exit 1 ;;
esac
for asset in "$relay_asset" "$relay_asset.sha256" codeg-relay-deploy.tar.gz codeg-relay-deploy.tar.gz.sha256; do
  curl -fL --retry 3 "$release_base/$asset" -o "$asset"
done
sha256sum -c "$relay_asset.sha256"
sha256sum -c codeg-relay-deploy.tar.gz.sha256
tar -xzf codeg-relay-deploy.tar.gz
```

**检查点：** 两个摘要均 `OK`，得到完整 `deploy/`。保留相邻 `install-common.sh`、`codeg-relay.service`、`no-sandbox.conf` 和 `uninstall.sh`，不要只复制安装脚本。摘要错误时重新下载核对，不能执行。

### 3. 安装

继续在相同 shell/目录中，替换域名为自己的域名或 `203.0.113.10` 所代表的公网 IP：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
bash deploy/install.sh --binary "./$relay_asset" \
  --port 18443 --public-port 24443 --public-host relay.example.com --no-print-key
```

| 选项 | 精确作用 |
| --- | --- |
| `--binary PATH` | 必填，Linux ELF。相邻 `PATH.sha256` 存在时必须验证成功；缺文件仅警告，因此本指南要求主动下载。安装前检查 ELF 与 CPU，下载文件无需预先 chmod。 |
| `--public-host IP[,DNS]` | 必填，证书 SAN 列表；第一项用于输出 URL。不能包含 scheme/端口。已有证书会复用，修改列表不自动轮换。 |
| `--port N` | 内部监听/UFW 端口，默认 `18443`，允许 `1024–65535`，拒绝 `3306`、`6379`。 |
| `--public-port N` | 公网端口 `1–65535`，默认等于 `--port`。只影响输出 URL/健康检查，不创建 NAT、不更改内部监听。 |
| `--no-print-key` | 始终使用：只给文件路径，不打印新 key。省略时首次生成会打印。 |
| `--no-sandbox` | 仅出现 namespace/seccomp 启动失败提示后，在原命令追加；关闭相关隔离，保留专用用户、NoNewPrivileges 和资源限制。 |
| `--open-firewall` | 明确授权脚本管理内部端口 UFW TCP 规则；不启用 UFW、不修改供应商规则。省略时不调用 UFW/iptables。保留预存规则，冲突须人工处理。 |
| `-h`, `--help` | 帮助，不安装。 |

安装路径：`/opt/codeg-relay/bin/codeg-relay`、`/etc/codeg-relay/`、`/var/lib/codeg-relay/`、`/etc/systemd/system/codeg-relay.service`。专用 `codeg-relay` 用户；unit 限制 `MemoryMax=128M`、`CPUQuota=50%`、`TasksMax=256`，不是全并发性能保证。

**失败：** 查看 `journalctl -u codeg-relay --no-pager -n 80`。仅安装器提示 namespace/seccomp 限制（如 `226/NAMESPACE`、`218/CAPABILITIES`）时重跑原命令并追加 `--no-sandbox`。它创建受管理 `codeg-relay.service.d/no-sandbox.conf`；不带该选项重跑会恢复加固，更新时保留已确认需要的选项。脚本拒绝接管未管理同名路径，不能绕过 marker 检查。

### 4. 验证与交接

VPS 本机：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
systemctl is-active codeg-relay
systemctl status codeg-relay --no-pager
curl --fail --max-time 10 http://127.0.0.1:18443/__codeg_relay/v1/health
journalctl -u codeg-relay --no-pager -n 50
umask 077
/opt/codeg-relay/bin/codeg-relay fingerprint --data-dir /var/lib/codeg-relay > /root/codeg-relay-setup/relay-fingerprint.txt
```

家庭电脑访问公网（PowerShell 用 `curl.exe`）：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
curl --fail --max-time 10 http://relay.example.com:24443/__codeg_relay/v1/health
```

**检查点：** service `active`，JSON 有 `status: "ok"`、`mode: "e2ee"`、`version`、`agent_connected`。尚无 agent 时为 `false`，家庭端连接后变 `true`。健康检查特意用 **HTTP**；同端口 HTTPS 转发给家庭端，不能用于探测 relay 证书。健康结果公开、未经认证，只证明可达性，不证明端到端身份。

安装器末尾：

```text
Values for the home agent:
Relay URL: https://relay.example.com:24443
Relay certificate SHA-256: <fingerprint>
Agent-key file (on VPS): /etc/codeg-relay/agent.key
```

通过已核实主机身份的 SSH，将 key **直接 scp 到家庭端私有文件**，指纹文件也用 scp。不能把 key、web token 或配对 JSON 粘贴到聊天。完整复制命令见 [Windows](home-windows.md) / [macOS/Linux](home-macos-linux.md)。非 root SSH 无权读取 key 时，由 root 用 `install -m 0600 -o SSH用户名` 暂存到该用户私有目录，复制后立即删除暂存件，不放宽原文件权限。指纹文件第一行用于 `init --relay-fingerprint`，手机不能使用此指纹。

### 5. 更新、回退与卸载

下载同一新标签的 binary、sidecar、deploy 包，校验后使用原端口/主机及 sandbox/firewall 选项重跑。保存原安装参数；`--public-port` 不持久化在 relay.env。重复安装保留 key 和 TLS 证书，替换 binary 并重启。回退到已验证兼容版本使用相同步骤；独立 agent 不支持 legacy plaintext relay。

需要保留身份时，先私有备份（不要上传工单）：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
umask 077
tar -czf /root/codeg-relay-setup/relay-state-backup.tar.gz -C / etc/codeg-relay var/lib/codeg-relay
```

决定移除该实例后，在解压 deploy 包的目录执行：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
bash deploy/uninstall.sh
systemctl is-active codeg-relay || true
ss -ltnp
```

卸载删除专用服务、用户/组、三个专用目录、key/cert 和自身添加且仍匹配 marker 的 UFW 规则；保留管理员其他 drop-in/预存规则。公网 NAT/安全组单独撤销。外部修改过规则时脚本停下保留状态，核对后处理。重装产生新 relay 身份，须更新家庭端 relay pin/key；只有 home 身份也改变时，才因 home pin 变化而需要手机重新配对。若公网 URL 改变，手机条目也须更新 URL 或重新导入配对。

## English

Use a fresh Linux x86_64/aarch64 VPS with systemd, root SSH access and a public TCP port. Map public `24443` to internal `18443` on NAT VPSes; direct VPSes can use `18443` for both. Open provider firewall/security-group rules separately. SSH may use another port. Run the numbered Bash blocks above in a root shell (`sudo -i` first if needed).

1. **Preflight:** confirm UID 0, systemd as PID 1, supported CPU, required commands and a free internal port. Record existing services/listeners. Install missing OS packages separately. Container detection alone does not justify disabling sandboxing.
2. **Download:** use one release tag for the architecture-specific binary, its sidecar and deploy archive. The commands pin planned `v0.1.0`; use `https://github.com/PanAndDuck/codeg-relay/releases/latest/download` for latest stable. Stop on 404/hash failure. Both checks must say `OK`. Keep the entire extracted `deploy/` directory and adjacent dependencies.
3. **Install:** use step 3 with your host/ports. Required `--binary` validates the ELF architecture and any sibling `.sha256`; missing checksums merely warn, so always download them. Required `--public-host IP[,DNS]` contains no scheme/port; its first item becomes the advertised host. Existing certificates are reused. `--port` is the internal/UFW port (default 18443, 1024–65535 excluding 3306/6379). `--public-port` only advertises a public port (1–65535, defaults to internal); it configures no NAT. Always pass `--no-print-key`. `--open-firewall` explicitly manages one UFW TCP rule, without enabling UFW or managing provider rules. `-h`/`--help` displays usage.
4. **Failure:** inspect `journalctl -u codeg-relay --no-pager -n 80`. Add `--no-sandbox` only after the installer's namespace/seccomp hint. It preserves the dedicated account, NoNewPrivileges and resource limits, while resetting related isolation directives. Reinstalling without it restores hardening. Preserve the choice during updates. Do not bypass unmanaged-path checks or stop unrelated services. Unit limits are 128 MiB, 50% CPU and 256 tasks, not a throughput guarantee.
5. **Verify:** local and public HTTP health must return `status: "ok"`, `mode: "e2ee"`, `version`, and `agent_connected` (false before the agent, true afterward). The service must be active. Health intentionally uses HTTP; HTTPS forwards home TLS. This public unauthenticated response is a reachability signal, not identity proof.
6. **Handoff:** keep the public HTTPS URL, relay fingerprint and `/etc/codeg-relay/agent.key`. Step 4 saves fingerprint output to a file; use its first line. Transfer files through verified SSH/scp into private home directories. If SSH is non-root, root can stage a 0600 copy owned by that user in a private directory; delete it after transfer. Never relax the original key permissions or paste secrets. Follow [Windows](home-windows.md) or [Unix](home-macos-linux.md). Phones use the home pin, never this relay pin.
7. **Update/rollback:** verify the selected release and rerun the same install options. Keep public-port/sandbox settings recorded: public-port is not persisted in relay.env. Reinstall preserves key/certificate and restarts. There is no legacy plaintext fallback. Protect any identity backup made with the command above.
8. **Uninstall:** `bash deploy/uninstall.sh` removes the managed unit/account, `/opt/codeg-relay`, `/etc/codeg-relay`, `/var/lib/codeg-relay`, key/certificate and only its still-matching UFW rule. Unrelated drop-ins/rules remain. Resolve changed rules if it stops; remove provider mappings separately and compare with preflight. A fresh install changes relay credentials; update the agent. Phone re-pairing follows a changed home identity, not merely a changed relay certificate. If the public URL changes, update the phone URL or reimport pairing too.
