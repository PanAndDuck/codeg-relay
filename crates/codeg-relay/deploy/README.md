# Relay 部署包 / Relay deployment kit

## 中文

本目录是 Linux VPS 安装包；家庭端由**独立 codeg-relay-agent**主动连接，继续使用官方 codeg 桌面应用。本包管理固定的一套目录/用户/unit，每个VPS一个实例；一个relay只服务一个家庭端，新agent以`4001 replaced`替换旧连接。

完整指南：[VPS部署](https://github.com/PanAndDuck/codeg-relay/blob/main/docs/relay-vps.md)、[Windows家庭端](https://github.com/PanAndDuck/codeg-relay/blob/main/docs/home-windows.md)、[macOS/Linux](https://github.com/PanAndDuck/codeg-relay/blob/main/docs/home-macos-linux.md)、[iPhone配对](https://github.com/PanAndDuck/codeg-relay/blob/main/docs/pair-phone.md)、[AI手册](https://github.com/PanAndDuck/codeg-relay/blob/main/AGENTS.md)。这些链接在独立解压的deploy包中也可使用。

### 最短安装路径

需要Linux x86_64/aarch64、systemd、root、curl/tar/sha256sum、useradd/userdel/groupdel/getent、flock/ss/coreutils。供应商必须开放公网TCP端口，NAT时先映射公网端口到内部监听端口；不要修改其他服务。

在VPS root shell选择架构、下载同一版本（首次计划v0.1.0）的binary/sidecar和本包：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
umask 077
mkdir -p /root/codeg-relay-setup/v0.1.0
cd /root/codeg-relay-setup/v0.1.0
base=https://github.com/PanAndDuck/codeg-relay/releases/download/v0.1.0
case "$(uname -m)" in
  x86_64|amd64) asset=codeg-relay-x86_64-unknown-linux-musl ;;
  aarch64|arm64) asset=codeg-relay-aarch64-unknown-linux-musl ;;
  *) exit 1 ;;
esac
for file in "$asset" "$asset.sha256" codeg-relay-deploy.tar.gz codeg-relay-deploy.tar.gz.sha256; do
  curl -fL "$base/$file" -o "$file"
done
sha256sum -c "$asset.sha256"
sha256sum -c codeg-relay-deploy.tar.gz.sha256
tar -xzf codeg-relay-deploy.tar.gz
bash deploy/install.sh --binary "./$asset" --port 18443 \
  --public-port 24443 --public-host relay.example.com --no-print-key
systemctl is-active codeg-relay
curl --fail --max-time 10 http://127.0.0.1:18443/__codeg_relay/v1/health
```

最新下载base为 `https://github.com/PanAndDuck/codeg-relay/releases/latest/download`。404或摘要错误必须停止；保留完整`deploy/`，包括`install-common.sh`、`codeg-relay.service`、`no-sandbox.conf`和卸载脚本。

### 选项与检查点

| 安装选项 | 语义 |
| --- | --- |
| `--binary PATH` | 必填；安装前核验ELF架构和存在的相邻`.sha256`；缺摘要仅警告，因此始终下载sidecar。 |
| `--public-host IP[,DNS]` | 必填；证书SAN列表，第一项用于输出URL；不含scheme/端口，已存在证书不自动轮换。 |
| `--port N` | 内部端口，默认18443，1024–65535，拒绝3306/6379。 |
| `--public-port N` | 公网端口1–65535，默认内部端口；只影响显示URL/health，不改变监听/UFW，不创建NAT。 |
| `--no-print-key` | 不打印key，包括首次生成；只显示文件路径及scp提示。 |
| `--no-sandbox` | 仅安装器提示namespace/seccomp失败后重试时添加。保留账号/NoNewPrivileges/资源上限；不带它重跑恢复加固。 |
| `--open-firewall` | 明确授权管理内部端口的UFW TCP规则；不启用UFW，不改云防火墙，保留既有规则。 |
| `-h` / `--help` | 帮助。 |

预期service `active`、health `mode=e2ee`，agent连接前`agent_connected=false`，连接后true。公网测试用 `http://relay.example.com:24443/__codeg_relay/v1/health`，特意使用HTTP。health未认证，不是证书信任来源。

最终输出三个家庭agent输入：relay HTTPS URL、relay证书SHA-256 `<fingerprint>`、`/etc/codeg-relay/agent.key`文件路径。使用已核实身份的SSH/scp把key直接复制到家庭端私有文件，不能贴聊天。独立agent使用`init`/`run`/`install-service`；它读取桌面应用**设置 → Web 服务**的token来源。手机扫描agent导出的QR，固定**home证书**，不能使用relay指纹。PNG/JSON包含token，配对后删除。

### 更新与撤销

日志：`journalctl -u codeg-relay --no-pager -n 80`。只在安装器提示namespace/seccomp限制时按原命令追加`--no-sandbox`。安装路径为`/opt/codeg-relay`、`/etc/codeg-relay`、`/var/lib/codeg-relay`和专用systemd unit。重复安装保留key/cert，更新binary并重启；每次保持原公网端口和sandbox/UFW选项。下载并验证兼容旧版本可回退；不支持legacy plaintext迁移。

在本包父目录执行卸载（会删除relay身份，需要保留时先私有备份）：

```bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
bash deploy/uninstall.sh
```

只移除受管理service、用户/组、三个目录、受管理sandbox drop-in及自身新增且仍匹配的UFW规则；保留其他管理员drop-in/既有规则。外部改过规则会停下待核对。另撤销供应商映射；对照部署前监听/服务。重新安装若relay身份改变，更新agent pin/key；home身份不变则无需因此重配手机。home身份位于agent state的`relay/home-tls/{cert.pem,key.pem}`，`rotate-cert`后所有手机重配，无桌面内置relay或设置页证书再生流程。

## English

This complete Linux deployment kit manages one relay service/account/path set per VPS. The standalone home agent dials out alongside official codeg; one relay slot serves one home, replacing an old connection with `4001 replaced`. The links above work both from the repository and extracted archive.

Run the root-shell download/install block above on Linux x86_64/aarch64 with systemd and the listed OS tools. Use one tag (planned first release `v0.1.0`) or the stated latest base; stop on 404 or checksum failure. Keep all adjacent deploy files. Configure provider TCP access/NAT separately and leave other services alone.

Required `--binary` verifies ELF architecture and a present `.sha256`; always download the sidecar because absence merely warns. Required `--public-host IP[,DNS]` contains hosts only, with the first used in output. `--port` is the internal/UFW port (18443 default, 1024–65535 excluding 3306/6379); `--public-port` advertises the external port (1–65535, defaults to internal) without configuring NAT. Always use `--no-print-key`. Use `--open-firewall` only for explicitly authorized UFW rule management; it does not enable UFW or manage cloud rules. Add `--no-sandbox` only after the installer hint; a later run without it restores hardening. `-h`/`--help` prints usage.

Expect active service and e2ee HTTP health, with agent_connected false before the home connects and true afterward. Public HTTP health is unauthenticated, not a certificate trust source. Retain the relay HTTPS URL, relay fingerprint and key-file path. Transfer the key through verified SSH/scp directly into a private home file, never chat. Initialize/run/install the independent agent with its platform guide. Phones scan its QR and pin the home certificate, not relay. Pairing files contain the web token and must be deleted afterward.

Inspect `journalctl -u codeg-relay --no-pager -n 80` for failures. Reinstalling preserves key/cert and restarts with the replacement binary; preserve public-port/sandbox/firewall choices. Roll back only to a compatible E2EE release, with no legacy plaintext migration. The uninstall command above deletes managed service/account/paths/identity and owned unchanged firewall rules, preserving unrelated rules/drop-ins; changed rules require review. Remove provider mappings separately. A new relay identity requires updating the agent pin/key. Only home identity changes require phone re-pairing; home cert/key live in agent state and rotate through `rotate-cert`, not desktop relay settings.
