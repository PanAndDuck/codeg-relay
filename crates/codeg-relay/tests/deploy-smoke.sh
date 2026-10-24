#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
deploy=$script_dir/../deploy

# This mode only sources helpers and writes fixtures under mktemp; it never runs
# the installer/uninstaller and is safe on macOS (including Bash 3.2).
if [[ ${1:-} == --helpers-only ]]; then
  (($# == 1)) || exit 1
  # shellcheck source=../deploy/install-common.sh
  source "$deploy/install-common.sh"
  helper_tmp=$(mktemp -d)
  trap 'rm -rf -- "$helper_tmp"' EXIT
  helper_count=0
  pass() { helper_count=$((helper_count + 1)); printf 'PASS: %s\n' "$1"; }
  fail() { printf 'FAIL: %s\n' "$*" >&2; exit 1; }
  rejects() {
    local label=$1 pattern=$2
    shift 2
    if "$@" >"$helper_tmp/rejected" 2>&1; then fail "$label was accepted"; fi
    grep -Eq "$pattern" "$helper_tmp/rejected" || fail "$label lacked an actionable error"
    pass "$label"
  }
  fixture_elf() {
    # ELF64, little endian, version 1, System V, executable; e_machine follows.
    printf '\177ELF\002\001\001\000\000\000\000\000\000\000\000\000\002\000'
    printf '%b' "$1"
    printf '%044d' 0
  }
  fixture_elf '\076\000' >"$helper_tmp/x86_64"
  fixture_elf '\267\000' >"$helper_tmp/aarch64"
  verify_relay_architecture "$helper_tmp/x86_64" x86_64
  verify_relay_architecture "$helper_tmp/aarch64" aarch64
  verify_relay_architecture "$helper_tmp/aarch64" arm64
  pass 'ELF architectures and uname aliases'
  rejects 'x86 binary on ARM host' 'architecture mismatch' verify_relay_architecture "$helper_tmp/x86_64" aarch64
  rejects 'ARM binary on x86 host' 'architecture mismatch' verify_relay_architecture "$helper_tmp/aarch64" x86_64
  cp "$helper_tmp/aarch64" "$helper_tmp/codeg-relay-x86_64-unknown-linux-musl"
  rejects 'renamed wrong-architecture asset' 'architecture mismatch' verify_relay_architecture "$helper_tmp/codeg-relay-x86_64-unknown-linux-musl" x86_64
  rejects 'unsupported host CPU' 'Unsupported.*architecture' verify_relay_architecture "$helper_tmp/x86_64" riscv64
  printf 'not an executable\n' >"$helper_tmp/invalid"
  rejects 'non-ELF download' 'ELF' verify_relay_architecture "$helper_tmp/invalid" x86_64
  head -c 18 "$helper_tmp/x86_64" >"$helper_tmp/short"
  rejects 'truncated ELF header' 'ELF' verify_relay_architecture "$helper_tmp/short" x86_64
  cp "$helper_tmp/x86_64" "$helper_tmp/invalid"
  printf '\001' | dd of="$helper_tmp/invalid" bs=1 seek=4 conv=notrunc 2>/dev/null
  rejects '32-bit ELF' 'ELF' verify_relay_architecture "$helper_tmp/invalid" x86_64
  cp "$helper_tmp/x86_64" "$helper_tmp/invalid"
  printf '\002' | dd of="$helper_tmp/invalid" bs=1 seek=5 conv=notrunc 2>/dev/null
  rejects 'big-endian ELF' 'ELF' verify_relay_architecture "$helper_tmp/invalid" x86_64

  # A checksum for some other file must never let this asset pass verification.
  asset="$helper_tmp/download with spaces"
  printf abc >"$asset"
  if command -v sha256sum >/dev/null; then digest=$(printf abc | sha256sum); else digest=$(printf abc | shasum -a 256); fi
  digest=${digest%% *}
  for suffix in '  downloaded-asset' ' *downloaded-asset' ''; do
    printf '%s%s\n' "$digest" "$suffix" >"$asset.sha256"
    verify_relay_checksum "$asset" >/dev/null
  done
  printf '%s  downloaded-asset\r\n' "$digest" >"$asset.sha256"
  verify_relay_checksum "$asset" >/dev/null
  pass 'checksum text, binary, digest-only, CRLF, and renamed paths'
  printf altered >"$asset"
  rejects 'checksum mismatch' 'SHA-256 mismatch' verify_relay_checksum "$asset"
  printf malformed >"$asset.sha256"
  rejects 'malformed checksum' 'Invalid.*sha256' verify_relay_checksum "$asset"
  printf '%s\n%s\n' "$digest" "$digest" >"$asset.sha256"
  rejects 'multiple checksum entries' 'Invalid.*sha256' verify_relay_checksum "$asset"
  rm "$asset.sha256"
  mkdir "$asset.sha256"
  rejects 'non-file checksum sibling' 'checksum.*regular file' verify_relay_checksum "$asset"
  rmdir "$asset.sha256"
  verify_relay_checksum "$asset" >"$helper_tmp/unsigned" 2>&1
  grep -q 'No sibling .sha256' "$helper_tmp/unsigned"
  pass 'optional checksum absence is visible'

  for number in 1 443 18443 65535; do validate_public_port "$number"; done
  pass 'public NAT port accepts privileged and unprivileged ranges'
  for number in 0 65536 080 -1 nope ''; do
    rejects "invalid public port $number" 'public-port' validate_public_port "$number"
  done
  for signature in 'status=226/NAMESPACE' 'Failed to set up mount namespacing' 'status=218/CAPABILITIES' 'status=228/SECCOMP' 'Failed to apply seccomp filter' 'status=232/ADDRESS_FAMILIES'; do
    is_sandbox_failure "$signature" || fail "missed sandbox signature: $signature"
  done
  if is_sandbox_failure 'address already in use'; then fail 'unrelated failure matched sandbox'; fi
  pass 'namespace/capability/seccomp failures, excluding unrelated errors'

  print_home_agent_values 'relay.example.com,203.0.113.10' 24443 '<relay-sha256>' /etc/codeg-relay/agent.key true >"$helper_tmp/values"
  grep -Fxq 'Relay URL: https://relay.example.com:24443' "$helper_tmp/values"
  grep -Fxq 'Relay certificate SHA-256: <relay-sha256>' "$helper_tmp/values"
  grep -Fxq 'Agent-key file (on VPS): /etc/codeg-relay/agent.key' "$helper_tmp/values"
  grep -Fq 'curl http://relay.example.com:24443/__codeg_relay/v1/health' "$helper_tmp/values"
  grep -Fq 'scp' "$helper_tmp/values"
  pass 'NAT values and secure-copy hint'
  print_home_agent_values '2001:db8::10' 24443 '<relay-sha256>' /etc/codeg-relay/agent.key true >"$helper_tmp/ipv6"
  grep -Fq 'https://[2001:db8::10]:24443' "$helper_tmp/ipv6"
  grep -Fq 'curl http://[2001:db8::10]:24443/__codeg_relay/v1/health' "$helper_tmp/ipv6"
  grep -Fq "'root@[2001:db8::10]:/etc/codeg-relay/agent.key'" "$helper_tmp/ipv6"
  pass 'IPv6 URL and scp formatting'

  dropin_dir="$helper_tmp/unit.d"
  mkdir "$dropin_dir"
  cp "$deploy/no-sandbox.conf" "$dropin_dir/no-sandbox.conf"
  printf '[Service]\nEnvironment=EXTRA=1\n' >"$dropin_dir/custom.conf"
  remove_sandbox_dropin "$dropin_dir"
  [[ ! -e $dropin_dir/no-sandbox.conf && -f $dropin_dir/custom.conf ]]
  pass 'managed drop-in removal preserves other overrides'
  printf '[Service]\nPrivateTmp=no\n' >"$dropin_dir/no-sandbox.conf"
  rejects 'unmanaged drop-in' 'Unmanaged.*drop-in' remove_sandbox_dropin "$dropin_dir"
  [[ -f $dropin_dir/no-sandbox.conf ]]
  rm "$dropin_dir/no-sandbox.conf"
  ln -s "$dropin_dir/custom.conf" "$dropin_dir/no-sandbox.conf"
  rejects 'symlink drop-in' 'symlink' remove_sandbox_dropin "$dropin_dir"
  rm "$dropin_dir/no-sandbox.conf" "$dropin_dir/custom.conf"
  ln -s "$dropin_dir" "$helper_tmp/linked.d"
  rejects 'symlink drop-in directory' 'symlink' remove_sandbox_dropin "$helper_tmp/linked.d"
  [[ -d $dropin_dir ]]
  remove_sandbox_dropin "$dropin_dir"
  [[ ! -e $dropin_dir ]]
  remove_sandbox_dropin "$dropin_dir"
  pass 'empty drop-in directory cleanup is idempotent'
  printf 'Helper tests: %s passed\n' "$helper_count"
  exit 0
fi
(($# == 0)) || { echo 'Usage: deploy-smoke.sh [--helpers-only]' >&2; exit 1; }
# Run ONLY in a disposable container: real user/filesystem operations, stubbed
# systemd/UFW/ss. No host ports or host system paths are mounted for writing.
[[ -f /.dockerenv && $EUID == 0 ]] || { echo 'Requires a disposable root Docker container' >&2; exit 1; }
mock=$(mktemp -d)
export C6_DEPLOY_MOCK=$mock
mkdir "$mock/bin"
mkdir -p /etc/systemd/system
export PATH="$mock/bin:$PATH"
touch "$mock/ufw-rules" "$mock/ufw-log"

cat >"$mock/bin/systemctl" <<'STUB'
#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
case "$1" in
  cat) test -f /etc/systemd/system/codeg-relay.service;;
  is-active) test -f "$C6_DEPLOY_MOCK/active";;
  enable|restart)
    if [[ -f $C6_DEPLOY_MOCK/fail-start && $(cat "$C6_DEPLOY_MOCK/fail-start") == "$1" ]]; then
      rm -f "$C6_DEPLOY_MOCK/active"
      exit 1
    fi
    if [[ -f $C6_DEPLOY_MOCK/fail-late ]]; then
      rm -f "$C6_DEPLOY_MOCK/active"
      exit 0
    fi
    touch "$C6_DEPLOY_MOCK/active"
    mkdir -p /var/lib/codeg-relay/tls
    echo fixture-cert >/var/lib/codeg-relay/tls/cert.pem;;
  disable|stop) rm -f "$C6_DEPLOY_MOCK/active";;
  daemon-reload) :;;
  *) echo "Unexpected systemctl operation: $*" >&2; exit 1;;
esac
STUB
cat >"$mock/bin/journalctl" <<'STUB'
#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
[[ $1 == -u && $2 == codeg-relay ]]
cat "$C6_DEPLOY_MOCK/journal"
STUB
cat >"$mock/bin/ufw" <<'STUB'
#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
printf '%s\n' "$*" >>"$C6_DEPLOY_MOCK/ufw-log"
case "$1" in
  show) cat "$C6_DEPLOY_MOCK/ufw-rules";;
  allow)
    [[ $2 == 18443/tcp && $3 == comment && $4 == codeg-relay ]]
    echo "ufw allow $2 comment 'codeg-relay'" >>"$C6_DEPLOY_MOCK/ufw-rules"
    echo 'Fixture intentionally uses a different success message';;
  delete)
    [[ $2 == allow && $3 == 18443/tcp ]]
    sed "/^ufw allow 18443\/tcp comment 'codeg-relay'$/d" "$C6_DEPLOY_MOCK/ufw-rules" >"$C6_DEPLOY_MOCK/new-rules"
    mv "$C6_DEPLOY_MOCK/new-rules" "$C6_DEPLOY_MOCK/ufw-rules";;
  *) exit 1;;
esac
STUB
cat >"$mock/bin/ss" <<'STUB'
#!/bin/sh
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
exit 0
STUB
# Use a native ELF fixture so install preflight inspects actual ELF bytes. The
# container needs a C compiler; no packages are installed by this test.
command -v cc >/dev/null || { echo 'Smoke container needs cc' >&2; exit 1; }
cat >"$mock/relay.c" <<'STUB'
#include <stdio.h>
#include <string.h>
int main(int argc, char **argv) {
  if (argc < 2) return 1;
  if (!strcmp(argv[1], "--version")) puts("codeg-relay fixture");
  else if (!strcmp(argv[1], "gen-key")) puts("fixture-key-not-a-real-secret-1234567890");
  else if (!strcmp(argv[1], "fingerprint")) {
    for (int i = 0; i < 64; ++i) putchar('0');
    puts("\nSHA256: fixture-colon-form");
  } else return 1;
  return 0;
}
STUB
cc "$mock/relay.c" -o "$mock/relay"
chmod 0644 "$mock/relay"
sha256sum "$mock/relay" >"$mock/relay.sha256"
chmod +x "$mock"/bin/*
install_relay() { bash "$deploy/install.sh" --binary "$mock/relay" --port 18443 --public-host 127.0.0.1 "$@"; }
uninstall_relay() { bash "$deploy/uninstall.sh"; }

# First-run suppression must apply to stdout AND stderr, and public-port must
# affect advertised addresses without changing the internal listener.
if ! install_relay --no-sandbox --no-print-key --public-port 24443 >"$mock/private" 2>&1; then
  sed -n '/^Error:/p' "$mock/private" >&2
  exit 1
fi
if grep -Fq 'fixture-key-not-a-real-secret-1234567890' "$mock/private"; then exit 1; fi
grep -Fq 'Agent-key file (on VPS): /etc/codeg-relay/agent.key' "$mock/private"
grep -Fq 'https://127.0.0.1:24443' "$mock/private"
grep -Fq 'curl http://127.0.0.1:24443/__codeg_relay/v1/health' "$mock/private"
grep -Fxq 'CODEG_RELAY_PORT=18443' /etc/codeg-relay/relay.env
cmp "$deploy/no-sandbox.conf" /etc/systemd/system/codeg-relay.service.d/no-sandbox.conf
printf '[Service]\nEnvironment=EXTRA=1\n' >/etc/systemd/system/codeg-relay.service.d/custom.conf
first_key=$(sha256sum /etc/codeg-relay/agent.key)
install_relay --no-sandbox --no-print-key >"$mock/private-again" 2>&1
[[ $(sha256sum /etc/codeg-relay/agent.key) == "$first_key" ]]
grep -Fq 'https://127.0.0.1:18443' "$mock/private-again"
install_relay --no-print-key >"$mock/hardened" 2>&1
[[ ! -e /etc/systemd/system/codeg-relay.service.d/no-sandbox.conf ]]
[[ -f /etc/systemd/system/codeg-relay.service.d/custom.conf ]]
install_relay --no-sandbox --no-print-key >"$mock/private-uninstall" 2>&1
uninstall_relay
[[ ! -e /etc/systemd/system/codeg-relay.service.d/no-sandbox.conf ]]
[[ -f /etc/systemd/system/codeg-relay.service.d/custom.conf ]]
rm /etc/systemd/system/codeg-relay.service.d/custom.conf
rmdir /etc/systemd/system/codeg-relay.service.d
echo 'PASS: no-print-key, NAT port, no-sandbox repeat/reset/uninstall'

# No firewall flag means no UFW invocation, even on uninstall. Repeat installation
# preserves identity and repairs key permissions without printing the key again.
install_relay >"$mock/first"
first_key=$(sha256sum /etc/codeg-relay/agent.key)
chmod 0644 /etc/codeg-relay/agent.key
chown root:root /etc/codeg-relay/agent.key
install_relay >"$mock/second"
[[ $(sha256sum /etc/codeg-relay/agent.key) == "$first_key" ]]
[[ $(stat -c '%a:%U' /etc/codeg-relay/agent.key) == 600:codeg-relay ]]
grep -q 'New agent key' "$mock/first"
grep -Fq 'fixture-key-not-a-real-secret-1234567890' "$mock/first"
if grep -q 'New agent key' "$mock/second"; then exit 1; fi
mv /var/lib/codeg-relay /var/lib/codeg-relay.saved
mkdir "$mock/outside"
ln -s "$mock/outside" /var/lib/codeg-relay
if install_relay >"$mock/symlink" 2>&1; then echo 'Symlink reinstall was accepted' >&2; exit 1; fi
[[ -z $(ls -A "$mock/outside") ]]
rm /var/lib/codeg-relay
mv /var/lib/codeg-relay.saved /var/lib/codeg-relay
# Partial uninstall (missing unit) must still remove owned state and account.
rm /etc/systemd/system/codeg-relay.service
uninstall_relay
uninstall_relay
[[ ! -s $mock/ufw-log && ! -e /var/lib/codeg-relay ]]
if getent passwd codeg-relay >/dev/null; then exit 1; fi
echo 'PASS: no-firewall, repeat install, key permissions, symlink, partial uninstall'

echo "ufw allow 18443/tcp comment 'pre-existing'" >"$mock/ufw-rules"
install_relay --open-firewall >"$mock/preexisting"
[[ ! -e /etc/codeg-relay/ufw-port ]]
uninstall_relay
grep -q 'pre-existing' "$mock/ufw-rules"
if grep -q '^delete ' "$mock/ufw-log"; then exit 1; fi
echo 'PASS: pre-existing TCP rule preserved'

printf '%s\n' 'ufw allow 22/tcp' 'ufw allow 18443/udp' >"$mock/ufw-rules"
install_relay --open-firewall --public-port 24443 --no-print-key >"$mock/added"
[[ $(cat /etc/codeg-relay/ufw-port) == 18443 ]]
grep -Fq 'https://127.0.0.1:24443' "$mock/added"
install_relay --open-firewall >"$mock/added-again"
[[ $(grep -c "codeg-relay'" "$mock/ufw-rules") == 1 ]]
uninstall_relay
[[ $(cat "$mock/ufw-rules") == $'ufw allow 22/tcp\nufw allow 18443/udp' ]]
echo 'PASS: UDP does not hide TCP; owned TCP rollback preserves other rules'

# Reject damaged/incorrect downloads before accounts or managed state exist.
cp "$mock/relay" "$mock/bad-relay"
cp "$mock/relay.sha256" "$mock/bad-relay.sha256"
printf corrupted >>"$mock/bad-relay"
if install_relay --binary "$mock/bad-relay" --no-print-key >"$mock/checksum-failure" 2>&1; then exit 1; fi
grep -q 'SHA-256 mismatch' "$mock/checksum-failure"
[[ ! -e /etc/codeg-relay && ! -e /opt/codeg-relay ]]
cp "$mock/relay" "$mock/bad-relay"
case "$(uname -m)" in
  aarch64|arm64) printf '\076\000';;
  x86_64|amd64) printf '\267\000';;
  *) exit 1;;
esac | dd of="$mock/bad-relay" bs=1 seek=18 conv=notrunc 2>/dev/null
sha256sum "$mock/bad-relay" >"$mock/bad-relay.sha256"
if install_relay --binary "$mock/bad-relay" --no-print-key >"$mock/arch-failure" 2>&1; then exit 1; fi
grep -q 'CPU architecture mismatch' "$mock/arch-failure"
[[ ! -e /etc/codeg-relay && ! -e /opt/codeg-relay ]]
if getent passwd codeg-relay >/dev/null; then exit 1; fi
echo 'PASS: checksum and CPU preflight reject before installation'

for number in 0 65536 ''; do
  if install_relay --public-port "$number" >"$mock/bad-port" 2>&1; then exit 1; fi
  grep -q 'public-port' "$mock/bad-port"
done
[[ ! -e /etc/codeg-relay && ! -e /opt/codeg-relay ]]
echo 'PASS: invalid public ports reject before installation'

# Recognize immediate, restart, and delayed failures without printing journals
# or silently creating a no-sandbox override.
for signature in 'status=226/NAMESPACE' 'Failed to set up mount namespacing' 'status=218/CAPABILITIES' 'Failed to apply seccomp filter'; do
  printf '%s\n' "$signature" >"$mock/journal"
  printf enable >"$mock/fail-start"
  if install_relay --no-print-key >"$mock/start-failure" 2>&1; then exit 1; fi
  grep -q 'Re-run.*--no-sandbox' "$mock/start-failure"
  [[ ! -e /etc/systemd/system/codeg-relay.service.d/no-sandbox.conf ]]
  if grep -Fq 'fixture-key-not-a-real-secret-1234567890' "$mock/start-failure"; then exit 1; fi
  rm "$mock/fail-start"
  uninstall_relay
done
install_relay --no-print-key >"$mock/before-restart-failure" 2>&1
printf restart >"$mock/fail-start"
if install_relay --no-print-key >"$mock/restart-failure" 2>&1; then exit 1; fi
grep -q 'Re-run.*--no-sandbox' "$mock/restart-failure"
rm "$mock/fail-start"
uninstall_relay
touch "$mock/fail-late"
if install_relay --no-print-key >"$mock/late-failure" 2>&1; then exit 1; fi
grep -q 'Re-run.*--no-sandbox' "$mock/late-failure"
rm "$mock/fail-late"
uninstall_relay
printf 'address already in use\n' >"$mock/journal"
printf enable >"$mock/fail-start"
if install_relay --no-print-key >"$mock/unrelated-failure" 2>&1; then exit 1; fi
if grep -q 'Re-run.*--no-sandbox' "$mock/unrelated-failure"; then exit 1; fi
rm "$mock/fail-start"
uninstall_relay
echo 'PASS: immediate/restart/late sandbox hints and unrelated failures'
