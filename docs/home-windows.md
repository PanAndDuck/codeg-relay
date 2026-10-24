# Windows 家庭端 / Windows home agent

## 中文

Windows 10/11 x64，使用运行官方 codeg 的同一普通用户、PowerShell 5.1 或更新版本、内置 OpenSSH Client（`ssh.exe`/`scp.exe`）。保持登录、电脑清醒、codeg 正在运行；睡眠、关机或注销会中断远程访问。安装 agent 不需要给 Windows 开入站端口或关闭安全功能。

### 1. 开启本地 Web 服务

官方 codeg 中打开 **设置 → Web 服务**，启用服务及 **Codeg 启动时自动开启 Web 服务**。默认 `http://127.0.0.1:3080`；同页显示 token，登录页也提示“Token 可在桌面端 设置 → Web 服务 中获取”。不把 token 发给 AI 或写进命令参数。默认 agent 只读 `%APPDATA%\app.codeg\codeg.db` 的 web token。

```powershell
$ErrorActionPreference = 'Stop'
foreach ($name in @('CODEG_DATA_DIR','CODEG_HOME','CODEG_STATIC_DIR','CODEG_ACP_TMP_ROOT','CODEG_TOKEN','CODEG_PORT','CODEG_HOST')) {
    Remove-Item "Env:$name" -ErrorAction SilentlyContinue
}
$State = Join-Path $env:LOCALAPPDATA 'codeg-relay-agent'
$Bin = Join-Path $env:LOCALAPPDATA 'Programs\codeg-relay-agent'
$Download = Join-Path $env:LOCALAPPDATA 'codeg-relay-downloads\v0.1.0'
New-Item -ItemType Directory -Force -Path $State,$Bin,$Download | Out-Null
$Agent = Join-Path $Bin 'codeg-relay-agent.exe'
$Db = Join-Path $env:APPDATA 'app.codeg\codeg.db'
(Invoke-WebRequest -UseBasicParsing -TimeoutSec 10 'http://127.0.0.1:3080/').StatusCode
Test-Path -LiteralPath $Db
icacls.exe "$State"
```

**检查点：** HTTP `200`、DB 存在；state 在私有用户 profile 下，ACL 仅允许本人及正常 SYSTEM/Administrators 管理权限，无其他普通用户读取。Agent 依赖继承 ACL，不自动收紧 ACL；共享/网络/云同步目录不适合存凭据。缺 APPDATA/LOCALAPPDATA 时明确指定路径，不猜测他人 profile。不要使用 junction/reparse point 作为 state/凭据路径。

### 2. 下载、校验并安装双 exe

```powershell
$Base = 'https://github.com/PanAndDuck/codeg-relay/releases/download/v0.1.0'
$Asset = 'codeg-relay-agent-x86_64-pc-windows-msvc.zip'
$Zip = Join-Path $Download $Asset
Invoke-WebRequest -UseBasicParsing "$Base/$Asset" -OutFile $Zip
Invoke-WebRequest -UseBasicParsing "$Base/$Asset.sha256" -OutFile "$Zip.sha256"
CertUtil.exe -hashfile "$Zip" SHA256
if ($LASTEXITCODE -ne 0) { throw 'CertUtil failed' }
$Expected = ((Get-Content -LiteralPath "$Zip.sha256" -Raw).Trim() -split '\s+')[0]
if ($Expected -notmatch '^[0-9a-fA-F]{64}$' -or (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash -ine $Expected) { throw 'Checksum mismatch' }
$Unpack = Join-Path $Download 'unpacked'
Expand-Archive -LiteralPath $Zip -DestinationPath $Unpack -Force
Copy-Item -LiteralPath (Join-Path $Unpack 'codeg-relay-agent.exe'),(Join-Path $Unpack 'codeg-relay-agentw.exe') -Destination $Bin -Force
& $Agent --help
if ($LASTEXITCODE -ne 0) { throw 'Agent CLI failed' }
```

**检查点：** 摘要匹配，同一稳定目录内有两个 exe。首个计划 tag 是 `v0.1.0`；未发布而 404 时停止。最新稳定版可使用 `https://github.com/PanAndDuck/codeg-relay/releases/latest/download`，升级时重新校验。SmartScreen/杀毒软件可能提示二进制未签名；核对仓库来源、摘要与本机策略，由用户/管理员处理提示，不能关闭 Defender、SmartScreen 或绕过组织策略。

### 3. 安全复制 relay key 与指纹

先完成 [VPS 指南](relay-vps.md) 的指纹文件生成。以下使用 root SSH；确认 SSH 主机指纹，不能禁用 host-key 检查。SSH 端口可能不同于公网 relay 端口。

```powershell
$SshTarget = 'root@203.0.113.10'
$SshPort = 22
$RelayUrl = 'https://relay.example.com:24443'
$KeyFile = Join-Path $State 'agent.key'
$FingerprintFile = Join-Path $State 'relay-fingerprint.txt'
scp.exe -P $SshPort "${SshTarget}:/etc/codeg-relay/agent.key" "$KeyFile"
if ($LASTEXITCODE -ne 0) { throw 'Key transfer failed' }
scp.exe -P $SshPort "${SshTarget}:/root/codeg-relay-setup/relay-fingerprint.txt" "$FingerprintFile"
if ($LASTEXITCODE -ne 0) { throw 'Fingerprint transfer failed' }
$RelayFingerprint = (Get-Content -LiteralPath $FingerprintFile -TotalCount 1).Trim()
if ($RelayFingerprint -notmatch '^[0-9a-fA-F]{64}$') { throw 'Invalid relay fingerprint file' }
```

不要用 `Get-Content` 显示 key。非 root SSH 按 VPS 指南使用私有暂存件；也可通过已有 SSH config/ssh-agent 选择身份，不能把 SSH 私钥/密码放进聊天。Relay key 与 codeg web token 是不同凭据。

### 4. 初始化与前台验证

```powershell
& $Agent --state-dir "$State" init --relay-url "$RelayUrl" --agent-key-file "$KeyFile" --relay-fingerprint "$RelayFingerprint" --db-path "$Db"
if ($LASTEXITCODE -ne 0) { throw 'Initialization failed' }
& $Agent --state-dir "$State" run
```

**检查点：** `Initialized ...`，随后前台日志 `state="connected"`。在另一个 PowerShell 窗口重新设置 `$State`、`$Bin`、`$Agent` 后运行 `& $Agent --state-dir "$State" status`；应为 `state: "connected"`。确认后回前台窗口 **Ctrl-C**，再安装计划任务，不要同时启动第二个 agent。

`init` 写配置和生成 home 证书，但不读取 token；`run` 才验证 token 来源。已存在 `config.json` 时 `init` 拒绝覆盖：先停服务，私有备份并编辑现有配置，再重启；不要删除整个 state。特殊安装可用 `init --db-path` 指定真实 DB，或改用 `init --token-file` 指向用户本地保存的 UTF-8 无 BOM token 文件；两个选项不能同时指定。默认本地 upstream 为 `http://127.0.0.1:3080`，更改端口时同时传 `--upstream`。

### 5. 计划任务与状态

```powershell
& $Agent --state-dir "$State" install-service --dry-run --platform windows
if ($LASTEXITCODE -ne 0) { throw 'Service preview failed' }
& $Agent --state-dir "$State" install-service
if ($LASTEXITCODE -ne 0) { throw 'Task registration/start failed' }
Start-Sleep -Seconds 3
& $Agent --state-dir "$State" status
$Sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$TaskName = "app.codeg.relay-agent-$Sid"
schtasks.exe /Query /TN "$TaskName" /V /FO LIST
```

**检查点：** Task Scheduler（任务计划程序 → 任务计划程序库）中同名任务正在运行；Action 是同目录 `codeg-relay-agentw.exe`，当前用户登录触发、最小权限、无需密码。`--platform` 只允许与 `--dry-run` 配合；正式安装会自动选 Windows。状态快照超过 5 秒不更新时为 `stopped_or_stale`，不能把任务“已创建”当作已连通。

默认 state 为 `%LOCALAPPDATA%\codeg-relay-agent`；默认日志为其中的 `logs\agent.log`，单文件 5 MiB、三个归档。即便自定义 state，存在 LOCALAPPDATA 时服务日志仍用该默认位置。查看日志：

```powershell
Get-Content -LiteralPath (Join-Path $env:LOCALAPPDATA 'codeg-relay-agent\logs\agent.log') -Tail 50
```

### 6. 配对 iPhone

```powershell
$PairPng = Join-Path $State 'pair.png'
$PairJson = Join-Path $State 'pair.json'
& $Agent --state-dir "$State" pair --name 'Windows Home' --png "$PairPng" --json-only --json-file "$PairJson"
if ($LASTEXITCODE -ne 0) { throw 'Pairing export failed; inspect existing files before retrying' }
Start-Process -FilePath "$PairPng"
```

用户在 **Codeg-Fork iOS app** 添加服务器并扫码；详细步骤见 [手机配对](pair-phone.md)。`--json-only` 抑制终端 QR，`--json-file` 防止 JSON 输出到终端；只加 `--png` 仍会打印含 token 的 JSON。文件已存在时拒绝覆盖。确认手机成功连接后关闭图片，执行：

```powershell
Remove-Item -LiteralPath $PairPng,$PairJson -Force
```

### 7. 更新与卸载

更新先下载/校验新版本到新目录，保留旧 ZIP。停止前台 agent 后，用下列命令移除任务；再按步骤 2 替换两个 exe，重装任务并检查 status。保留 state/key/home 证书，不重复 init；回退用旧的已验证双 exe。不能覆盖仍运行的 exe。

```powershell
& $Agent --state-dir "$State" uninstall-service
if ($LASTEXITCODE -ne 0) { throw 'Check Task Scheduler before replacing binaries' }
```

卸载只停止/删除本用户任务及任务 XML，保留 state、身份、日志。再次卸载不存在的 Windows 任务可能报错；先查询任务确认缺失，不把其他错误忽略。确认完全移除且无需保留身份后，单独删除 `$Bin`、`$State`、默认日志和本次下载目录。失去 home 私钥后必须重配手机。官方 codeg 不受 agent 卸载影响。

### 8. 排障

| 现象 | 检查与处理 |
| --- | --- |
| 401 | 核对桌面 Web 服务/token、DB 路径、token 是否轮换；token 最长 60 秒刷新，重新生成配对材料。服务关闭导致不可达通常是 502。 |
| 429 | 同 IP 10 分钟内 10 次失败触发 15 分钟封锁；遵守 Retry-After。正确 token 始终通过，但不会清除失败计数/封锁。 |
| 502 | codeg 未运行/Web 服务关闭、upstream 端口或 HTTPS 信任错误；先验证本地 URL。 |
| relay backoff | URL、公网端口/NAT、relay pin、key 文件、网络；401/403 的 relay 鉴权失败退避 5 分钟。 |
| `4001 replaced` | 同一 relay 有第二个 agent；停止重复实例，每个家庭端独占一个 relay。 |
| certificate changed | 核实 home 是否轮换/丢失，按配对指南重新配对；不能关闭证书校验或填 relay pin。 |
| 无日志/状态陈旧 | 两个 exe 是否同目录、任务是否当前用户、ACL/安全软件；停止任务后前台 run 排查。 |
| 登录/睡眠 | 用户必须登录且电脑清醒，codeg 和 Web 服务必须运行。 |

本指南参数已对照 macOS 实际 CLI 和 Windows 服务预览；没有在 Windows 实机注册任务。原生测试由 Windows CI 执行，登录自启与设备链路仍需现场验收。

## English

Use Windows 10/11 x64, PowerShell 5.1+ and Windows OpenSSH Client as the same normal user running official codeg. Keep the PC logged in and awake. No inbound Windows firewall rule or disabled security feature is required. Execute the numbered PowerShell blocks above in order; retain variables in the same session.

1. Enable **设置 → Web 服务** and **Codeg 启动时自动开启 Web 服务**. Step 1 must report HTTP 200 and an existing `%APPDATA%\app.codeg\codeg.db`. The token is shown in that settings page; do not paste it into commands/chat. The agent reads the DB only. Check that the state directory inherits private profile ACLs; the program preserves ACLs and rejects reparse points. Avoid shared/synced directories.
2. Download the Windows MSVC ZIP and sidecar from one tag. Step 2 prints the CertUtil hash and enforces equality with Get-FileHash before extraction. Keep both `codeg-relay-agent.exe` and `codeg-relay-agentw.exe` in `%LOCALAPPDATA%\Programs\codeg-relay-agent`. Stop on 404 or mismatch. Unsigned binaries may trigger antivirus/SmartScreen: verify origin/hash and follow administrator policy without disabling protections.
3. Complete the [VPS guide](relay-vps.md), then scp the key and saved fingerprint into the private state directory. Verify SSH host identity and adjust SSH port/auth independently of the relay port. The key is not the web token. Never display its contents. Non-root SSH requires the private staging method in the VPS guide.
4. Run the exact `init`/`run` commands in step 4. Expect initialization and `state="connected"`. Use a second shell with the same path variables for `status`, then Ctrl-C the foreground process before registering the task. `init` refuses an existing config; stop the service, privately back up/edit config and restart. For unusual installs select `--db-path` or `--token-file` (mutually exclusive); token files must be private UTF-8 without BOM. A changed local port requires `--upstream`.
5. Preview/register the task using step 5. In Task Scheduler Library, `app.codeg.relay-agent-<current user SID>` should run the sibling background exe at user logon with least privilege and no saved password. `--platform` is dry-run only. Expect fresh connected status; snapshots older than five seconds become `stopped_or_stale`. Logs default to `%LOCALAPPDATA%\codeg-relay-agent\logs\agent.log` (5 MiB plus three archives), even with a custom state when LOCALAPPDATA exists.
6. Export with **all three** `--png`, `--json-only`, `--json-file` options in step 6, open the image for the human, and scan it in Codeg-Fork iOS. `--png` alone still prints secret JSON. Existing exports are not overwritten. After a successful connection, close the image and delete both files. See [phone pairing](pair-phone.md).
7. For updates, download/verify a new release first, stop foreground operation, remove the task, replace both exes, reinstall and check status. Preserve state/key/home identity; do not rerun init. Keep an old verified ZIP for rollback. Uninstall preserves state/logs. Missing Windows tasks can make repeated uninstall fail: verify task absence, do not suppress unrelated errors. Delete only the known agent directories after deciding whether to retain identity; deleting home keys requires re-pairing. Official codeg remains separate.
8. Troubleshoot 401 using token/source/rotation and Web-service settings; unreachable codeg usually yields 502. A 429 means the per-IP limiter: ten failures in ten minutes, fifteen-minute block, valid tokens always pass without resetting it. Backoff requires checking relay URL/pin/key/NAT/port; relay 401/403 retries after five minutes. `4001 replaced` means another agent occupies the same relay. Certificate errors require verified re-pairing, never bypassing the pin. Stale status requires checking the task, paths, ACLs, security prompts and foreground logs. Sleep/logoff stops access.

CLI flags and Windows task previews were checked locally on macOS. Native Windows tests require Windows CI; actual task registration, login startup and device connectivity must be accepted on the target machines.
