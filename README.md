# codeg-relay

Standalone components for end-to-end encrypted (E2EE) access to codeg: a [relay server](crates/codeg-relay/deploy/README.md), a [home agent](crates/codeg-relay-agent/README.md), and a [shared multiplexing tunnel](crates/codeg-tunnel/README.md). The home agent connects outbound to the relay and terminates phone TLS at home, keeping phone traffic encrypted through the relay. One relay serves one home.
