use std::path::{Path, PathBuf};
pub const LABEL: &str = "app.codeg.relay-agent";
#[derive(Clone, Copy)]
pub enum Platform {
    MacOs,
    Linux,
}
pub struct Plan {
    pub path: PathBuf,
    pub content: String,
    pub logs: PathBuf,
    pub commands: Vec<Vec<String>>,
}
fn path_text(path: &Path) -> anyhow::Result<&str> {
    let value = path
        .to_str()
        .ok_or_else(|| anyhow::anyhow!("service path must be UTF-8"))?;
    anyhow::ensure!(
        path.is_absolute() && !value.chars().any(char::is_control),
        "service paths must be absolute and contain no control characters"
    );
    Ok(value)
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
    home: &Path,
    exe: &Path,
    state: &Path,
    uid: u32,
    install: bool,
) -> anyhow::Result<Plan> {
    path_text(home)?;
    let exe = path_text(exe)?;
    let state = path_text(state)?;
    match platform {
        Platform::MacOs => {
            let path = home
                .join("Library/LaunchAgents")
                .join(format!("{LABEL}.plist"));
            let logs = home.join("Library/Logs/codeg-relay-agent");
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
            let domain = format!("gui/{uid}");
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
            })
        }
        Platform::Linux => {
            let name = format!("{LABEL}.service");
            let path = home.join(".config/systemd/user").join(&name);
            let logs = home.join(".local/state/codeg-relay-agent/logs");
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
        println!("Execute: {command:?}");
    }
    if dry_run {
        if install {
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
        std::fs::create_dir_all(plan.path.parent().unwrap())?;
        crate::secrets::private_dir(&plan.logs)?;
        crate::secrets::write_private(&plan.path, plan.content.as_bytes(), true)?;
        for (i, args) in plan.commands.iter().enumerate() {
            execute(args, matches!(platform, Platform::MacOs) && i == 0)?;
        }
    } else {
        execute(
            &plan.commands[0],
            !plan.path.exists() || matches!(platform, Platform::MacOs),
        )?;
        match std::fs::remove_file(&plan.path) {
            Ok(()) => {}
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
            Err(e) => return Err(e.into()),
        }
        for args in &plan.commands[1..] {
            execute(args, false)?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn plans_are_user_scoped_and_escape_paths_without_a_shell() {
        let temp = tempfile::tempdir().unwrap();
        let home_buf = temp.path().join("isolated-home");
        let home = home_buf.as_path();
        let exe = Path::new("/tmp/Codeg & Agent/bin");
        let state = Path::new("/tmp/state space/%n/$HOME");
        let mac = plan(Platform::MacOs, home, exe, state, 501, true).unwrap();
        assert!(mac.path.starts_with(home.join("Library/LaunchAgents")));
        assert!(mac.content.contains("Codeg &amp; Agent"));
        assert!(mac.content.contains("<key>KeepAlive</key><true/>"));
        assert!(mac
            .commands
            .iter()
            .any(|c| c.iter().any(|a| a == "gui/501")));
        let linux = plan(Platform::Linux, home, exe, state, 501, true).unwrap();
        assert!(linux.path.starts_with(home.join(".config/systemd/user")));
        assert!(linux.content.contains("%%n/$$HOME"));
        assert!(linux
            .commands
            .iter()
            .all(|c| c.get(1).is_some_and(|v| v == "--user")));
        assert!(!home.exists());
        assert!(plan(
            Platform::Linux,
            home,
            Path::new("/tmp/evil\npath"),
            state,
            501,
            true
        )
        .is_err());
    }
}
