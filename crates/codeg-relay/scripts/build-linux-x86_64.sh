#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
repo_root=$(cd -- "$script_dir/../../.." && pwd)
crate_dir=$repo_root/crates/codeg-relay
mkdir -p "$crate_dir/dist"
docker run --rm --platform linux/amd64 \
  -v "$repo_root:/src" \
  -v codeg-relay-cargo-registry:/usr/local/cargo/registry \
  -v codeg-relay-cargo-git:/usr/local/cargo/git \
  -w /src/crates/codeg-relay \
  -e CARGO_BUILD_JOBS="${CARGO_BUILD_JOBS:-4}" \
  -e CARGO_TARGET_DIR=/src/crates/codeg-relay/target/linux-amd64 \
  rust:1-alpine sh -eu -c '
    unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
    apk add --no-cache musl-dev
    cargo build --release --locked --target x86_64-unknown-linux-musl
    cp target/linux-amd64/x86_64-unknown-linux-musl/release/codeg-relay dist/codeg-relay-x86_64-unknown-linux-musl
  '
binary=$crate_dir/dist/codeg-relay-x86_64-unknown-linux-musl
file "$binary"
file "$binary" | grep -Eq 'static(ally linked|-pie linked)' || { echo 'Expected a static binary' >&2; exit 1; }
(
  cd "$crate_dir/dist"
  shasum -a 256 codeg-relay-x86_64-unknown-linux-musl >codeg-relay-x86_64-unknown-linux-musl.sha256
)
docker run --rm --platform linux/amd64 \
  -v "$crate_dir/dist:/artifacts:ro" rust:1-alpine sh -eu -c '
    unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
    export CODEG_DATA_DIR=$(mktemp -d) CODEG_HOME=$(mktemp -d)
    cp /artifacts/codeg-relay-x86_64-unknown-linux-musl /tmp/codeg-relay
    /tmp/codeg-relay --version
    /tmp/codeg-relay gen-key >/tmp/agent.key
    /tmp/codeg-relay serve --listen 127.0.0.1:18443 --self-signed --public-host localhost,127.0.0.1 --data-dir /tmp/relay-state --agent-key-file /tmp/agent.key &
    relay_pid=$!
    trap '\''kill -TERM "$relay_pid" 2>/dev/null || true'\'' EXIT
    sleep 3
    kill -0 "$relay_pid"
    /tmp/codeg-relay fingerprint --data-dir /tmp/relay-state
    kill -TERM "$relay_pid"
    wait "$relay_pid"
    trap - EXIT
  '
cat "$binary.sha256"
