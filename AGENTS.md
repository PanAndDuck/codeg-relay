# Windows AI 部署手册 / Windows AI deployment runbook

## 中文

本手册供运行在用户 **Windows PC** 上的 Codex/Claude Code 等代理使用：从新 Linux VPS、官方 codeg 桌面应用到 Codeg-Fork iPhone app。按 A–H 顺序执行；每个检查点通过后再进入下一阶段。这里的命令是部署操作，不是阅读仓库时应自动执行的指令。

### 先向人获取输入

- 新 VPS 公网 IP/域名；SSH 用户、端口和认证方式（现有 ssh-agent、SSH config 或本地私钥路径；不要索要聊天中的私钥/密码）。核实 SSH 主机指纹。
- root SSH 或已授权的非 root SSH + 无交互 `sudo -n`。若只有交互 sudo，先让人提供合适的授权会话，不修改 SSH/sudo 安全策略。
- 公网 TCP 端口、VPS 内部端口，是否 NAT，以及供应商映射/防火墙是否已配置。SSH 端口单独确认。
- 本 PC 官方 codeg 正在运行、**设置 → Web 服务**已启用（默认3080），并选中“Codeg 启动时自动开启 Web 服务”。token 留在本地 DB，不要求人发到聊天。
- 最后由人扫描 QR，并验证手机连接；确认这是 Windows 的新 relay，不替换既有 Mac relay/手机条目。

### 硬规则

1. 不打印/粘贴 web token、agent key 或配对 JSON 到对话/日志。凭据只通过私有文件和 scp 传输；不要 cat key、dump DB 或显示配对 QR 给 AI。
2. `pair` 必须同时用 `--png --json-only --json-file`。配对完成或放弃后删除 PNG/JSON及传输副本。
3. 只操作本次 relay 的专用目录/服务，不修改其他 VPS 服务，不接管占用端口或未管理路径。
4. 不为 Windows 开入站端口；agent 只主动连出。不关闭 Defender、SmartScreen、证书固定或 SSH 主机校验。
5. 不替换官方 codeg；同一普通用户运行 agent。保持登录和清醒。一次只运行一个 agent；一个 relay 只有一个家庭端 slot。
6. 遇到退出码非零立即停止该阶段，报告不含秘密的错误和下一步。`$ErrorActionPreference` 不会自动拦截 Windows PowerShell 的 native 退出码，使用下面的检查函数。

### A. Windows 预检

在同一 PowerShell 5.1+ 会话中设置经人确认的参数。示例都是占位值；SSH 身份使用已配置的 ssh-agent/config，若必须指定本地 key，在 ssh/scp 调用添加 `-i` 和其路径。

```powershell
$ErrorActionPreference = 'Stop'
function Assert-Native {
    if ($LASTEXITCODE -ne 0) { throw "Native command failed (exit $LASTEXITCODE); stop this phase." }
}
foreach ($name in @('CODEG_DATA_DIR','CODEG_HOME','CODEG_STATIC_DIR','CODEG_ACP_TMP_ROOT','CODEG_TOKEN','CODEG_PORT','CODEG_HOST')) {
    Remove-Item "Env:$name" -ErrorAction SilentlyContinue
}
$SshTarget = 'root@203.0.113.10'
[int]$SshPort = 22
$PublicHost = 'relay.example.com'
[int]$InternalPort = 18443
[int]$PublicPort = 24443
$Tag = 'v0.1.0'
if ($PublicHost -notmatch '^[a-zA-Z0-9.-]+$') { throw 'Use a DNS name or IPv4 address for this runbook' }
if ($SshPort -lt 1 -or $SshPort -gt 65535) { throw 'Invalid SSH port' }
if ($InternalPort -lt 1024 -or $InternalPort -gt 65535 -or $InternalPort -in @(3306,6379)) { throw 'Invalid internal port' }
if ($PublicPort -lt 1 -or $PublicPort -gt 65535) { throw 'Invalid public port' }
if ($Tag -notmatch '^v[0-9]+\.[0-9]+\.[0-9]+$') { throw 'Select a stable vX.Y.Z tag' }
$RelayUrl = "https://${PublicHost}:$PublicPort"
$HealthUrl = "http://${PublicHost}:$PublicPort/__codeg_relay/v1/health"
$State = Join-Path $env:LOCALAPPDATA 'codeg-relay-agent'
$Bin = Join-Path $env:LOCALAPPDATA 'Programs\codeg-relay-agent'
$Work = Join-Path $env:LOCALAPPDATA "codeg-relay-downloads\$Tag"
$Agent = Join-Path $Bin 'codeg-relay-agent.exe'
$Db = Join-Path $env:APPDATA 'app.codeg\codeg.db'
if (Test-Path -LiteralPath (Join-Path $State 'config.json')) { throw 'Existing home agent: use recovery/update instructions before transferring keys or binaries' }
New-Item -ItemType Directory -Force -Path $State,$Bin,$Work | Out-Null
Get-Command ssh.exe,scp.exe,CertUtil.exe
Get-CimInstance Win32_OperatingSystem | Select-Object Caption,OSArchitecture
Get-CimInstance Win32_Processor | Select-Object Name,Architecture
icacls.exe "$State"
Assert-Native
if (-not (Test-Path -LiteralPath $Db)) { throw 'codeg DB missing: confirm the desktop profile/path' }
if ((Invoke-WebRequest -UseBasicParsing -TimeoutSec 10 'http://127.0.0.1:3080/').StatusCode -ne 200) { throw 'Local Web service unavailable' }
```

**检查点 A：** Windows 10/11 x64（CPU Architecture `9`），OpenSSH 可用，本地 HTTP 200、DB 存在，state ACL 为本用户私有（正常 SYSTEM/Administrators 可访问）。缺工具通过 Windows 正常功能安装；Web 服务失败由人确认 codeg 设置/端口；非默认 DB 用真实路径替换 `$Db`。不要读取 token 解决连通性问题。

### B. VPS 预检

```powershell
$RemoteUid = ssh.exe -p $SshPort $SshTarget 'id -u'
Assert-Native
$RootPrefix = if ($RemoteUid.Trim() -eq '0') { '' } else { 'sudo -n ' }
$RootUid = ssh.exe -p $SshPort $SshTarget "${RootPrefix}id -u"
Assert-Native
if ($RootUid.Trim() -ne '0') { throw 'Authorized root access is required' }
$Preflight = 'uname -srm; cat /etc/os-release; ps -p 1 -o comm=; systemctl --version; systemd-detect-virt --container || true; ss -ltnp; systemctl --failed --no-pager'
ssh.exe -p $SshPort $SshTarget $Preflight
Assert-Native
$PortCheck = 'ss -H -ltn "sport = :{0}"' -f $InternalPort
$Listeners = ssh.exe -p $SshPort $SshTarget $PortCheck
Assert-Native
if ($Listeners) { throw 'Selected internal port is occupied; do not stop its owner' }
ssh.exe -p $SshPort $SshTarget 'for command in bash curl tar sha256sum systemctl install useradd userdel groupdel getent flock ss; do command -v "$command" || exit 1; done'
Assert-Native
$RemoteDir = 'codeg-relay-setup-' + [Guid]::NewGuid().ToString('N')
ssh.exe -p $SshPort $SshTarget "umask 077; mkdir '$RemoteDir'"
Assert-Native
```

**检查点 B：** Linux x86_64/aarch64、PID1=systemd、授权 root 可用、端口空闲，记录已有服务/监听和 `$RemoteDir`。检查每行输出，不仅看最后退出码。若已有本项目安装，走“重跑/恢复”而非占用同名路径。容器名称只提供诊断线索；缺 systemd 或授权时停下，不能为此改动其他服务。

### C. 下载校验并安装 relay

在 Windows 写 UTF-8 无 BOM、LF 脚本，通过 scp 传到 VPS，再执行。默认先使用完整 sandbox；`--public-port` 始终明确填写，NAT 时必须等于供应商公网映射端口。

```powershell
$InstallScript = @'
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
umask 077
cd -- "$(dirname -- "${BASH_SOURCE[0]}")"
base=https://github.com/PanAndDuck/codeg-relay/releases/download/__TAG__
case "$(uname -m)" in
  x86_64|amd64) asset=codeg-relay-x86_64-unknown-linux-musl ;;
  aarch64|arm64) asset=codeg-relay-aarch64-unknown-linux-musl ;;
  *) printf 'Unsupported architecture\n' >&2; exit 1 ;;
esac
for file in "$asset" "$asset.sha256" codeg-relay-deploy.tar.gz codeg-relay-deploy.tar.gz.sha256; do
  curl -fL --retry 3 "$base/$file" -o "$file"
done
sha256sum -c "$asset.sha256"
sha256sum -c codeg-relay-deploy.tar.gz.sha256
tar -xzf codeg-relay-deploy.tar.gz
bash deploy/install.sh --binary "./$asset" --public-host __PUBLIC_HOST__ --port __INTERNAL_PORT__ --public-port __PUBLIC_PORT__ --no-print-key "$@"
systemctl is-active codeg-relay
curl --fail --max-time 10 http://127.0.0.1:__INTERNAL_PORT__/__codeg_relay/v1/health
'@
$InstallScript = $InstallScript.Replace('__TAG__',$Tag).Replace('__PUBLIC_HOST__',$PublicHost).Replace('__INTERNAL_PORT__',[string]$InternalPort).Replace('__PUBLIC_PORT__',[string]$PublicPort)
$ScriptFile = Join-Path $Work 'install-relay.sh'
[IO.File]::WriteAllText($ScriptFile, $InstallScript.Replace("`r`n","`n") + "`n", [Text.UTF8Encoding]::new($false))
scp.exe -P $SshPort "$ScriptFile" "${SshTarget}:${RemoteDir}/install-relay.sh"
Assert-Native
ssh.exe -p $SshPort $SshTarget "${RootPrefix}bash '$RemoteDir/install-relay.sh'"
Assert-Native
```

**检查点 C：** 两个摘要 `OK`、service `active`、本地 health 为 `mode=e2ee` 且尚未连接时 `agent_connected=false`。首个计划版本未发布导致 404 时停止。脚本不更改供应商 NAT/防火墙。需要管理 UFW 时，确认该端口规则范围后在远端执行命令末尾加 `--open-firewall`；不要改其他规则。

失败时：

```powershell
ssh.exe -p $SshPort $SshTarget "${RootPrefix}journalctl -u codeg-relay --no-pager -n 80"
Assert-Native
```

只有出现安装器 namespace/seccomp 提示时，才在原 `ssh ... bash ...` 命令末尾加 `--no-sandbox` 重试；记录这个选择。以后更新必须保留它，否则重跑会恢复 sandbox。其他失败按实际原因处理，不能盲目降级。成功后比较原服务状态，无关服务不得重启。

### D. key 与指纹只走文件

下面兼容 root 或无交互 sudo 的 SSH 用户：以 SSH 用户 owner 创建0600暂存 key，指纹 stdout 直接重定向到文件；scp 后删除远端导出件。

```powershell
$KeyFile = Join-Path $State 'agent.key'
$FingerprintFile = Join-Path $State 'relay-fingerprint.txt'
$ExportCommand = 'umask 077; {0}install -m 0600 -o "$(id -u)" -g "$(id -g)" /etc/codeg-relay/agent.key "{1}/agent.key" && {0}/opt/codeg-relay/bin/codeg-relay fingerprint --data-dir /var/lib/codeg-relay > "{1}/relay-fingerprint.txt"' -f $RootPrefix,$RemoteDir
ssh.exe -p $SshPort $SshTarget $ExportCommand
Assert-Native
scp.exe -P $SshPort "${SshTarget}:${RemoteDir}/agent.key" "$KeyFile"
Assert-Native
scp.exe -P $SshPort "${SshTarget}:${RemoteDir}/relay-fingerprint.txt" "$FingerprintFile"
Assert-Native
ssh.exe -p $SshPort $SshTarget "rm -f '$RemoteDir/agent.key' '$RemoteDir/relay-fingerprint.txt'"
Assert-Native
$RelayFingerprint = (Get-Content -LiteralPath $FingerprintFile -TotalCount 1).Trim()
if ($RelayFingerprint -notmatch '^[0-9a-fA-F]{64}$') { throw 'Invalid relay fingerprint file' }
if ((Get-Item -LiteralPath $KeyFile).Length -eq 0) { throw 'Empty key file' }
```

**检查点 D：** key 非空、指纹第一行格式正确、仅私有文件保存，远端导出件已删除。传输失败时不进入 init；清理残余导出件后重试。不要查看 key 内容。此 fingerprint 只给 agent，手机使用随后生成的 home pin。

### E. 安装 Windows agent、前台运行、计划任务

```powershell
$Base = "https://github.com/PanAndDuck/codeg-relay/releases/download/$Tag"
$Asset = 'codeg-relay-agent-x86_64-pc-windows-msvc.zip'
$Zip = Join-Path $Work $Asset
Invoke-WebRequest -UseBasicParsing "$Base/$Asset" -OutFile $Zip
Invoke-WebRequest -UseBasicParsing "$Base/$Asset.sha256" -OutFile "$Zip.sha256"
CertUtil.exe -hashfile "$Zip" SHA256
Assert-Native
$Expected = ((Get-Content -LiteralPath "$Zip.sha256" -Raw).Trim() -split '\s+')[0]
if ($Expected -notmatch '^[0-9a-fA-F]{64}$' -or (Get-FileHash -LiteralPath $Zip -Algorithm SHA256).Hash -ine $Expected) { throw 'Checksum mismatch' }
$Unpack = Join-Path $Work 'unpacked'
Expand-Archive -LiteralPath $Zip -DestinationPath $Unpack -Force
Copy-Item -LiteralPath (Join-Path $Unpack 'codeg-relay-agent.exe'),(Join-Path $Unpack 'codeg-relay-agentw.exe') -Destination $Bin -Force
& $Agent --help
Assert-Native
if (Test-Path -LiteralPath (Join-Path $State 'config.json')) { throw 'Existing agent: use recovery/update instructions before changing it' }
& $Agent --state-dir "$State" init --relay-url "$RelayUrl" --agent-key-file "$KeyFile" --relay-fingerprint "$RelayFingerprint" --db-path "$Db"
Assert-Native
& $Agent --state-dir "$State" run
```

**前台检查点：** 日志变为 `state="connected"`，随后 Ctrl-C；不要把长期运行当作工具超时并再启动副本。若工具无法交互停止，只结束本次启动的已知 PID，不使用按进程名杀所有实例。前台失败按 [Windows 排障](docs/home-windows.md) 处理。init 不读取 token，run 才检验 DB；不能仅凭 init 成功进入配对。

```powershell
& $Agent --state-dir "$State" install-service --dry-run --platform windows
Assert-Native
& $Agent --state-dir "$State" install-service
Assert-Native
$Sid = [System.Security.Principal.WindowsIdentity]::GetCurrent().User.Value
$TaskName = "app.codeg.relay-agent-$Sid"
schtasks.exe /Query /TN "$TaskName" /V /FO LIST
Assert-Native
```

**检查点 E：** 同目录双 exe；Task Scheduler Library 中当前用户任务运行，Action 指向 `codeg-relay-agentw.exe`，非管理员、无需保存密码。SmartScreen/杀毒提示由人按正常策略核实，不能关闭防护。原生 Windows 任务/登录生命周期需要目标机器验收；跨平台 dry-run 不能代替它。

### F. 公网和本地状态双检查

```powershell
Start-Sleep -Seconds 3
$StatusText = & $Agent --state-dir "$State" status
Assert-Native
$Status = $StatusText | ConvertFrom-Json
$Health = Invoke-RestMethod -TimeoutSec 10 -Uri $HealthUrl
if ($Status.state -ne 'connected') { throw 'Agent is not connected; inspect local logs' }
if ($Health.mode -ne 'e2ee' -or $Health.agent_connected -ne $true) { throw 'Public relay does not report the home connected' }
$Status | Select-Object state,pid,updated_at,last_error
$Health | Select-Object status,mode,agent_connected,version
```

**检查点 F：** fresh `connected` + 公网 `agent_connected=true`。若 status stale，查看 `%LOCALAPPDATA%\codeg-relay-agent\logs\agent.log` 和任务 Action/ACL。公网失败但 VPS 本地 health 成功，检查公网端口、NAT、供应商防火墙/DNS。不要在 Windows 放开入站端口，不用 HTTPS health 测 relay pin。401 检查 token；429 等待 Retry-After（正确 token 仍通过）；502 检查 codeg/Web 服务。

### G. 人扫描 iPhone QR

```powershell
$PairPng = Join-Path $State 'pair.png'
$PairJson = Join-Path $State 'pair.json'
& $Agent --state-dir "$State" pair --name 'Windows Home' --png "$PairPng" --json-only --json-file "$PairJson"
Assert-Native
Start-Process -FilePath "$PairPng"
```

**检查点 G：** 人在 **Codeg-Fork iOS app → 添加服务器 → 扫码**，完成一次鉴权操作（例如读取工作区）和蜂窝网络访问，并确认原 Mac 条目仍可用。必要时通过私有渠道粘贴 JSON，不能经聊天转发。证书报错先核实 home 身份，不跳过固定校验。只有人能完成的扫描尚未完成时，明确记录“等待手机验收”，不能宣称全链路成功。

确认成功或放弃后，关闭图片并清理：

```powershell
Remove-Item -LiteralPath $PairPng,$PairJson -Force
if ((Test-Path -LiteralPath $PairPng) -or (Test-Path -LiteralPath $PairJson)) { throw 'Pairing cleanup incomplete' }
```

### H. 向人交付

报告版本、VPS service/目录、内部与公网端口、Windows 双 exe 路径、state/log路径、任务名，以及 A–G 哪些实际通过。说明更新/卸载入口、保持登录清醒的要求、是否用了 `--no-sandbox`。仅报告脱敏路径/地址；不附 key/token/JSON/QR/真实指纹。检查：

```powershell
& $Agent --version
Assert-Native
ssh.exe -p $SshPort $SshTarget "${RootPrefix}/opt/codeg-relay/bin/codeg-relay --version"
Assert-Native
```

安装参数保存在本地私有运维记录中（包括 `$RemoteDir` 和 sandbox/UFW 选择）；公开交付只使用占位值。链接 [VPS](docs/relay-vps.md)、[Windows](docs/home-windows.md)、[配对](docs/pair-phone.md)、[安全/轮换](SECURITY.md)。

### 重跑、恢复、更新与全部回滚

| 阶段 | 重跑原则 |
| --- | --- |
| A/B | 只读检查可重复；已装 agent 或已占用的 relay 端口应确认所有者后转更新，不能直接重做首次安装。保留原 `$RemoteDir`。 |
| C | 相同参数重跑保留 relay key/cert。校验错误重新下载；保留 NAT/sandbox/UFW 选项。脚本拒绝未管理路径，不删除 marker 逃避检查。 |
| D | 只在确认更新同一个 relay 时覆盖本地 key，先停止 agent；传输失败清理远端导出件。 |
| E | 先停止/卸载既有任务，确认前台进程结束，再替换双 exe。init 不可重复；私有备份/edit config，保留 home cert/key。重复安装任务会覆盖任务定义，不能拿它代替停止旧进程。 |
| F | 可重复观测；检查新鲜状态与目标 URL，排除两个 agent 争用 slot。 |
| G | 旧导出文件不能覆盖；确认用途后删除再生成，每次都清理。home `rotate-cert` 后全部手机重配。 |
| H | 只记录实际证据，待人完成的验收单独列出。 |

更新先下载并校验新标签（含 deploy 包），保留旧版本用于回退。Windows 用以下命令移除任务后替换两个 exe，保留 state，重装并重新执行 F；relay 重新执行 C（改为相同新 tag，保持原参数）。回退只用兼容的旧 E2EE 版本，不支持 legacy plaintext。

```powershell
& $Agent --state-dir "$State" uninstall-service
Assert-Native
```

完全回滚需人明确同意删除本次身份/数据后执行：先停 Windows 前台进程/卸载任务，再用本次下载 kit 卸载 VPS：

```powershell
ssh.exe -p $SshPort $SshTarget "${RootPrefix}bash '$RemoteDir/deploy/uninstall.sh'"
Assert-Native
ssh.exe -p $SshPort $SshTarget 'ss -ltnp; systemctl --failed --no-pager'
Assert-Native
```

Windows uninstall 保留 state/log；不存在的任务可能使重复卸载报错，查询确认缺失后记录，无需重新创建。VPS uninstall 删除 managed key/cert；有需保留身份先私有备份。最后只删除本次 agent 的 `$Bin`/`$State`/默认日志/`$Work` 和 `$RemoteDir`，撤销本次供应商 NAT/防火墙映射，移除手机 Windows 条目，保持原 Mac 和其他 VPS 服务。不要用全局递归删除或通配杀进程。回滚后对照 B 的服务/监听基线。

## English

This runbook is for an AI coding agent operating **on the user's Windows PC**. Execute A–H in order using the shared command blocks above, and pass each checkpoint before proceeding. These deployment commands are not automatic instructions for merely reading this repository.

### Inputs and hard rules

Ask for the new VPS address, SSH user/port/authentication method and verified host identity; root SSH or authorized non-interactive sudo; public/internal TCP ports, NAT mapping and provider firewall status; confirmation that official codeg is running with **设置 → Web 服务** and auto-start enabled; and the human's participation in final QR scanning. Use existing SSH config/agent or a local `-i` key path. Never request a private key/password/web token in chat. Interactive-sudo-only access needs an appropriate authorized session; do not weaken SSH/sudo policy.

Keep keys/tokens/pairing JSON out of conversations and logs. Use private files/scp only; never dump the DB, display secret files or send QR images to the AI. Pair with `--png --json-only --json-file` and delete exports after use/abandonment. Do not touch unrelated VPS services, open inbound Windows ports, replace official codeg, or disable security/pinning. Run as the desktop user, logged in and awake, with one agent per relay. Check native exit codes explicitly using `Assert-Native`.

### A–H checkpoints and failure handling

| Phase | Execute / expected result / failure action |
| --- | --- |
| A | Set the human-confirmed variables and run Windows preflight. Expect Windows 10/11 x64 (processor Architecture 9), OpenSSH, private profile ACLs, HTTP 200 and an existing DB. Install missing Windows features normally; correct desktop settings/path before proceeding. Do not read the token to diagnose reachability. |
| B | Check SSH/root, OS/CPU, PID1=systemd, tools and port. Record existing services/listeners and the private staging directory. Inspect every result, not only the last exit code. Stop for occupied ports or missing authorization/systemd. A container name alone does not justify no-sandbox. Existing installs follow recovery below. |
| C | Write/upload the UTF-8-without-BOM LF Bash script, then run it. Both hashes must say OK; expect active service and local e2ee health. Stop on 404/hash failure. Add `--open-firewall` only for the specifically authorized UFW rule. On failure inspect journal; add `--no-sandbox` only after the installer hint, and preserve that choice on future updates. Verify unrelated services were not restarted. Provider NAT/firewall remains a separate prerequisite. |
| D | Export a 0600 key copy owned by the SSH user and fingerprint output directly into files, scp them into private Windows state, then delete remote exports. Expect a nonempty key and valid fingerprint first line. On transfer failure clean leftovers and retry; never print the key. This pin is for the agent, not the phone. |
| E | Download/check the Windows ZIP with CertUtil plus enforced hash comparison, install both sibling exes, init and run foreground. Expect connected logs, then Ctrl-C before task installation. Stop only the known foreground PID if tools cannot send Ctrl-C. Preview/register the task; expect the current-user task running the background exe with least privilege. Resolve unsigned-binary prompts through normal human/admin policy. Cross-platform previews cannot prove native registration/login behavior. |
| F | Run fresh status and public HTTP health checks. Require local connected state and `agent_connected=true`. For stale status inspect task action/ACL/logs. If only public health fails, inspect provider NAT/port/DNS/firewall, never Windows inbound rules. Check token for 401, Retry-After for 429, local Web service for 502. HTTPS health cannot fetch the relay pin. |
| G | Export/open the private PNG for the human to scan in Codeg-Fork iOS. Require an authenticated operation and cellular access, plus the existing Mac entry still working. Private JSON paste is an alternative, never via chat. Do not bypass certificate errors. If scanning is pending, report pending phone acceptance. Delete both exports after success or abandonment. |
| H | Report versions, service/install/state/log paths, ports, task name, sandbox choice, actual checkpoints and update/uninstall instructions. Keep real credentials, QR/JSON and fingerprints out of the report. Privately retain install parameters and remote staging location; use placeholders publicly. |

### Idempotency, updates and rollback

A/B reads are repeatable; existing state/ports require ownership checks and the update path. C with the same parameters preserves relay key/certificate; preserve public-port/sandbox/UFW choices and never bypass markers. D may overwrite a key only for the confirmed same relay with the agent stopped; remove transfer leftovers. E requires stopping foreground/task processes before replacing both exes. Init refuses existing config: privately back up/edit config while retaining the home identity. Re-registering a task is not a substitute for stopping an existing process. F is repeatable; eliminate relay-slot contention. G refuses existing exports: inspect purpose, remove and regenerate, then clean up. Home certificate rotation requires re-pairing every phone. H records only actual evidence.

For updates, verify the new tag and retain the old package first. Use the uninstall-service block, replace both Windows exes, preserve state, reinstall and repeat F. Re-run C with the same new tag and original options for relay. Roll back only to a compatible E2EE release; there is no legacy plaintext fallback.

For complete removal, obtain explicit approval to delete this deployment's identity/data, stop foreground operation/uninstall the Windows task, then run the VPS uninstall block above. A missing Windows task can make repeat uninstall fail; query and record absence rather than recreating it. Windows uninstall preserves state/logs, while VPS uninstall deletes managed keys/certificates: make a private identity backup first if needed. Remove only this task's known agent/download/staging directories and provider mappings, and the phone's Windows entry. Preserve Mac and unrelated services; compare against phase B. Never use broad process killing or unscoped recursive deletion. See the [VPS](docs/relay-vps.md), [Windows](docs/home-windows.md), [pairing](docs/pair-phone.md) and [security](SECURITY.md) guides.
