# codeg-relay deployment kit

See [README.zh-CN.md](README.zh-CN.md) for the full CLI, installation, isolation
checks and rollback guide. This kit serves one active home codeg agent over a
single multiplexed WSS tunnel. Phone TLS passes through the relay and terminates
only at home. The relay is blind to HTTP content and web tokens; it sees only
IP/timing/size metadata and the public `agent_connected` flag. A compromised
relay can disrupt service, but cannot read or alter traffic or impersonate home
to phones that pin its certificate.

Download `codeg-relay-x86_64-unknown-linux-musl` or
`codeg-relay-aarch64-unknown-linux-musl` and its matching `.sha256` from
https://github.com/PanAndDuck/codeg-relay/releases. Keep the checksum next to
the binary and copy this entire `deploy/` directory to the VPS:

```bash
sudo bash deploy/install.sh --binary ./codeg-relay-x86_64-unknown-linux-musl \
  --port 18443 --public-host relay.example.com --no-print-key
```

Replace `relay.example.com` with your server's DNS name or IP. Allow that TCP
port in the provider firewall; optionally pass `--open-firewall` to manage UFW.
The installer verifies a sibling `.sha256` when present and refuses mismatches
or an ELF CPU architecture different from `uname -m`, before changing the
installation. A download need not already be executable. Missing checksums
produce a warning; download the matching checksum for release installs.
The installer needs Linux, root, systemd, useradd, flock, ss, coreutils and
`sha256sum` (or `shasum`), and does not install packages.
Keep the generated agent key and certificate fingerprint private. Configure
Settings → Web service → Remote relay in home codeg, then pair iOS using codeg's
QR. The agent key is different from the web token used by iOS.

For provider NAT, add `--public-port 24443` and arrange public TCP port 24443
to forward to the listening `--port 18443`. `--public-port` defaults to `--port`
and only changes advertised URLs; UFW still manages the listening port. The
final **Values for the home agent** block supplies the public HTTPS URL,
relay certificate SHA-256 and `/etc/codeg-relay/agent.key` path, followed by
the public plaintext health-check command. `--no-print-key` suppresses the
key even on first creation and prints an `scp` hint to copy it straight to a
local file (adjust the SSH host/port for your provider). Without the option,
the key is still printed only when first generated.

For a container VPS that cannot provide mount namespaces or seccomp, add
`--no-sandbox`. This installs a managed
`/etc/systemd/system/codeg-relay.service.d/no-sandbox.conf` override. It resets
the unit's namespace/seccomp directives, including `ReadWritePaths` and
`RestrictAddressFamilies`, while keeping the dedicated user/group,
`NoNewPrivileges` and resource limits. The default remains hardened. Namespace,
capability or seccomp startup failures produce a hint; the installer never
automatically relaxes the sandbox. Re-running without `--no-sandbox` restores
the hardened default; uninstall removes this managed override while preserving
other administrator-owned drop-ins.

The install owns `/opt/codeg-relay`, `/etc/codeg-relay`, `/var/lib/codeg-relay`,
its dedicated user and unit. Verify existing services remain unaffected before
and after deployment. `sudo bash deploy/uninstall.sh` removes only managed
resources and its own firewall rule; back up the key/certificate first if you
want to preserve the relay identity.

Upgrade **home first, then relay, then re-pair phones**. Home supports old relays
with a visible legacy-plaintext warning during migration. The home agent still
pins the relay certificate; phones pin a separate home certificate (QR `fp`).
Home stores its ECDSA P-256 certificate/key under
`<data_dir>/relay/home-tls/{cert.pem,key.pem}` (0600, ten-year validity).
Regenerating it in settings requires re-pairing every phone.

The single listener classifies ClientHello before TLS (10 seconds, 16 KiB,
including split records). ALPN `codeg-relay-agent/1` selects relay-terminated
agent TLS/WSS. Other TLS is spliced unchanged through codeg-tunnel; no second
tunnel, pool or new mux protocol is used. Phones negotiate HTTP/1.1 with home.
`GET http://HOST:PORT/__codeg_relay/v1/health` is plaintext and publicly reports
`status`, `version`, `mode: "e2ee"`, `agent_connected`. Other plaintext GET/HEAD
requests return 404. Offline or over-limit TLS clients are closed without an
HTTP response. Connection logs contain IP, classification, byte counts,
duration and reason; request lines are never parsed on the client path.

Self-signed home certificates require fingerprint pinning or explicit trust in
browsers. A public certificate on the relay does not establish browser trust
in home's separate identity. Back up home certificate and key together; restore
the matching pair to retain phone pins. Rolling back relay requires home legacy
support and phones paired to the old relay certificate; it restores plaintext
visibility at that old relay.
