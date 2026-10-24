use std::path::{Path, PathBuf};
pub const LABEL: &str = "app.codeg.relay-agent";
#[derive(Clone, Copy)]
pub enum Platform {
    MacOs,
    Linux,
    Windows,
}
pub struct User {
    pub home: PathBuf,
    pub logs: PathBuf,
    /// Unix effective UID, or Windows account SID (never a password).
    pub id: String,
}
pub struct Plan {
    pub path: PathBuf,
    pub content: String,
    pub logs: PathBuf,
    pub commands: Vec<Vec<String>>,
    pub background_exe: Option<PathBuf>,
}
fn path_text(path: &Path) -> anyhow::Result<&str> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("service path must be UTF-8"))?;
    anyhow::ensure!(
        (path.is_absolute() || windows_absolute(value)) && !value.chars().any(char::is_control),
        "service paths must be absolute and contain no control characters"
    );
    Ok(value)
}
fn windows_absolute(value: &str) -> bool {
    let value = value.strip_prefix(r"\\?\").unwrap_or(value);
    let bytes = value.as_bytes();
    (bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'\\' | b'/'))
        || value
            .strip_prefix(r"\\")
            .is_some_and(|v| v.split('\\').filter(|s| !s.is_empty()).count() >= 2)
}

// Quote for the Windows CRT argv parser, not cmd.exe or PowerShell. In
// particular, a trailing backslash must be doubled before the closing quote.
fn windows_quote(value: &str) -> String {
    let mut output = String::from("\"");
    let mut slashes = 0;
    for character in value.chars() {
        if character == '\\' {
            slashes += 1;
        } else {
            output.extend(std::iter::repeat_n(
                '\\',
                if character == '"' {
                    slashes * 2 + 1
                } else {
                    slashes
                },
            ));
            output.push(character);
            slashes = 0;
        }
    }
    output.extend(std::iter::repeat_n('\\', slashes * 2));
    output.push('"');
    output
}

fn valid_sid(value: &str) -> bool {
    value.starts_with("S-1-")
        && value[4..]
            .split('-')
            .all(|p| !p.is_empty() && p.bytes().all(|b| b.is_ascii_digit()))
}

pub fn windows_user_id() -> anyhow::Result<String> {
    #[cfg(windows)]
    {
        // SID is unambiguous for local/domain/Microsoft accounts and avoids
        // depending on USERNAME, locale, or an OEM-encoded account name.
        let output = std::process::Command::new("whoami.exe")
            .args(["/user", "/fo", "csv", "/nh"])
            .output()?;
        anyhow::ensure!(
            output.status.success(),
            "cannot determine current Windows user SID"
        );
        String::from_utf8_lossy(&output.stdout)
            .split('"')
            .find(|v| valid_sid(v))
            .map(str::to_owned)
            .ok_or_else(|| anyhow::anyhow!("cannot parse current Windows user SID"))
    }
    #[cfg(not(windows))]
    {
        // A synthetic SID is used only for foreign-platform dry runs.
        Ok("S-1-5-21-1000-1000-1000-1001".into())
    }
}

fn encoded_content(content: &str, platform: Platform) -> Vec<u8> {
    if matches!(platform, Platform::Windows) {
        // schtasks /XML accepts UTF-16LE with BOM; the declaration matches the
        // bytes on disk, including non-ASCII user paths. No PowerShell dependency.
        [0xff, 0xfe]
            .into_iter()
            .chain(content.encode_utf16().flat_map(u16::to_le_bytes))
            .collect()
    } else {
        content.as_bytes().to_vec()
    }
}
fn xml(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;")
}
fn unit_quote(value: &str) -> String {
    format!(
        "\"{}\"",
        value
            .replace('\\', "\\\\")
            .replace('"', "\\\"")
            .replace('%', "%%")
            .replace('$', "$$")
    )
}
fn command(args: &[&str]) -> Vec<String> {
    args.iter().map(|s| s.to_string()).collect()
}
pub fn plan(
    platform: Platform,
    user: &User,
    exe: &Path,
    state: &Path,
    install: bool,
) -> anyhow::Result<Plan> {
    let home = &user.home;
    path_text(home)?;
    let exe = path_text(exe)?;
    let state = path_text(state)?;
    match platform {
        Platform::MacOs => {
            let path = home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist"));
            let logs = user.logs.clone();
            let arguments = [
                exe,
                "--state-dir",
                state,
                "run",
                "--log-dir",
                path_text(&logs)?,
            ]
            .iter()
            .map(|s| format!("<string>{}</string>", xml(s)))
            .collect::<String>();
            let content = format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict>\n<key>Label</key><string>{LABEL}</string>\n<key>ProgramArguments</key><array>{arguments}</array>\n<key>RunAtLoad</key><true/>\n<key>KeepAlive</key><true/>\n<key>ThrottleInterval</key><integer>10</integer>\n<key>Umask</key><integer>63</integer>\n<key>StandardOutPath</key><string>/dev/null</string>\n<key>StandardErrorPath</key><string>/dev/null</string>\n</dict></plist>\n");
            let domain = format!("gui/{}", user.id);
            let mut commands = vec![command(&[
                "launchctl",
                "bootout",
                &format!("{domain}/{LABEL}"),
            ])];
            if install {
                commands.push(command(&[
                    "launchctl",
                    "enable",
                    &format!("{domain}/{LABEL}"),
                ]));
                commands.push(command(&[
                    "launchctl",
                    "bootstrap",
                    &domain,
                    path_text(&path)?,
                ]));
            }
            Ok(Plan {
                path,
                content,
                logs,
                commands,
                background_exe: None,
            })
        }
        Platform::Linux => {
            let name = format!("{LABEL}.service");
            let path = home.join(".config/systemd/user").join(&name);
            let logs = user.logs.clone();
            let args = [
                exe,
                "--state-dir",
                state,
                "run",
                "--log-dir",
                path_text(&logs)?,
            ]
            .iter()
            .map(|s| unit_quote(s))
            .collect::<Vec<_>>()
            .join(" ");
            let content = format!("[Unit]\nDescription=codeg E2EE relay agent\nAfter=network-online.target\n\n[Service]\nType=simple\nExecStart={args}\nRestart=always\nRestartSec=5\nUMask=0077\nNoNewPrivileges=true\nStandardOutput=null\nStandardError=null\n\n[Install]\nWantedBy=default.target\n");
            let commands = if install {
                vec![
                    command(&["systemctl", "--user", "daemon-reload"]),
                    command(&["systemctl", "--user", "enable", &name]),
                    command(&["systemctl", "--user", "restart", &name]),
                ]
            } else {
                vec![
                    command(&["systemctl", "--user", "disable", "--now", &name]),
                    command(&["systemctl", "--user", "daemon-reload"]),
                ]
            };
            Ok(Plan {
                path,
                content,
                logs,
                commands,
                background_exe: None,
            })
        }
        Platform::Windows => {
            anyhow::ensure!(
                valid_sid(&user.id),
                "Windows task requires a current-user SID"
            );
            let logs = user.logs.clone();
            let log_text = path_text(&logs)?;
            let separator = exe
                .rfind(['/', '\\'])
                .ok_or_else(|| anyhow::anyhow!("service executable needs an absolute directory"))?;
            let background_exe = format!("{}codeg-relay-agentw.exe", &exe[..=separator]);
            // Native joins handle drive/verbatim paths at runtime; lexical joins
            // also let Windows-style fixture paths be previewed on Unix.
            let separator = if state.contains('\\') { "\\" } else { "/" };
            let path = PathBuf::from(format!(
                "{}{separator}{LABEL}.xml",
                state.trim_end_matches(['/', '\\'])
            ));
            let task_name = format!("{LABEL}-{}", user.id);
            let arguments = ["--state-dir", state, "--log-dir", log_text]
                .iter()
                .map(|arg| windows_quote(arg))
                .collect::<Vec<_>>()
                .join(" ");
            let sid = xml(&user.id);
            let executable = xml(&background_exe);
            let arguments = xml(&arguments);
            let content = format!(
                r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.2" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>codeg E2EE relay agent (current user)</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{sid}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="User"><UserId>{sid}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <AllowHardTerminate>true</AllowHardTerminate>
    <StartWhenAvailable>true</StartWhenAvailable>
    <RunOnlyIfNetworkAvailable>false</RunOnlyIfNetworkAvailable>
    <AllowStartOnDemand>true</AllowStartOnDemand><Enabled>true</Enabled>
    <RunOnlyIfIdle>false</RunOnlyIfIdle><ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure><Interval>PT1M</Interval><Count>999</Count></RestartOnFailure>
  </Settings>
  <Actions Context="User"><Exec><Command>{executable}</Command><Arguments>{arguments}</Arguments></Exec></Actions>
</Task>
"#
            );
            let commands = if install {
                vec![
                    command(&[
                        "schtasks.exe",
                        "/Create",
                        "/TN",
                        &task_name,
                        "/XML",
                        path_text(&path)?,
                        "/F",
                    ]),
                    command(&["schtasks.exe", "/Run", "/TN", &task_name]),
                ]
            } else {
                vec![
                    command(&["schtasks.exe", "/End", "/TN", &task_name]),
                    command(&["schtasks.exe", "/Delete", "/TN", &task_name, "/F"]),
                ]
            };
            Ok(Plan {
                path,
                content,
                logs,
                commands,
                background_exe: Some(background_exe.into()),
            })
        }
    }
}

pub fn apply(plan: &Plan, platform: Platform, install: bool, dry_run: bool) -> anyhow::Result<()> {
    println!(
        "{} {}",
        if install { "Write" } else { "Remove" },
        plan.path.display()
    );
    if install {
        println!("Logs: {} (5 MiB/file, 3 archives)", plan.logs.display());
    }
    for command in &plan.commands {
        if matches!(platform, Platform::Windows) {
            println!(
                "Execute: {}",
                command
                    .iter()
                    .map(|arg| windows_quote(arg))
                    .collect::<Vec<_>>()
                    .join(" ")
            );
        } else {
            println!("Execute: {command:?}");
        }
    }
    if dry_run {
        if install {
            if matches!(platform, Platform::Windows) {
                println!("XML file encoding: UTF-16LE with BOM (preview below is text).");
            }
            println!("{}", plan.content);
        }
        println!("Dry run: no files written and no service commands executed.");
        return Ok(());
    }
    let execute = |args: &[String], allow_missing: bool| -> anyhow::Result<()> {
        let result = std::process::Command::new(&args[0])
            .args(&args[1..])
            .status()?;
        anyhow::ensure!(
            result.success() || allow_missing,
            "service manager command failed"
        );
        Ok(())
    };
    if install {
        if let Some(exe) = &plan.background_exe {
            anyhow::ensure!(exe.is_file(), "install codeg-relay-agentw.exe beside codeg-relay-agent.exe before registering the task");
        }
        std::fs::create_dir_all(plan.path.parent().unwrap())?;
        crate::secrets::private_dir(&plan.logs)?;
        crate::secrets::write_private(&plan.path, &encoded_content(&plan.content, platform), true)?;
        for (i, args) in plan.commands.iter().enumerate() {
            execute(args, matches!(platform, Platform::MacOs) && i == 0)?;
        }
    } else {
        execute(
            &plan.commands[0],
            !plan.path.exists() || matches!(platform, Platform::MacOs | Platform::Windows),
        )?;
        if matches!(platform, Platform::Windows) {
            // /End is allowed to fail when already stopped. /Delete must succeed
            // before discarding our saved task definition. Neither command
            // deletes identity, configuration or logs.
            execute(&plan.commands[1], false)?;
        }
        match std::fs::remove_file(&plan.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        if !matches!(platform, Platform::Windows) {
            for args in &plan.commands[1..] {
                execute(args, false)?;
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn windows_task_has_a_user_logon_trigger_and_a_hidden_restartable_action() {
        let user = User {
            home: PathBuf::from(r"C:\Users\测试 用户"),
            logs: PathBuf::from(r"C:\Users\测试 用户\AppData\Local\codeg-relay-agent\logs"),
            id: "S-1-5-21-1000-1000-1000-1001".into(),
        };
        let exe = Path::new(r"C:\Tools & Helpers\codeg-relay-agent.exe");
        let state = Path::new(r"\\?\C:\Users\测试 用户\Agent State");
        let plan = plan(Platform::Windows, &user, exe, state, true).unwrap();
        for required in [
            "<LogonTrigger><Enabled>true</Enabled><UserId>S-1-5-21-1000-1000-1000-1001</UserId></LogonTrigger>",
            "<Principal id=\"User\"><UserId>S-1-5-21-1000-1000-1000-1001</UserId>",
            "<LogonType>InteractiveToken</LogonType><RunLevel>LeastPrivilege</RunLevel>",
            "<MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>",
            "<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>",
            "<RestartOnFailure><Interval>PT1M</Interval><Count>999</Count></RestartOnFailure>",
            "<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>",
            "<StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>",
            r"<Command>C:\Tools &amp; Helpers\codeg-relay-agentw.exe</Command>",
            "&quot;--log-dir&quot;",
        ] {
            assert!(plan.content.contains(required), "missing {required}");
        }
        assert_eq!(plan.logs, user.logs);
        assert_eq!(
            plan.commands,
            vec![
                command(&[
                    "schtasks.exe",
                    "/Create",
                    "/TN",
                    "app.codeg.relay-agent-S-1-5-21-1000-1000-1000-1001",
                    "/XML",
                    r"\\?\C:\Users\测试 用户\Agent State\app.codeg.relay-agent.xml",
                    "/F"
                ]),
                command(&[
                    "schtasks.exe",
                    "/Run",
                    "/TN",
                    "app.codeg.relay-agent-S-1-5-21-1000-1000-1000-1001"
                ]),
            ]
        );
        assert!(!plan.content.contains("<Password>"));
        assert!(!plan
            .commands
            .iter()
            .flatten()
            .any(|v| v == "/RP" || v == "/RU"));
        let bytes = encoded_content(&plan.content, Platform::Windows);
        assert_eq!(&bytes[..2], &[0xff, 0xfe]);
        let units = bytes[2..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|b| u16::from_le_bytes([b[0], b[1]]))
            .collect::<Vec<_>>();
        assert_eq!(String::from_utf16(&units).unwrap(), plan.content);
        assert!(plan.content.contains("encoding=\"UTF-16\""));
    }

    #[test]
    fn windows_uninstall_ends_then_deletes_only_this_users_task() {
        let root = tempfile::tempdir().unwrap();
        let mut user = User {
            home: root.path().to_owned(),
            logs: root.path().join("logs"),
            id: "S-1-5-21-1001".into(),
        };
        let exe = root.path().join("codeg-relay-agent.exe");
        let first = plan(Platform::Windows, &user, &exe, root.path(), false).unwrap();
        assert_eq!(
            first.commands,
            vec![
                command(&[
                    "schtasks.exe",
                    "/End",
                    "/TN",
                    "app.codeg.relay-agent-S-1-5-21-1001"
                ]),
                command(&[
                    "schtasks.exe",
                    "/Delete",
                    "/TN",
                    "app.codeg.relay-agent-S-1-5-21-1001",
                    "/F"
                ]),
            ]
        );
        user.id = "S-1-5-21-1002".into();
        let second = plan(Platform::Windows, &user, &exe, root.path(), false).unwrap();
        assert_ne!(first.commands, second.commands);
        user.id = "not-a-sid".into();
        assert!(plan(Platform::Windows, &user, &exe, root.path(), false).is_err());
    }

    #[test]
    fn windows_argv_quotes_spaces_quotes_and_trailing_backslashes() {
        for (input, expected) in [
            ("", "\"\""),
            (r"C:\State Dir\", r#""C:\State Dir\\""#),
            (r#"one\"two"#, r#""one\\\"two""#),
            ("%USER% & name", "\"%USER% & name\""),
        ] {
            assert_eq!(windows_quote(input), expected);
        }
    }

    #[test]
    fn plans_are_user_scoped_and_escape_paths_without_a_shell() {
        let temp = tempfile::tempdir().unwrap();
        let user = User {
            home: temp.path().join("isolated-home"),
            logs: temp.path().join("logs"),
            id: "501".into(),
        };
        let home = &user.home;
        let exe = temp.path().join("Codeg & Agent").join("bin");
        let state = temp.path().join("state space").join("%n").join("$HOME");
        let mac = plan(Platform::MacOs, &user, &exe, &state, true).unwrap();
        assert!(mac.path.starts_with(home.join("Library/LaunchAgents")));
        assert!(mac.content.contains("Codeg &amp; Agent"));
        assert!(mac.content.contains("<key>KeepAlive</key><true/>"));
        assert!(mac
            .commands
            .iter()
            .any(|c| c.iter().any(|a| a == "gui/501")));
        let linux = plan(Platform::Linux, &user, &exe, &state, true).unwrap();
        assert!(linux.path.starts_with(home.join(".config/systemd/user")));
        assert!(linux.content.contains("%%n") && linux.content.contains("$$HOME"));
        assert!(linux
            .commands
            .iter()
            .all(|c| c.get(1).is_some_and(|v| v == "--user")));
        assert!(!home.exists());
        assert!(plan(
            Platform::Linux,
            &user,
            &temp.path().join("evil\npath"),
            &state,
            true
        )
        .is_err());
    }
}
