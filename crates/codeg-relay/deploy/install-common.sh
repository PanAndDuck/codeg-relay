#!/usr/bin/env bash
unset CODEG_DATA_DIR CODEG_HOME CODEG_STATIC_DIR CODEG_ACP_TMP_ROOT CODEG_TOKEN CODEG_PORT CODEG_HOST
# Sourced helpers: no installation or service operations at load time.

relay_install_error() { printf 'Error: %s\n' "$*" >&2; return 1; }

validate_public_port() {
  if [[ ! $1 =~ ^[1-9][0-9]{0,4}$ ]] || (( $1 > 65535 )); then
    relay_install_error '--public-port must be a TCP port (1–65535)'
    return 1
  fi
}

verify_relay_checksum() {
  local binary=$1 checksum=$1.sha256 contents expected actual
  if [[ ! -e $checksum && ! -L $checksum ]]; then
    printf 'Warning: No sibling .sha256; checksum verification skipped.\n' >&2
    return 0
  fi
  [[ -f $checksum && -r $checksum ]] || { relay_install_error 'The checksum must be a readable regular file'; return 1; }
  contents=$(cat -- "$checksum") || return 1
  contents=${contents%$'\r'}
  # Accept a single sha256sum/shasum entry or a digest alone. Hash the supplied
  # asset, never a filename from the checksum (downloads may have been renamed).
  if [[ $contents == *$'\n'* || ! $contents =~ ^[[:xdigit:]]{64}([[:blank:]]+.+)?$ ]]; then
    relay_install_error "Invalid .sha256 file: $checksum (expected one SHA-256 entry)"
    return 1
  fi
  expected=$(printf '%s' "${contents:0:64}" | tr '[:upper:]' '[:lower:]')
  if command -v sha256sum >/dev/null; then
    actual=$(sha256sum <"$binary") || return 1
  elif command -v shasum >/dev/null; then
    actual=$(shasum -a 256 <"$binary") || return 1
  else
    relay_install_error 'Missing sha256sum (or shasum) to verify the release asset'
    return 1
  fi
  [[ ${actual%% *} == "$expected" ]] || { relay_install_error "SHA-256 mismatch for $binary; download the asset and its .sha256 again"; return 1; }
  printf 'Verified SHA-256: %s\n' "$binary"
}

verify_relay_architecture() {
  local binary=$1 machine=$2 host_arch binary_arch header
  case "$machine" in
    x86_64|amd64) host_arch=x86_64;;
    aarch64|arm64) host_arch=aarch64;;
    *) relay_install_error "Unsupported CPU architecture: $machine (use x86_64 or aarch64 Linux)"; return 1;;
  esac
  # Inspect bytes rather than the asset name, without executing the download.
  header=$(LC_ALL=C od -An -v -tx1 -N64 "$binary" | tr -d '[:space:]') || return 1
  if [[ ${#header} != 128 || $header != 7f454c46020101* ||
        ( ${header:14:2} != 00 && ${header:14:2} != 03 ) ||
        ( ${header:32:4} != 0200 && ${header:32:4} != 0300 ) ]]; then
    relay_install_error '--binary must be a 64-bit little-endian Linux ELF executable (extract archives first)'
    return 1
  fi
  case "${header:36:4}" in
    3e00) binary_arch=x86_64;;
    b700) binary_arch=aarch64;;
    *) relay_install_error 'Unsupported ELF CPU architecture (use x86_64 or aarch64 Linux)'; return 1;;
  esac
  [[ $binary_arch == "$host_arch" ]] || {
    relay_install_error "CPU architecture mismatch: binary is $binary_arch, host is $machine; use codeg-relay-$host_arch-unknown-linux-musl"
    return 1
  }
}

check_sandbox_dropin() {
  local directory=$1 dropin=$1/no-sandbox.conf first_line
  [[ ! -L $directory && ! -L $dropin ]] || { relay_install_error "Refusing symlink in drop-in path: $dropin"; return 1; }
  [[ ! -e $directory || -d $directory ]] || { relay_install_error "Invalid drop-in directory: $directory"; return 1; }
  if [[ -e $dropin ]]; then
    [[ -f $dropin ]] || { relay_install_error "Invalid drop-in file: $dropin"; return 1; }
    IFS= read -r first_line <"$dropin" || true
    [[ $first_line == '# Managed by codeg-relay installer: no-sandbox-v1' ]] || {
      relay_install_error "Unmanaged no-sandbox drop-in exists: $dropin"
      return 1
    }
  fi
}

remove_sandbox_dropin() {
  local directory=$1
  check_sandbox_dropin "$directory" || return 1
  rm -f -- "$directory/no-sandbox.conf" || return 1
  # Other administrator-owned overrides are deliberately retained.
  if [[ -d $directory ]]; then rmdir -- "$directory" 2>/dev/null || true; fi
}

is_sandbox_failure() {
  printf '%s\n' "$1" | grep -Eiq '226/NAMESPACE|Failed to set up mount namespacing|218/CAPABILITIES|seccomp|232/ADDRESS_FAMILIES|Failed to restrict address families|Failed to (set up|apply).*system call filter|step (NAMESPACE|CAPABILITIES)'
}

print_home_agent_values() {
  local host=${1%%,*} port=$2 fingerprint=$3 key_file=$4 no_print_key=$5
  if [[ $host == *:* ]]; then host="[$host]"; fi
  printf '\nValues for the home agent:\n'
  printf 'Relay URL: https://%s:%s\n' "$host" "$port"
  printf 'Relay certificate SHA-256: %s\n' "$fingerprint"
  printf 'Agent-key file (on VPS): %s\n' "$key_file"
  printf '\nPublic health check: curl http://%s:%s/__codeg_relay/v1/health\n' "$host" "$port"
  if [[ $no_print_key == true ]]; then
    printf "Copy securely (adjust SSH host/port if needed): scp 'root@%s:%s' ./agent.key\n" "$host" "$key_file"
  fi
}
