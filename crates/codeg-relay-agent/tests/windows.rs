#![cfg(windows)]

use codeg_relay_agent::{certificate, secrets};
use std::{fs, process::Command};

#[test]
fn junctions_cannot_redirect_private_directories_or_the_tls_identity() {
    let root = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let state = root.path().join("state");
    fs::create_dir(&state).unwrap();
    let junction = state.join("relay");
    // Junctions need no Developer Mode or symlink privilege. Pass paths through
    // the child environment so spaces/Unicode/shell metacharacters remain data.
    let output = Command::new("powershell.exe")
        .args(["-NoProfile", "-NonInteractive", "-Command", "$null = New-Item -ItemType Junction -Path $env:TEST_JUNCTION_LINK -Target $env:TEST_JUNCTION_TARGET -ErrorAction Stop"])
        .env("TEST_JUNCTION_LINK", &junction)
        .env("TEST_JUNCTION_TARGET", outside.path())
        .output().unwrap();
    assert!(
        output.status.success(),
        "cannot create test junction: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(secrets::private_dir(&junction).is_err());
    assert!(certificate::load(&state, false).is_err());
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 0);
    fs::remove_dir(&junction).unwrap();
}

#[test]
fn file_symlinks_cannot_redirect_credential_reads_or_writes() {
    let root = tempfile::tempdir().unwrap();
    let target = root.path().join("actual-token");
    let link = root.path().join("redirected-token");
    secrets::write_private(&target, b"test-only-value", false).unwrap();
    if let Err(error) = std::os::windows::fs::symlink_file(&target, &link) {
        if error.raw_os_error() == Some(1314) {
            eprintln!("file symlink test needs Developer Mode or symlink privilege; junction coverage still runs");
            return;
        }
        panic!("cannot create test symlink: {error}");
    }
    assert!(secrets::read_private(&link).is_err());
    assert!(secrets::open_private(&link, true).is_err());
    assert!(secrets::write_private(&link, b"replacement", true).is_err());
    assert_eq!(secrets::read_private(&target).unwrap(), "test-only-value");
}
