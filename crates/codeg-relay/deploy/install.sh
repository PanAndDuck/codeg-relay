#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
umask 077

die() { echo "Error: $*" >&2; exit 1; }
usage() { echo "Usage: $0 --binary PATH --port 18443 --public-host IP[,DNS] [--open-firewall]"; }
binary='' port=18443 public_host='' open_firewall=false
while (($#)); do
  case "$1" in
    --binary|--port|--public-host)
      (($# >= 2)) || die "Missing value for $1"
      case "$1" in --binary) binary=$2;; --port) port=$2;; --public-host) public_host=$2;; esac
      shift 2;;
    --open-firewall) open_firewall=true; shift;;
    -h|--help) usage; exit 0;;
    *) usage >&2; die "Unknown argument: $1";;
  esac
done
[[ $EUID == 0 ]] || die 'Run as root'
[[ -f $binary && -x $binary ]] || die '--binary must name an executable regular file'
if [[ ! $port =~ ^[1-9][0-9]{3,4}$ ]] || ((port < 1024 || port > 65535)); then die 'Use an unprivileged port (1024–65535)'; fi
[[ $port != 3306 && $port != 6379 ]] || die 'Refusing a shared database port'
[[ $public_host =~ ^[a-zA-Z0-9.:_-]+(,[a-zA-Z0-9.:_-]+)*$ ]] || die 'Invalid public host list'
for command in systemctl install useradd userdel getent flock ss; do command -v "$command" >/dev/null || die "Missing required command: $command"; done
script_dir=$(cd -- "$(dirname -- "${BASH_SOURCE[0]}")" && pwd)
[[ -f $script_dir/codeg-relay.service ]] || die 'Missing adjacent service unit'
exec 9>/run/codeg-relay-install.lock
flock -n 9 || die 'Another install/uninstall is running'

marker=/etc/codeg-relay/installed-by-codeg-relay
firewall_marker=/etc/codeg-relay/ufw-port
new_install=false
if [[ ! -f $marker ]]; then
  for path in /opt/codeg-relay /etc/codeg-relay /var/lib/codeg-relay /etc/systemd/system/codeg-relay.service; do
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
  printf 'New agent key (shown only on creation): '
  cat /etc/codeg-relay/agent.key
fi
[[ -f /etc/codeg-relay/agent.key && ! -L /etc/codeg-relay/agent.key ]] || die 'Invalid key file'
chown codeg-relay:codeg-relay /etc/codeg-relay/agent.key
chmod 0600 /etc/codeg-relay/agent.key
printf 'CODEG_RELAY_PORT=%s\nCODEG_RELAY_PUBLIC_HOST=%s\n' "$port" "$public_host" >/etc/codeg-relay/relay.env.new
chmod 0600 /etc/codeg-relay/relay.env.new
mv -f /etc/codeg-relay/relay.env.new /etc/codeg-relay/relay.env
install -o root -g root -m 0644 "$script_dir/codeg-relay.service" /etc/systemd/system/codeg-relay.service
systemctl daemon-reload
was_active=false
if systemctl is-active --quiet codeg-relay; then was_active=true; fi
systemctl enable --now codeg-relay
if $was_active; then systemctl restart codeg-relay; fi

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
  [[ -s /var/lib/codeg-relay/tls/cert.pem ]] && break
  sleep 0.1
done
systemctl is-active --quiet codeg-relay || die 'Relay did not start; inspect journalctl -u codeg-relay'
printf 'Certificate fingerprint:\n'
"$relay" fingerprint --data-dir /var/lib/codeg-relay
url_host=${public_host%%,*}
if [[ $url_host == *:* ]]; then url_host="[$url_host]"; fi
printf 'Client URL: https://%s:%s\n' "$url_host" "$port"
printf 'Pair home codeg with this URL, the agent key, and fingerprint. Pair iOS with the home codeg web token, NOT the agent key.\n'
printf 'Keep TLS state for future upgrades. Changing public hosts requires a deliberate certificate rotation and re-pairing.\n'
