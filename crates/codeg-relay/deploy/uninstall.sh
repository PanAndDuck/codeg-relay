#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
set -euo pipefail
umask 077
die() { echo "Error: $*" >&2; exit 1; }
[[ $EUID == 0 ]] || die 'Run as root'
(($# == 0)) || die 'Usage: uninstall.sh (removes relay keys and state)'
exec 9>/run/codeg-relay-install.lock
flock -n 9 || die 'Another install/uninstall is running'
marker=/etc/codeg-relay/installed-by-codeg-relay
if [[ ! -e $marker ]]; then
  echo 'No managed installation; nothing removed.'
  exit 0
fi
[[ -f $marker && ! -L $marker && $(cat "$marker") == codeg-relay-v1 ]] || die 'Invalid installation marker'
for path in /etc/codeg-relay /opt/codeg-relay /var/lib/codeg-relay; do [[ ! -L $path ]] || die "Refusing symlink: $path"; done
if systemctl cat codeg-relay.service >/dev/null 2>&1; then
  systemctl disable --now codeg-relay
elif systemctl is-active --quiet codeg-relay; then
  systemctl stop codeg-relay
fi
rm -f /etc/systemd/system/codeg-relay.service
systemctl daemon-reload
if [[ -f /etc/codeg-relay/ufw-port ]]; then
  port=$(cat /etc/codeg-relay/ufw-port)
  if [[ ! $port =~ ^[1-9][0-9]{3,4}$ ]] || ((port < 1024 || port > 65535)); then die 'Invalid UFW marker port'; fi
  command -v ufw >/dev/null || die 'ufw missing; retain marker until firewall rollback can be checked'
  rules=$(LC_ALL=C ufw show added)
  if printf '%s\n' "$rules" | grep -Eq "^ufw allow ${port}/tcp comment '?codeg-relay'?$"; then
    ufw delete allow "$port/tcp"
  elif printf '%s\n' "$rules" | grep -Eq "(^|[^0-9])${port}(/tcp|[^0-9]|$)"; then
    die 'Marked rule was changed externally; retain state and resolve it manually'
  fi
fi
if getent passwd codeg-relay >/dev/null; then userdel codeg-relay; fi
if getent group codeg-relay >/dev/null; then groupdel codeg-relay; fi
rm -rf -- /opt/codeg-relay /etc/codeg-relay /var/lib/codeg-relay
echo 'Removed managed relay installation, keys, certificate and owned firewall rule.'
