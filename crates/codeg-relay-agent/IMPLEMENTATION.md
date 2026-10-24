# 实现说明与历史验收 / Implementation and historical validation

## 中文

### 当前结构

- `config.rs` / `secrets.rs` / `token.rs`：私有配置、平台路径和只读 SQLite/token 文件。Windows 支持 drive/UNC/verbatim 路径，继承 profile ACL 并拒绝相关 reparse point；Unix 保持 owner/0600检查。
- `certificate.rs` / `pairing.rs`：持久 home 身份、证书轮换、v1 relay 配对。手机固定 home 指纹；配对材料包含 web token，默认 stdout 行为须由自动化显式抑制。
- `transport.rs` / `client.rs` / `status.rs`：relay pin、ALPN、共享 mux、心跳、替换/退避和本地状态。
- `auth.rs` / `limiter.rs`：上游兼容 token 提取/公开路由，SHA-256 摘要恒定时间比较，错误凭据限流；正确 token 不受封锁，但成功不清空失败状态。
- `proxy.rs` / `buffer.rs` / `headers.rs` / `tls.rs`：HTTP/1.1连接复用、body/SSE streaming、WS升级、hop-header处理、有限缓冲和停滞取消。
- `runtime.rs` / `service.rs` / `logging.rs` / `windows_session.rs`：共享运行循环、平台退出信号、用户服务和轮转日志。Windows console CLI 与 GUI-subsystem 后台入口分离，后者在同目录。
- `tests/relay_proxy.rs`：真实 relay/agent + fake upstream，仅临时凭据和loopback；`tests/cli.rs`、`tests/windows.rs`：CLI、服务预览、Windows路径/导出/后台行为。

桌面应用是独立 upstream，无 src-tauri 依赖。401 为 text/plain；上游公开例外保留自己的 ticket/HMAC/capability 验证。有限缓冲只能吸收有限突发，满后可能有 TCP 回压，超时关闭两端；内存界限按流计算。

### 历史快照，不是当前运行证明

基线提交 `6f832a81bf8f397cc64bcdfdf14320f6eb3fa405` 的已记录 macOS 集成验收：

| crate | 分布 | 通过 |
| --- | --- | ---: |
| codeg-tunnel | 13 unit + 19 mux | 32 |
| codeg-relay | 12 unit + 2 CLI + 15 relay | 29 |
| codeg-relay-agent | 36 unit + 4 CLI + 16 relay/proxy | 56 |
| 合计 | 0失败、0忽略，doc-tests为0 | 117 |

该快照三个 crate 的 Clippy/fmt 均通过；另外打包12项、安装helper30项（macOS/Linux两平台同一套）、隔离容器部署smoke7组通过。Windows GNU双exe、测试链接/Clippy以及x64 Linux musl构建通过。旧的“agent43项、tunnel格式差异”是更早开发记录，不代表该基线。

这些结果不能证明 Windows 原生 MSVC执行、非管理员任务注册、真实登录/退出、VPS systemd/NAT 或手机配对。Windows CI及实际部署需各自验收；容器smoke使用服务管理替身。本文件只保存历史边界，当前提交是否通过必须重新运行相应检查，不把历史数字当作新测试输出。

## English

The implementation is split into private configuration/token handling, persistent home identity/pairing, pinned relay transport/lifecycle/status, token authentication and limiter, streaming HTTP/WS proxying, and shared runtime/platform services. The file map above identifies each module. Windows uses a console CLI plus a sibling GUI-subsystem background exe, profile ACLs/reparse checks, and a per-user task. Unix retains owner/mode checks. No desktop/src-tauri dependency is introduced.

The upstream compatibility details matter: 401 is text/plain; public routes retain their own capability/ticket/HMAC checks. Valid tokens always pass without clearing failed-auth state. Buffer limits are per stream and cannot prevent all TCP backpressure; stalls eventually close both endpoints.

**Historical evidence only:** the integration snapshot at `6f832a81bf8f397cc64bcdfdf14320f6eb3fa405` recorded tunnel 32, relay 29 and agent 56 passing macOS tests (117 total, zero failures/ignored), plus clean Clippy/fmt for all three. Packaging had 12 checks, installer helpers 30 checks on two platforms, and isolated deployment smoke seven groups. GNU Windows binaries/test links/Clippy and x64 musl builds also passed. Earlier figures of 43 agent tests and tunnel formatting differences are obsolete development history.

These numbers are not a fresh run for any later commit and do not prove native Windows MSVC execution, task registration/login/shutdown, real VPS systemd/NAT or phone pairing. CI and target deployment must validate those boundaries; container smoke used service-manager substitutes. Re-run relevant checks before making current success claims.
