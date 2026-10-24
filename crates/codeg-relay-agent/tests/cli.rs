use codeg_relay_agent::secrets;
use std::{
    path::Path,
    process::{Command, Output},
};

fn cli(root: &Path, args: &[&str]) -> Output {
    Command::new(env!("CARGO_BIN_EXE_codeg-relay-agent"))
        .env_remove("CODEG_DATA_DIR")
        .env_remove("CODEG_HOME")
        .env_remove("CODEG_STATIC_DIR")
        .env_remove("CODEG_ACP_TMP_ROOT")
        .env_remove("CODEG_TOKEN")
        .env_remove("CODEG_PORT")
        .env_remove("CODEG_HOST")
        .env("HOME", root.join("home"))
        .arg("--state-dir")
        .arg(root.join("agent"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn init_pair_rotate_and_both_service_dry_runs_are_private_and_side_effect_free() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("agent.key");
    let token = dir.path().join("web.token");
    secrets::write_private(&key, b"test-only-agent-value-123456", false).unwrap();
    secrets::write_private(&token, b"test-only-web-value", false).unwrap();
    let out = cli(
        dir.path(),
        &[
            "init",
            "--relay-url",
            "https://relay.example:18443",
            "--agent-key-file",
            key.to_str().unwrap(),
            "--relay-fingerprint",
            &"ab".repeat(32),
            "--token-file",
            token.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    assert!(dir.path().join("agent/config.json").exists());
    assert!(dir.path().join("agent/relay/home-tls/key.pem").exists());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("test-only"));
    let pair = dir.path().join("pair.json");
    let png = dir.path().join("pair.png");
    let out = cli(
        dir.path(),
        &[
            "pair",
            "--json-only",
            "--json-file",
            pair.to_str().unwrap(),
            "--png",
            png.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    assert!(!String::from_utf8_lossy(&out.stdout).contains("test-only"));
    let before: serde_json::Value =
        serde_json::from_str(&secrets::read_private(&pair).unwrap()).unwrap();
    assert_eq!(before["mode"], "relay");
    assert_eq!(before["url"], "https://relay.example:18443");
    assert!(before["fp"] != "ab".repeat(32));
    let out = cli(dir.path(), &["rotate-cert"]);
    assert!(out.status.success());
    let out = cli(dir.path(), &["pair", "--json-only"]);
    assert!(out.status.success());
    let after: serde_json::Value = serde_json::from_slice(&out.stdout).unwrap();
    assert!(before["fp"] != after["fp"]);
    assert!(before["token"] == after["token"]);
    for platform in ["macos", "linux"] {
        for command in ["install-service", "uninstall-service"] {
            let out = cli(dir.path(), &[command, "--dry-run", "--platform", platform]);
            assert!(out.status.success());
            assert!(String::from_utf8_lossy(&out.stdout).contains("Dry run"));
            assert!(!String::from_utf8_lossy(&out.stdout).contains("test-only"));
        }
    }
    assert!(!dir.path().join("home").exists());
    assert!(cli(dir.path(), &["status"]).status.success());
}

#[test]
fn init_rejects_plaintext_relay_and_does_not_create_state() {
    let dir = tempfile::tempdir().unwrap();
    let key = dir.path().join("agent.key");
    secrets::write_private(&key, b"test-only-agent-value-123456", false).unwrap();
    let out = cli(
        dir.path(),
        &[
            "init",
            "--relay-url",
            "http://127.0.0.1:18443",
            "--agent-key-file",
            key.to_str().unwrap(),
            "--relay-fingerprint",
            &"ab".repeat(32),
        ],
    );
    assert!(!out.status.success());
    assert!(!dir.path().join("agent").exists());
}
