use std::process::Command;

fn run(args: &[&str]) -> std::process::Output {
    Command::new(env!("CARGO_BIN_EXE_codeg-relay"))
        .args(args)
        .env_remove("CODEG_RELAY_AGENT_KEY")
        .output()
        .unwrap()
}

#[test]
fn generated_key_is_random_base64url_32_bytes() {
    use base64::Engine;
    let a = run(&["gen-key"]);
    let b = run(&["gen-key"]);
    assert!(a.status.success());
    let key = String::from_utf8(a.stdout).unwrap();
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(key.trim())
        .unwrap();
    assert_eq!(bytes.len(), 32);
    assert_ne!(key.as_bytes(), b.stdout);
}

#[test]
fn unsafe_or_incomplete_configuration_is_rejected() {
    for args in [
        vec!["serve", "--insecure-plaintext", "--listen", "0.0.0.0:18443"],
        vec!["serve", "--insecure-plaintext", "--listen", "127.0.0.1:0"],
        vec!["serve", "--self-signed"],
        vec!["serve", "--tls-cert", "/nonexistent.pem"],
    ] {
        assert!(!run(&args).status.success(), "{args:?}");
    }
}
