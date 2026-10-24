# Standalone relay agent implementation plan

Architecture: independent Rust crate; reuse the existing mux and home identity,
replace the in-process codeg router with an authenticated HTTP/1.1 reverse proxy.
No dependency on src-tauri. All validation uses temporary data and fake upstreams.

- [x] Auth/config/identity: `auth.rs`, `config.rs`, `secrets.rs`, `certificate.rs`.
  Mirror upstream bed89314 token extraction and public route exceptions; hash
  candidates with SHA-256 then compare fixed-size digests using subtle. Test
  malformed protocols, empty tokens, route boundaries, IP isolation, expiry,
  config validation, read-only SQLite rotation and private-file permissions.
- [x] Agent lifecycle: `transport.rs`, `client.rs`, `status.rs`, `serve.rs`.
  Pin relay before sending the key, ALPN codeg-relay-agent/1, unchanged mux
  heartbeat/replacement, backoff 1/2/4/8/16/30 seconds with jitter, 300 seconds
  on auth rejection. Incoming TLS only; permits survive upgraded connections.
- [x] Proxy: `proxy.rs`, `buffer.rs`, `headers.rs`, `tls.rs`.
  One reusable upstream HTTP1 connection per phone, streaming bodies, remove
  Connection-nominated and standard hop headers, reconstruct WS upgrade headers.
  Bounded duplex buffers (8 MiB/direction), 32 KiB pump chunks and 30-second
  saturation timeout; cancel both endpoints and all connection tasks on stall.
  Configure upstream TCP_NODELAY/keepalive and optional HTTPS leaf pin.
- [x] Real integration: expose relay library + listener/shutdown entry point,
  preserving its CLI. `tests/relay_proxy.rs` starts actual relay and agent with
  a fake axum upstream on loopback ephemeral ports; check >=10 MiB, chunked
  POST, SSE timing, WS echo/push, auth interception, rotation, slow readers,
  unaffected concurrent streams, and restart with the same relay identity.
- [x] CLI/service: `main.rs`, `pairing.rs`, `service.rs`, `logging.rs`.
  Private init/run/status/rotation; pairing JSON + terminal QR + optional PNG.
  LaunchAgent/systemd user installation with dry-run, no shell interpolation,
  bounded rotated logs. Test using temp locations; never install a real service.
- [x] Delivery: bilingual README, tests/clippy for all three crates, macOS
  arm64 release, feasible Linux musl check, local commits and Chinese report.

Upstream verification found two specification nuances: v0.39.0 token-auth
401 bodies are text/plain, not JSON; its public API exceptions remain public
and self-authenticate where applicable. Opt-in server proxy-JWT auth does not
replace this connector's required web token. A finite lossless buffer can
absorb only bounded traffic; once full, upstream socket backpressure lasts at
most the configured stall timeout before both ends are closed.

Final validation (2026-10-23): agent 43 tests, relay 29, tunnel 32; all passed,
none ignored. All three pass clippy with `--all-targets -- -D warnings`.
Agent and relay pass rustfmt; untouched tunnel has baseline formatting diffs.
Release artifacts: macOS arm64 5,830,944 bytes; Linux x64 musl 6,263,664 bytes
(static, stripped; cross-build only). No real service or live endpoint used.
