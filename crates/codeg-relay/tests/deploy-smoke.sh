#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
# Run ONLY in a disposable container: real user/filesystem operations, stubbed
# systemd/UFW/ss. No host ports or host system paths are mounted for writing.
[[ -f /.dockerenv && $EUID == 0 ]] || { echo 'Requires a disposable root Docker container' >&2; exit 1; }
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
deploy=$script_dir/../deploy
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
    touch "$C6_DEPLOY_MOCK/active"
    mkdir -p /var/lib/codeg-relay/tls
    echo fixture-cert >/var/lib/codeg-relay/tls/cert.pem;;
  disable|stop) rm -f "$C6_DEPLOY_MOCK/active";;
  daemon-reload) :;;
  *) echo "Unexpected systemctl operation: $*" >&2; exit 1;;
esac
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
cat >"$mock/relay" <<'STUB'
#!/bin/sh
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
case "$1" in
  --version) echo 'codeg-relay fixture';;
  gen-key) echo 'fixture-key-not-a-real-secret-1234567890';;
  fingerprint) echo 'fixture-fingerprint';;
  *) exit 1;;
esac
STUB
chmod +x "$mock"/bin/* "$mock/relay"
install_relay() { bash "$deploy/install.sh" --binary "$mock/relay" --port 18443 --public-host 127.0.0.1 "$@"; }
uninstall_relay() { bash "$deploy/uninstall.sh"; }

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
install_relay --open-firewall >"$mock/added"
[[ $(cat /etc/codeg-relay/ufw-port) == 18443 ]]
install_relay --open-firewall >"$mock/added-again"
[[ $(grep -c "codeg-relay'" "$mock/ufw-rules") == 1 ]]
uninstall_relay
[[ $(cat "$mock/ufw-rules") == $'ufw allow 22/tcp\nufw allow 18443/udp' ]]
echo 'PASS: UDP does not hide TCP; owned TCP rollback preserves other rules'
