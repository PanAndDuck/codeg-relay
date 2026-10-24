# macOS / Linux 家庭端

## 中文

继续使用官方 codeg 桌面应用。在 **设置 → Web 服务** 开启服务及自动启动选项，保持应用运行；默认 `http://127.0.0.1:3080`。以运行 codeg 的同一用户操作，用户服务不要用 sudo 安装。先完成 [VPS 部署](relay-vps.md)，下列变量在同一 shell 中保持。

### 1. 下载与私有目录

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
umask 077
case "$(uname -s)-$(uname -m)" in
  Darwin-arm64) target=aarch64-apple-darwin ;;
  Darwin-x86_64) target=x86_64-apple-darwin ;;
  Linux-x86_64) target=x86_64-unknown-linux-musl ;;
  Linux-aarch64) target=aarch64-unknown-linux-musl ;;
  *) printf 'Unsupported platform\n' >&2; exit 1 ;;
esac
if [ "$(uname -s)" = Darwin ]; then
  state="$HOME/Library/Application Support/codeg-relay-agent"
  db="$HOME/Library/Application Support/app.codeg/codeg.db"
  logs="$HOME/Library/Logs/codeg-relay-agent"
else
  state="${XDG_STATE_HOME:-$HOME/.local/state}/codeg-relay-agent"
  db="$HOME/.local/share/app.codeg/codeg.db"
  logs="$HOME/.local/state/codeg-relay-agent/logs"
fi
download="$HOME/.cache/codeg-relay-downloads/v0.1.0"
mkdir -p "$state" "$download" "$HOME/.local/bin"
chmod 0700 "$state" "$download"
cd "$download"
base=https://github.com/PanAndDuck/codeg-relay/releases/download/v0.1.0
asset="codeg-relay-agent-$target.tar.gz"
curl -fL --retry 3 "$base/$asset" -o "$asset"
curl -fL --retry 3 "$base/$asset.sha256" -o "$asset.sha256"
if [ "$(uname -s)" = Darwin ]; then shasum -a 256 -c "$asset.sha256"; else sha256sum -c "$asset.sha256"; fi
tar -xzf "$asset"
install -m 0755 codeg-relay-agent "$HOME/.local/bin/codeg-relay-agent"
agent="$HOME/.local/bin/codeg-relay-agent"
"$agent" --help
curl --fail --max-time 10 -o /dev/null http://127.0.0.1:3080/
test -f "$db"
```

**检查点：** 摘要 `OK`、CLI 帮助正常、本地 Web 服务可达、DB 存在。`XDG_STATE_HOME` 若设置必须为绝对路径。固定版本 `v0.1.0` 尚未发布时 404 应停止；最新稳定版可改用 `https://github.com/PanAndDuck/codeg-relay/releases/latest/download`。macOS 下载安全提示应按系统允许的正常流程由用户核实处理，不关闭 Gatekeeper。

### 2. 文件交接与初始化

确认 SSH 主机身份；以下 root SSH 端口与 relay 端口相互独立，指纹文件由 VPS 指南生成：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
ssh_target=root@203.0.113.10
ssh_port=22
relay_url=https://relay.example.com:24443
scp -P "$ssh_port" "$ssh_target:/etc/codeg-relay/agent.key" "$state/agent.key"
scp -P "$ssh_port" "$ssh_target:/root/codeg-relay-setup/relay-fingerprint.txt" "$state/relay-fingerprint.txt"
chmod 0600 "$state/agent.key" "$state/relay-fingerprint.txt"
relay_fingerprint=$(head -n 1 "$state/relay-fingerprint.txt")
"$agent" --state-dir "$state" init --relay-url "$relay_url" \
  --agent-key-file "$state/agent.key" --relay-fingerprint "$relay_fingerprint" --db-path "$db"
"$agent" --state-dir "$state" run
```

**检查点：** 前台 `state="connected"`；然后 Ctrl-C。失败先检查 token 来源、文件 owner/0600、URL/pin/key/端口/NAT。不要给手机 relay 指纹。非 root SSH 的私有 key 暂存方式见 VPS 指南。

`init` 只初始化一次；已有配置时不要删除 state 重做，应停服务、私有备份并编辑 `config.json`。默认 DB 只读；特殊安装可用 `--db-path`，或以 `--token-file` 替代（两者互斥，token 文件 UTF-8 无 BOM、当前用户拥有、0600）。默认 upstream 为 loopback HTTP，跨机器应使用 HTTPS；相关选项见 [agent 参考](../crates/codeg-relay-agent/README.md)。

### 3. 用户服务与配对

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
"$agent" --state-dir "$state" install-service --dry-run
"$agent" --state-dir "$state" install-service
sleep 3
"$agent" --state-dir "$state" status
tail -n 50 "$logs/agent.log"
"$agent" --state-dir "$state" pair --name 'Home' --png "$state/pair.png" \
  --json-only --json-file "$state/pair.json"
if [ "$(uname -s)" = Darwin ]; then open "$state/pair.png"; else xdg-open "$state/pair.png"; fi
```

**检查点：** status `connected` 且快照新鲜；用户在 [Codeg-Fork iOS app](pair-phone.md) 扫码并成功连接。无 Linux 图形桌面时用可信的本机图片查看方式；不要把 PNG 上传网站。确认后关闭图片并删除导出文件：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
rm -f -- "$state/pair.png" "$state/pair.json"
```

| 平台 | 用户服务 | 日志与生命周期 |
| --- | --- | --- |
| macOS | `~/Library/LaunchAgents/app.codeg.relay-agent.plist` | `~/Library/Logs/codeg-relay-agent/agent.log`；登录后运行，KeepAlive，重启节流 10 秒。 |
| Linux | `~/.config/systemd/user/app.codeg.relay-agent.service` | `~/.local/state/codeg-relay-agent/logs/agent.log`（即使自定义 XDG state）；`systemctl --user`，重启间隔 5 秒。 |

日志每文件 5 MiB、三个归档。Linux user manager/linger 决定注销后是否运行；安装器不修改 linger，且 codeg 本身也必须可用。两平台睡眠时无法远程访问。可用 `install-service --dry-run --platform macos` 或 `--platform linux` 预览，正式安装不要加 `--platform`。

### 4. 更新、卸载与故障

更新先下载校验新包，再停止前台进程并执行 `uninstall-service`，替换稳定路径 binary，执行 `install-service` 和 `status`。保留原下载包用于回退，保留配置/key/home 证书，不重复 init。

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
"$agent" --state-dir "$state" uninstall-service --dry-run
"$agent" --state-dir "$state" uninstall-service
```

卸载服务保留 state 和日志。决定彻底清除后才手动删除上述已知目录/binary；备份 home cert/key 时必须成对保护。`rotate-cert` 后重新配对所有手机。401 查 token/刷新，429 查 Retry-After（正确 token 始终通过），502 查本地 codeg/Web 服务，backoff 查 relay URL/pin/key/NAT。详细表见 [Windows 指南](home-windows.md)，证书错误见 [手机指南](pair-phone.md)。不要通过重装整个桌面应用排查 agent。

## English

Keep official codeg running with **设置 → Web 服务** and its auto-start option enabled (default `http://127.0.0.1:3080`). Use the same desktop user; do not install the user service with sudo. Complete [VPS deployment](relay-vps.md) first. Execute the four numbered command groups above in the same shell.

1. Select the correct macOS Apple Silicon/Intel or Linux ARM64/x64 archive. Verify its sidecar with `shasum -a 256 -c` on macOS or `sha256sum -c` on Linux before extraction. Use a stable binary path. Expect checksum `OK`, working CLI help, reachable local web service and an existing DB. The planned first tag is `v0.1.0`; stop on 404. The latest-stable base is `https://github.com/PanAndDuck/codeg-relay/releases/latest/download`. Any XDG_STATE_HOME must be absolute. Resolve macOS download prompts through the normal trusted-user process without disabling Gatekeeper.
2. Verify SSH identity and scp key/fingerprint files directly into the private state directory. Set files to 0600 owned by the current user and state to 0700. Use the relay fingerprint for init, never for the phone. Expect connected foreground logs, then Ctrl-C before service installation. `init` refuses existing config: stop, privately back up/edit config and restart. `--db-path` and `--token-file` are mutually exclusive; explicit token files must be private UTF-8 without BOM. Default DB paths are in the command block. A remote upstream should use HTTPS.
3. Preview/install the native user service, check fresh connected status and logs, and export pairing with `--png --json-only --json-file`. Open it locally for the human, confirm the phone connects, then delete both exports using the cleanup block. Do not upload QR images to a website. macOS uses the listed LaunchAgent at login (KeepAlive, ten-second throttle); Linux uses the listed systemd user unit (five-second restart). Linux logout behavior depends on the user manager/linger; the installer does not enable linger, and codeg must remain available. Sleep stops access. Logs are 5 MiB plus three archives; Linux service logs use the table's fixed path even with a custom XDG state. `--platform` is only for cross-platform dry-runs.
4. Update by downloading/verifying first, stopping foreground operation, uninstalling the service, replacing the stable binary and reinstalling/checking status. Keep the old verified package for rollback and preserve state/key/home identity. Uninstall removes only the user service, leaving state/logs. Delete known agent paths only after deciding whether to retain identity. Back up the home certificate/private key together. `rotate-cert` requires re-pairing. For 401 check token/refresh, 429 Retry-After, 502 local codeg, and backoff relay URL/pin/key/NAT. See [Windows troubleshooting](home-windows.md) and [phone certificate recovery](pair-phone.md).
