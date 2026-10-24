#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
umask 077

die() { echo "Error: $*" >&2; exit 1; }
usage() {
  cat <<USAGE
Usage: $0 --binary PATH --public-host IP[,DNS] [options]
  --binary PATH       Linux x86_64/aarch64 ELF asset; verify PATH.sha256 if present
  --port N            Internal listening port, 1024–65535 (default: 18443)
  --public-port N     Public TCP port for NAT, 1–65535 (default: --port)
  --no-sandbox        Disable namespace/seccomp sandboxing for container VPSes
  --no-print-key      Print the agent-key file path, never the generated key
  --open-firewall     Explicitly manage the listening port's UFW rule
USAGE
}
binary='' port=18443 public_port='' public_host='' open_firewall=false no_sandbox=false no_print_key=false
while (($#)); do
  case "$1" in
    --binary|--port|--public-port|--public-host)
      (($# >= 2)) || die "Missing value for $1"
      case "$1" in
        --binary) binary=$2;;
        --port) port=$2;;
        --public-port) [[ -n $2 ]] || die 'Missing value for --public-port'; public_port=$2;;
        --public-host) public_host=$2;;
      esac
      shift 2;;
    --open-firewall) open_firewall=true; shift;;
    --no-sandbox) no_sandbox=true; shift;;
    --no-print-key) no_print_key=true; shift;;
    -h|--help) usage; exit 0;;
    *) usage >&2; die "Unknown argument: $1";;
  esac
done
[[ $(uname -s) == Linux ]] || die 'Run on Linux with systemd'
[[ $EUID == 0 ]] || die 'Run as root'
[[ -f $binary && -r $binary ]] || die '--binary must name a readable regular file'
if [[ ! $port =~ ^[1-9][0-9]{3,4}$ ]] || ((port < 1024 || port > 65535)); then die 'Use an unprivileged port (1024–65535)'; fi
[[ $port != 3306 && $port != 6379 ]] || die 'Refusing a shared database port'
[[ $public_host =~ ^[a-zA-Z0-9.:_-]+(,[a-zA-Z0-9.:_-]+)*$ ]] || die 'Invalid public host list'
for command in systemctl install useradd userdel getent flock ss; do command -v "$command" >/dev/null || die "Missing required command: $command"; done
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
[[ -f $script_dir/codeg-relay.service ]] || die 'Missing adjacent service unit'
[[ -f $script_dir/no-sandbox.conf ]] || die 'Missing adjacent no-sandbox.conf'
# shellcheck source=install-common.sh
source "$script_dir/install-common.sh"
public_port=${public_port:-$port}
validate_public_port "$public_port"
verify_relay_checksum "$binary"
verify_relay_architecture "$binary" "$(uname -m)"
exec 9>/run/codeg-relay-install.lock
flock -n 9 || die 'Another install/uninstall is running'

marker=/etc/codeg-relay/installed-by-codeg-relay
firewall_marker=/etc/codeg-relay/ufw-port
dropin_dir=/etc/systemd/system/codeg-relay.service.d
new_install=false
if [[ ! -f $marker ]]; then
  for path in /opt/codeg-relay /etc/codeg-relay /var/lib/codeg-relay /etc/systemd/system/codeg-relay.service "$dropin_dir"; do
    [[ ! -e $path && ! -L $path ]] || die "Unmanaged path exists: $path"
  done
  ! getent passwd codeg-relay >/dev/null || die 'Unmanaged codeg-relay user already exists'
  ! getent group codeg-relay >/dev/null || die 'Unmanaged codeg-relay group already exists'
  ! systemctl cat codeg-relay.service >/dev/null 2>&1 || die 'Unmanaged relay unit already exists'
  [[ -z $(ss -H -ltn "sport = :$port") ]] || die "Port $port is already in use"
  new_install=true
else
  [[ ! -L $marker && $(cat "$marker") == codeg-relay-v1 ]] || die 'Invalid installation marker'
  for path in /etc/codeg-relay /opt/codeg-relay /opt/codeg-relay/bin /var/lib/codeg-relay /etc/systemd/system/codeg-relay.service /etc/codeg-relay/relay.env /etc/codeg-relay/agent.key /etc/codeg-relay/ufw-port; do
    [[ ! -L $path ]] || die "Refusing symlink: $path"
  done
  getent passwd codeg-relay >/dev/null || die 'Managed user missing; restore it or uninstall first'
  if [[ -f $firewall_marker && $(cat "$firewall_marker") != "$port" ]]; then
    die 'Changing a managed firewall port requires uninstall first'
  fi
fi
check_sandbox_dropin "$dropin_dir"

# UFW is neither inspected nor changed unless explicitly requested.
add_firewall=false
if $open_firewall; then
  command -v ufw >/dev/null || die 'ufw is not installed; install it separately or omit --open-firewall'
  rules=$(LC_ALL=C ufw show added)
  if printf '%s\n' "$rules" | grep -Eq "^ufw allow ${port}(/tcp)?( comment .*)?$"; then
    printf 'Existing UFW rule for %s retained; no new rule claimed.\n' "$port"
  elif printf '%s\n' "$rules" | grep -Ev "^ufw (allow|deny|reject|limit) ${port}/udp( comment .*)?$" | grep -Eq "(^|[^0-9])${port}(/tcp|[^0-9]|$)"; then
    die 'Conflicting or restricted UFW rule exists; resolve it manually before opening the port'
  else
    add_firewall=true
  fi
fi

if $new_install; then
  useradd --system --user-group --home-dir /var/lib/codeg-relay --shell /usr/sbin/nologin --no-create-home codeg-relay
  install -d -o root -g codeg-relay -m 0750 /etc/codeg-relay
  printf '%s\n' codeg-relay-v1 >"$marker"
fi
install -d -o root -g codeg-relay -m 0750 /etc/codeg-relay
install -d -o root -g root -m 0755 /opt/codeg-relay /opt/codeg-relay/bin
install -d -o codeg-relay -g codeg-relay -m 0700 /var/lib/codeg-relay
install -o root -g root -m 0755 "$binary" /opt/codeg-relay/bin/codeg-relay.new
mv -f /opt/codeg-relay/bin/codeg-relay.new /opt/codeg-relay/bin/codeg-relay
relay=/opt/codeg-relay/bin/codeg-relay
"$relay" --version
if [[ ! -e /etc/codeg-relay/agent.key ]]; then
  "$relay" gen-key >/etc/codeg-relay/agent.key
  chown codeg-relay:codeg-relay /etc/codeg-relay/agent.key
  chmod 0600 /etc/codeg-relay/agent.key
  if ! $no_print_key; then
    printf 'New agent key (shown only on creation): '
    cat /etc/codeg-relay/agent.key
  fi
fi
[[ -f /etc/codeg-relay/agent.key && ! -L /etc/codeg-relay/agent.key ]] || die 'Invalid key file'
chown codeg-relay:codeg-relay /etc/codeg-relay/agent.key
chmod 0600 /etc/codeg-relay/agent.key
printf 'CODEG_RELAY_PORT=%s\nCODEG_RELAY_PUBLIC_HOST=%s\n' "$port" "$public_host" >/etc/codeg-relay/relay.env.new
chmod 0600 /etc/codeg-relay/relay.env.new
mv -f /etc/codeg-relay/relay.env.new /etc/codeg-relay/relay.env
install -o root -g root -m 0644 "$script_dir/codeg-relay.service" /etc/systemd/system/codeg-relay.service
if $no_sandbox; then
  install -d -o root -g root -m 0755 "$dropin_dir"
  install -o root -g root -m 0644 "$script_dir/no-sandbox.conf" "$dropin_dir/no-sandbox.conf"
else
  remove_sandbox_dropin "$dropin_dir"
fi
systemctl daemon-reload
start_time=$(date '+%Y-%m-%d %H:%M:%S')
relay_start_failed() {
  local journal=''
  if command -v journalctl >/dev/null; then
    journal=$(journalctl -u codeg-relay --since "$start_time" -n 80 --no-pager 2>/dev/null || true)
  fi
  if ! $no_sandbox && is_sandbox_failure "$journal"; then
    printf 'Hint: This VPS may restrict namespaces/seccomp. Re-run the same installer command with --no-sandbox; this disables namespace/seccomp sandboxing.\n' >&2
  fi
  die 'Relay did not start; inspect journalctl -u codeg-relay --no-pager'
}
was_active=false
if systemctl is-active --quiet codeg-relay; then was_active=true; fi
systemctl enable --now codeg-relay || relay_start_failed
if $was_active; then systemctl restart codeg-relay || relay_start_failed; fi

if $add_firewall; then
  # Inspect resulting rules even if ufw reports a partial failure (e.g. IPv6).
  ufw_result=0
  LC_ALL=C ufw allow "$port/tcp" comment 'codeg-relay' || ufw_result=$?
  rules_after=$(LC_ALL=C ufw show added)
  if printf '%s\n' "$rules_after" | grep -Eq "^ufw allow ${port}/tcp comment '?codeg-relay'?$"; then
    printf '%s\n' "$port" >"$firewall_marker"
    chmod 0600 "$firewall_marker"
  else
    die 'UFW rule not present; no rollback marker created'
  fi
  ((ufw_result == 0)) || die 'UFW partially failed; owned rule recorded for rollback'
fi

for _ in {1..50}; do
  sleep 0.1
  systemctl is-active --quiet codeg-relay || relay_start_failed
  [[ -s /var/lib/codeg-relay/tls/cert.pem ]] && break
done
systemctl is-active --quiet codeg-relay || relay_start_failed
fingerprint=$("$relay" fingerprint --data-dir /var/lib/codeg-relay)
fingerprint=${fingerprint%%$'\n'*}
[[ $fingerprint =~ ^[[:xdigit:]]{64}$ ]] || die 'Invalid relay certificate SHA-256 fingerprint'
printf 'Pair iOS with the home codeg web token and home certificate fingerprint.\n'
printf 'Keep TLS state for future upgrades. Changing public hosts requires a deliberate certificate rotation and re-pairing.\n'
print_home_agent_values "$public_host" "$public_port" "$fingerprint" /etc/codeg-relay/agent.key "$no_print_key"
