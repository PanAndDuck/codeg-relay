# codeg-relay deployment kit

See [README.zh-CN.md](README.zh-CN.md) for the full CLI, installation, isolation
checks and rollback guide. This kit serves one active home codeg agent over a
single multiplexed WSS tunnel. Phone TLS passes through the relay and terminates
only at home. The relay is blind to HTTP content and web tokens; it sees only
IP/timing/size metadata and the public `agent_connected` flag. A compromised
relay can disrupt service, but cannot read or alter traffic or impersonate home
to phones that pin its certificate.

Download the Linux x64 or arm64 tarball and matching `.sha256` from
https://github.com/PanAndDuck/codeg-fork/releases. Verify with
`sha256sum -c codeg-relay-linux-<arch>.tar.gz.sha256`, then extract it and enter
the extracted directory:

```bash
sudo bash deploy/install.sh --binary ./codeg-relay \
  --port 18443 --public-host relay.example.com
```

Replace `relay.example.com` with your server's DNS name or IP. Allow that TCP
port in the provider firewall; optionally pass `--open-firewall` to manage UFW.
The installer needs systemd, useradd, flock and ss, and does not install packages.
Keep the generated agent key and certificate fingerprint private. Configure
Settings → Web service → Remote relay in home codeg, then pair iOS using codeg's
QR. The agent key is different from the web token used by iOS.

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
