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
        .env("USERPROFILE", root.join("home"))
        .env("APPDATA", root.join("home").join("AppData").join("Roaming"))
        .env(
            "LOCALAPPDATA",
            root.join("home").join("AppData").join("Local"),
        )
        .arg("--state-dir")
        .arg(root.join("agent"))
        .args(args)
        .output()
        .unwrap()
}

#[test]
fn init_pair_rotate_and_all_service_dry_runs_are_private_and_side_effect_free() {
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
    let png = dir.path().join("pair 测试 space.png");
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
    #[cfg(windows)]
    {
        let png = dir.path().join("pair-terminal.png");
        let json = dir.path().join("pair-terminal.json");
        let out = cli(
            dir.path(),
            &[
                "pair",
                "--png",
                png.to_str().unwrap(),
                "--json-file",
                json.to_str().unwrap(),
            ],
        );
        assert!(out.status.success());
        assert!(png.exists());
        assert!(String::from_utf8_lossy(&out.stderr).contains("--png"));
        assert!(!out.stderr.contains(&0x1b));
        assert!(!String::from_utf8_lossy(&out.stdout).contains("test-only"));
        assert!(!String::from_utf8_lossy(&out.stderr).contains("test-only"));
    }
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
    for platform in ["macos", "linux", "windows"] {
        for command in ["install-service", "uninstall-service"] {
            let out = cli(dir.path(), &[command, "--dry-run", "--platform", platform]);
            assert!(
                out.status.success(),
                "{platform} {command}: {}",
                String::from_utf8_lossy(&out.stderr)
            );
            assert!(String::from_utf8_lossy(&out.stdout).contains("Dry run"));
            assert!(!String::from_utf8_lossy(&out.stdout).contains("test-only"));
        }
    }
    assert!(!dir.path().join("home").exists());
    assert!(cli(dir.path(), &["status"]).status.success());
}

#[test]
fn background_entry_writes_startup_failure_to_files_and_exits_unsuccessfully() {
    let root = tempfile::tempdir().unwrap();
    let state = root.path().join("uninitialized");
    let logs = root.path().join("logs 测试");
    let output = Command::new(env!("CARGO_BIN_EXE_codeg-relay-agentw"))
        .arg("--state-dir")
        .arg(&state)
        .arg("--log-dir")
        .arg(&logs)
        .output()
        .unwrap();
    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!state.exists());
    let log = secrets::read_private(&logs.join("agent.log")).unwrap();
    assert!(log.contains("background agent failed"));
}

#[cfg(any(unix, windows))]
#[test]
fn background_entry_handles_shutdown_and_persists_stopped_status() {
    use std::{
        net::TcpListener,
        thread,
        time::{Duration, Instant},
    };
    let root = tempfile::tempdir().unwrap();
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    let key = root.path().join("agent.key");
    let token = root.path().join("token");
    secrets::write_private(&key, b"test-only-agent-key-1234", false).unwrap();
    secrets::write_private(&token, b"test-only-web-value", false).unwrap();
    let out = cli(
        root.path(),
        &[
            "init",
            "--relay-url",
            &format!("https://{addr}"),
            "--upstream",
            &format!("http://{addr}"),
            "--relay-fingerprint",
            &"ab".repeat(32),
            "--agent-key-file",
            key.to_str().unwrap(),
            "--token-file",
            token.to_str().unwrap(),
        ],
    );
    assert!(out.status.success());
    struct Child(std::process::Child);
    impl Drop for Child {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    let mut child = Child(
        Command::new(env!("CARGO_BIN_EXE_codeg-relay-agentw"))
            .arg("--state-dir")
            .arg(root.path().join("agent"))
            .arg("--log-dir")
            .arg(root.path().join("logs"))
            .spawn()
            .unwrap(),
    );
    let status = root.path().join("agent/status.json");
    let deadline = Instant::now() + Duration::from_secs(5);
    while !status.exists() {
        assert!(Instant::now() < deadline, "background agent did not start");
        assert!(child.0.try_wait().unwrap().is_none());
        thread::sleep(Duration::from_millis(20));
    }
    // Signal exclusively the disposable child created above.
    #[cfg(unix)]
    assert_eq!(
        unsafe { libc::kill(child.0.id() as libc::pid_t, libc::SIGTERM) },
        0
    );
    #[cfg(windows)]
    {
        use windows_sys::{
            core::BOOL,
            Win32::{
                Foundation::{HWND, LPARAM},
                UI::WindowsAndMessaging::*,
            },
        };
        unsafe extern "system" fn find_child(window: HWND, data: LPARAM) -> BOOL {
            // SAFETY: EnumWindows invokes this synchronously with our live tuple.
            let state = &mut *(data as *mut (u32, HWND));
            let mut pid = 0;
            GetWindowThreadProcessId(window, &mut pid);
            if pid == state.0 {
                state.1 = window;
                return 0;
            }
            1
        }
        let mut target: (u32, HWND) = (child.0.id(), std::ptr::null_mut());
        // Exercise the real GUI executable, not a console allocation/hide trick.
        // This posts a synthetic message to our child only; it cannot log off Windows.
        unsafe {
            EnumWindows(Some(find_child), &mut target as *mut (u32, HWND) as LPARAM);
            assert!(!target.1.is_null(), "background session window missing");
            assert_eq!(IsWindowVisible(target.1), 0);
            assert_ne!(PostMessageW(target.1, WM_ENDSESSION, 1, 0), 0);
        }
    }
    loop {
        if let Some(exit) = child.0.try_wait().unwrap() {
            assert!(exit.success());
            break;
        }
        assert!(Instant::now() < deadline, "background agent failed to stop");
        thread::sleep(Duration::from_millis(20));
    }
    let value: serde_json::Value =
        serde_json::from_str(&secrets::read_private(&status).unwrap()).unwrap();
    assert_eq!(value["state"], "stopped");
    #[cfg(windows)]
    {
        assert_eq!(
            value["state_dir"],
            root.path().join("agent").to_str().unwrap()
        );
        assert_eq!(value["log_dir"], root.path().join("logs").to_str().unwrap());
    }
    let log = secrets::read_private(&root.path().join("logs/agent.log")).unwrap();
    assert!(!log.contains("test-only"));
}

#[cfg(windows)]
#[test]
fn windows_defaults_and_missing_variables_allow_explicit_overrides() {
    let root = tempfile::tempdir().unwrap();
    let local = root.path().join("Local 数据");
    let roaming = root.path().join("Roaming 数据");
    let exe = env!("CARGO_BIN_EXE_codeg-relay-agent");
    let status = Command::new(exe)
        .env("LOCALAPPDATA", &local)
        .arg("status")
        .output()
        .unwrap();
    assert!(status.status.success());
    let status: serde_json::Value = serde_json::from_slice(&status.stdout).unwrap();
    assert_eq!(
        status["state_dir"],
        local.join("codeg-relay-agent").to_str().unwrap()
    );
    assert_eq!(
        status["log_dir"],
        local
            .join("codeg-relay-agent")
            .join("logs")
            .to_str()
            .unwrap()
    );
    assert!(!local.exists());
    let missing = Command::new(exe)
        .env_remove("LOCALAPPDATA")
        .arg("status")
        .output()
        .unwrap();
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("LOCALAPPDATA") && error.contains("--state-dir"));
    let explicit = Command::new(exe)
        .env_remove("LOCALAPPDATA")
        .arg("--state-dir")
        .arg(root.path().join("explicit"))
        .arg("status")
        .output()
        .unwrap();
    assert!(explicit.status.success());

    let key = root.path().join("agent.key");
    secrets::write_private(&key, b"test-only-agent-key-1234", false).unwrap();
    let init = |state: &str| {
        let mut cmd = Command::new(exe);
        cmd.env_remove("APPDATA")
            .env_remove("LOCALAPPDATA")
            .arg("--state-dir")
            .arg(root.path().join(state))
            .args([
                "init",
                "--relay-url",
                "https://relay.example:18443",
                "--relay-fingerprint",
                &"ab".repeat(32),
                "--agent-key-file",
            ])
            .arg(&key);
        cmd
    };
    let missing = init("missing-db").output().unwrap();
    assert!(!missing.status.success());
    let error = String::from_utf8_lossy(&missing.stderr);
    assert!(error.contains("APPDATA") && error.contains("init --db-path"));
    assert!(!root.path().join("missing-db").exists());
    assert!(init("default-db")
        .env("APPDATA", &roaming)
        .output()
        .unwrap()
        .status
        .success());
    let cfg = codeg_relay_agent::config::Config::load(&root.path().join("default-db")).unwrap();
    assert!(
        matches!(cfg.token_source, codeg_relay_agent::config::TokenSource::Database(path) if path == roaming.join("app.codeg").join("codeg.db"))
    );
    // init is deliberately offline: it must not create or open the codeg DB.
    assert!(!roaming.exists());
    for option in ["--db-path", "--token-file"] {
        let out = init(option.trim_start_matches('-'))
            .arg(option)
            .arg(root.path().join("explicit credential"))
            .output()
            .unwrap();
        assert!(
            out.status.success(),
            "{}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
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
