# iPhone 配对 / iPhone pairing

## 中文

使用 **Codeg-Fork iOS app**。前提：家庭 agent 的 `status` 为 `connected`，公网 HTTP health 中 `agent_connected=true`，官方 codeg 与 Web 服务保持运行。健康状态只是连通性提示，证书固定才验证家庭端身份。

### 1. 生成只落文件的配对材料

Windows，在 [Windows 指南](home-windows.md) 的相同 PowerShell 变量环境：

```powershell
$PairPng = Join-Path $State 'pair.png'
$PairJson = Join-Path $State 'pair.json'
& $Agent --state-dir "$State" pair --name 'Windows Home' --png "$PairPng" --json-only --json-file "$PairJson"
if ($LASTEXITCODE -ne 0) { throw 'Pair export failed' }
Start-Process -FilePath "$PairPng"
```

macOS/Linux，在 [Unix 指南](home-macos-linux.md) 的相同变量环境：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
"$agent" --state-dir "$state" pair --name 'Home' --png "$state/pair.png" \
  --json-only --json-file "$state/pair.json"
```

用本机图片查看器打开 PNG。`--png` 单独使用仍将 JSON 打印到 stdout；同时传 `--json-only --json-file` 才抑制终端 QR/JSON。已存在文件不会覆盖：确认其用途后删除旧材料，再生成。两份文件都含 web token，不能贴聊天、上传图片站、进入录屏/截图/日志或同步目录。

### 2. 手机添加服务器

1. 在 Codeg-Fork app 的服务器列表选择添加服务器，再选择扫码，由人扫描电脑上的 PNG。
2. 若扫码不可用，可在可信私有渠道将生成的 JSON 交给手机，使用粘贴配对 JSON 的入口；不要让 AI 在聊天中转 JSON。粘贴后清理剪贴板/临时传输副本。
3. 协议为 v1、`mode=relay`：`url` 为公网 relay HTTPS URL，`token` 为 codeg web token，**`fp` 为 HOME 证书指纹**，`name` 为可选名称。不要手工改成 relay 指纹，也不要将 agent key 填入 token。
4. 连接并完成一次需要鉴权的操作，例如加载工作区列表；仅扫码成功不能证明业务连接可用。再切换到蜂窝网络验证家庭路由器外仍可访问。
5. 确认成功后关闭图片，在 Windows 执行 `Remove-Item -LiteralPath $PairPng,$PairJson -Force`；Unix 执行下面清理命令。

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
rm -f -- "$state/pair.png" "$state/pair.json"
```

### 3. 多个家庭端

Mac 与 Windows 各用自己的 relay 实例、home 身份及 web token，在手机中分别添加命名清楚的条目。保留现有 Mac 条目；新增 Windows 不需要改它。单 relay 只有一个 slot，新 agent 会以 `4001 replaced` 替换旧 agent，两个电脑共享它会反复掉线。

### 4. 轮换与证书错误

主动轮换 home 证书：Windows 执行 `& $Agent --state-dir "$State" rotate-cert`；Unix：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
"$agent" --state-dir "$state" rotate-cert
```

运行中的 agent 在两秒内载入新证书，现有 TLS 连接可自然结束；为每台手机重新生成并导入配对材料，替换对应家庭端旧条目，最后删除材料。

若手机突然提示证书变化，先停止连接，核对是否执行过 `rotate-cert`、state 是否丢失/换目录、URL 是否指向正确 relay。通过可信的本机 `status` 核实 home 身份，确认原因后才重配。不要接受未知新 pin、关闭校验或拿 relay pin 替代。只有 relay 证书改变时，通过已验证 SSH 获取新 relay 指纹，更新 agent 私有配置并重启；home 证书未变时不需要因此重配手机。

若公网 URL 也变更，更新手机条目的 URL 或重新导入配对，即使 home 身份未变。Token 轮换时重新生成配对材料（agent 默认最长 60 秒刷新 token）。502 优先检查 codeg/Web 服务，401 检查 token，429 按 Retry-After 等待；正确 token 始终可以通过限流器。无法解释的 home 证书变化按 [安全问题](../SECURITY.md) 处理。

## English

Use the **Codeg-Fork iOS app**. First verify connected agent status and public HTTP health with `agent_connected=true`; keep codeg and its Web service running. Health proves reachability only; the pinned certificate establishes home identity.

1. Run the Windows or Unix export block above with variables from the corresponding home guide. Always combine `--png`, `--json-only` and `--json-file`: PNG alone still prints secret JSON. Existing files are not overwritten. Open the PNG locally for the human. Both exports contain the web token; keep them out of chat, logs, screenshots, recordings, public upload services and synced folders.
2. In the app's server list, add a server and scan the QR. Alternatively transfer the generated JSON privately and use the paste-pairing-JSON entry point, then clear clipboard/transfer copies. The v1 relay schema uses `url`, `token`, `fp` and optional `name`: **fp is the HOME certificate**, token is the codeg web token. Neither the relay fingerprint nor agent key belongs in those fields.
3. Connect and perform an authenticated action, such as loading the workspace list, then try cellular access. Successful QR parsing alone is insufficient. Close the image and delete both exports using the Windows cleanup command or Unix cleanup block above.
4. Each Mac/Windows home needs its own relay instance and phone entry. Preserve the existing Mac entry when adding Windows. Sharing a relay slot replaces the old agent with `4001 replaced` and can cause reconnect churn.
5. `rotate-cert` changes the home certificate. A running agent reloads within two seconds; existing TLS sessions may finish. Regenerate/reimport pairing for every phone and remove the corresponding old entry/materials. For an unexpected certificate error, stop connecting, verify deliberate rotation/state loss/wrong URL and check the trusted local home identity before re-pairing. Never bypass certificate checks or substitute a relay pin. A relay-only certificate change requires fetching its pin through trusted SSH and restarting the updated agent; unchanged home identity does not require phone re-pairing.
6. If the public URL changes, update the phone entry's URL or reimport pairing even if the home identity is unchanged. After a web-token change, regenerate pairing; the agent refreshes within sixty seconds by default. Check local codeg for 502, token for 401, and Retry-After for 429 (valid tokens still pass). Treat unexplained home identity changes as a [security issue](../SECURITY.md).
