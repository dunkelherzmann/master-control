//! Installing the agent as a worker that starts with the user's desktop session and has admin rights (specs.md §6).
//!
//! - Linux: a systemd user service tied to the graphical session. Admin rights come from sudo: the install checks
//!   that the user can use it without a password and says what to add if not.
//! - macOS: a launch agent in the user's desktop session, kept running by launchd. Admin rights come from sudo, as
//!   on Linux. Screen and input are not built for macOS yet; a Mac worker does commands and files.
//! - Windows: a scheduled task at sign-in that runs with highest privileges, and a firewall rule that lets the
//!   master reach the worker's port on private networks. Must be run from an elevated prompt.

use std::io;
use std::path::PathBuf;
use std::process::Command;

pub const SERVICE: &str = "master-control";
pub const TASK: &str = "Master Control";
pub const FIREWALL_RULE: &str = "Master Control worker";
pub const LAUNCH_AGENT: &str = "org.mastercontrol.worker";

/// Where the installed program lives.
pub fn installed_program() -> PathBuf {
    let home = crate::state::user_home();
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| home.join("AppData").join("Local")).join("Programs").join("Master Control").join("mc-agent.exe")
    } else {
        home.join(".local").join("bin").join("mc-agent")
    }
}

/// The systemd unit: started with the graphical session, so the worker has the screen, and restarted if it dies.
pub fn unit(program: &str, name: Option<&str>) -> String {
    let name = name.map(|name| format!(" --name {}", quoted(name))).unwrap_or_default();
    format!(
        "[Unit]\nDescription=Master Control worker\nPartOf=graphical-session.target\nAfter=graphical-session.target\n\n\
         [Service]\nExecStart={} serve --worker{name}\nRestart=on-failure\nRestartSec=2\n\n\
         [Install]\nWantedBy=graphical-session.target\n",
        quoted(program)
    )
}

/// The launch agent: started when the user signs in to the desktop, started again if it dies, its output in a log.
pub fn launch_agent(program: &str, name: Option<&str>, log: &str) -> String {
    let mut arguments = vec![program, "serve", "--worker"];
    if let Some(name) = name {
        arguments.extend(["--name", name]);
    }
    let arguments: String = arguments.iter().map(|argument| format!("        <string>{}</string>\n", xml(argument))).collect();
    format!(
        r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key><string>{LAUNCH_AGENT}</string>
    <key>ProgramArguments</key>
    <array>
{arguments}    </array>
    <key>RunAtLoad</key><true/>
    <key>KeepAlive</key><dict><key>SuccessfulExit</key><false/></dict>
    <key>LimitLoadToSessionType</key><string>Aqua</string>
    <key>ProcessType</key><string>Interactive</string>
    <key>StandardOutPath</key><string>{log}</string>
    <key>StandardErrorPath</key><string>{log}</string>
</dict>
</plist>
"#,
        log = xml(log)
    )
}

#[cfg(target_os = "macos")]
fn launch_agent_file() -> PathBuf {
    crate::state::user_home().join("Library").join("LaunchAgents").join(format!("{LAUNCH_AGENT}.plist"))
}

/// The user's own launchd domain, where a desktop session's agents live.
#[cfg(target_os = "macos")]
fn launch_domain() -> String {
    // SAFETY: getuid has no arguments and cannot fail.
    format!("gui/{}", unsafe { libc::getuid() })
}

fn quoted(text: &str) -> String {
    format!("\"{}\"", text.replace('\\', "\\\\").replace('"', "\\\""))
}

fn xml(text: &str) -> String {
    text.replace('&', "&amp;").replace('<', "&lt;").replace('>', "&gt;").replace('"', "&quot;")
}

/// The scheduled task: at this user's sign-in, in their desktop session, with highest privileges, never stopped for
/// running long or for being on battery, and without a console window (conhost's headless mode).
pub fn task(program: &str, user: &str, name: Option<&str>) -> String {
    let name = name.map(|name| format!(" --name \"{name}\"")).unwrap_or_default();
    format!(
        r#"<?xml version="1.0" encoding="UTF-16"?>
<Task version="1.4" xmlns="http://schemas.microsoft.com/windows/2004/02/mit/task">
  <RegistrationInfo><Description>Master Control worker: makes this machine available to its master.</Description></RegistrationInfo>
  <Triggers><LogonTrigger><Enabled>true</Enabled><UserId>{user}</UserId></LogonTrigger></Triggers>
  <Principals><Principal id="Author"><UserId>{user}</UserId><LogonType>InteractiveToken</LogonType><RunLevel>HighestAvailable</RunLevel></Principal></Principals>
  <Settings>
    <MultipleInstancesPolicy>IgnoreNew</MultipleInstancesPolicy>
    <DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>
    <StopIfGoingOnBatteries>false</StopIfGoingOnBatteries>
    <ExecutionTimeLimit>PT0S</ExecutionTimeLimit>
    <RestartOnFailure><Interval>PT1M</Interval><Count>10</Count></RestartOnFailure>
    <Enabled>true</Enabled>
  </Settings>
  <Actions Context="Author"><Exec><Command>conhost.exe</Command><Arguments>{}</Arguments></Exec></Actions>
</Task>
"#,
        xml(&format!("--headless \"{program}\" serve --worker{name}")),
        user = xml(user),
    )
}

/// The networks the Windows firewall rule lets the master in from. Private only, unless the machine's network is
/// marked Public and its owner chose to allow that too.
pub fn firewall_profiles(any_network: bool) -> &'static str {
    if any_network {
        "private,public"
    } else {
        "private"
    }
}

fn run(program: &str, args: &[&str]) -> io::Result<()> {
    let output = Command::new(program).args(args).output().map_err(|error| io::Error::other(format!("{program} couldn't be run: {error}")))?;
    if output.status.success() {
        Ok(())
    } else {
        let said = String::from_utf8_lossy(if output.stderr.is_empty() { &output.stdout } else { &output.stderr }).trim().to_string();
        Err(io::Error::other(format!("{program} {} failed: {said}", args.join(" "))))
    }
}

/// Puts this program in its place and sets it to start as a worker with the desktop session. Returns what was done,
/// line by line.
pub fn install(name: Option<&str>, any_network: bool) -> io::Result<Vec<String>> {
    let program = installed_program();
    let mut done = Vec::new();
    let current = std::env::current_exe()?;
    if current != program {
        std::fs::create_dir_all(program.parent().expect("a folder"))?;
        // Stopped first: a running program's file can't be replaced on Windows.
        let _ = stop();
        // As a new file moved into place: on a Mac, writing over the old file leaves the system with a signature
        // that no longer fits, and it then kills the program as it starts. A copy cut out of the app also carries
        // the app's signature, which doesn't fit a file on its own: it gets one of its own.
        let mut name = program.file_name().unwrap_or_default().to_os_string();
        name.push(".new");
        let new = program.with_file_name(name);
        let failed = |what: &str, error: io::Error| io::Error::other(format!("the program couldn't be {what} {}: {error}", program.display()));
        std::fs::copy(&current, &new).map_err(|error| failed("copied to", error))?;
        if cfg!(target_os = "macos") {
            let _ = std::process::Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&new).output();
        }
        std::fs::rename(&new, &program).map_err(|error| failed("moved to", error))?;
        done.push(format!("copied the program to {}", program.display()));
    }
    let path = program.to_string_lossy().into_owned();
    #[cfg(target_os = "linux")]
    {
        // Linux has no rule to add: nothing here stands between the master and the worker's port.
        let _ = any_network;
        let folder = crate::state::user_home().join(".config").join("systemd").join("user");
        std::fs::create_dir_all(&folder)?;
        std::fs::write(folder.join(format!("{SERVICE}.service")), unit(&path, name))?;
        run("systemctl", &["--user", "daemon-reload"])?;
        run("systemctl", &["--user", "enable", SERVICE])?;
        run("systemctl", &["--user", "restart", SERVICE])?;
        done.push(format!("installed and started the user service {SERVICE} (starts with the desktop session)"));
        if run("sudo", &["-n", "true"]).is_ok() {
            done.push("admin rights: this user can use sudo without a password, so elevated commands work".into());
        } else {
            let user = crate::platform::user();
            done.push(format!(
                "admin rights are NOT set up: sudo asks this user for a password. To grant them, as root: echo '{user} ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/master-control && chmod 440 /etc/sudoers.d/master-control"
            ));
        }
    }
    #[cfg(windows)]
    {
        if !crate::platform::elevated() {
            return Err(io::Error::other("run the install from an elevated prompt: the task and the firewall rule need admin rights"));
        }
        let user = Command::new("whoami").output().map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string()).unwrap_or_default();
        let file = std::env::temp_dir().join("master-control-task.xml");
        // The task scheduler reads its XML as UTF-16 with a byte-order mark.
        let bytes: Vec<u8> = std::iter::once(0xfeffu16).chain(task(&path, &user, name).encode_utf16()).flat_map(u16::to_le_bytes).collect();
        std::fs::write(&file, bytes)?;
        let created = run("schtasks", &["/Create", "/TN", TASK, "/XML", &file.to_string_lossy(), "/F"]);
        let _ = std::fs::remove_file(&file);
        created?;
        done.push(format!("installed the task \"{TASK}\" (starts at sign-in, with highest privileges)"));
        let _ = run("netsh", &["advfirewall", "firewall", "delete", "rule", &format!("name={FIREWALL_RULE}")]);
        run("netsh", &["advfirewall", "firewall", "add", "rule", &format!("name={FIREWALL_RULE}"), "dir=in", "action=allow", &format!("program={path}"), "protocol=TCP", &format!("localport={}", crate::worker::WORKER_PORT), &format!("profile={}", firewall_profiles(any_network))])?;
        done.push(format!("added the firewall rule \"{FIREWALL_RULE}\": this program, TCP port {}, {}", crate::worker::WORKER_PORT, if any_network { "private and public networks" } else { "private networks only" }));
        run("schtasks", &["/Run", "/TN", TASK])?;
        done.push("started the worker".into());
    }
    #[cfg(target_os = "macos")]
    {
        // macOS has its own question for a program that listens on the network; there is no rule to add here.
        let _ = any_network;
        let file = launch_agent_file();
        std::fs::create_dir_all(file.parent().expect("a folder"))?;
        let log = crate::state::user_home().join("Library").join("Logs").join("master-control-worker.log");
        std::fs::write(&file, launch_agent(&path, name, &log.to_string_lossy()))?;
        // Taken out first, so a changed file is read again. launchd takes a moment to let go of the old one, and
        // refuses the new one ("Input/output error") until it has: wait for it, and try more than once.
        let service = format!("{}/{LAUNCH_AGENT}", launch_domain());
        let _ = run("launchctl", &["bootout", &service]);
        let mut loaded = Err(io::Error::other("launchd didn't take the worker"));
        for _ in 0..20 {
            if run("launchctl", &["print", &service]).is_err() {
                loaded = run("launchctl", &["bootstrap", &launch_domain(), &file.to_string_lossy()]);
                if loaded.is_ok() {
                    break;
                }
            }
            std::thread::sleep(std::time::Duration::from_millis(250));
        }
        loaded?;
        done.push(format!("installed and started the launch agent {LAUNCH_AGENT} (starts when you sign in)"));
        done.push("for the screen, the keyboard and the mouse, allow this program in System Settings > Privacy & Security: Screen Recording and Accessibility".into());
        if run("sudo", &["-n", "true"]).is_ok() {
            done.push("admin rights: this user can use sudo without a password, so elevated commands work".into());
        } else {
            let user = crate::platform::user();
            done.push(format!(
                "admin rights are NOT set up: sudo asks this user for a password. To grant them: echo '{user} ALL=(ALL) NOPASSWD: ALL' | sudo tee /etc/sudoers.d/master-control && sudo chmod 440 /etc/sudoers.d/master-control"
            ));
        }
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
    {
        let _ = (name, path, any_network);
        return Err(io::Error::other("installing as a worker isn't built for this system yet"));
    }
    #[allow(unreachable_code)]
    Ok(done)
}

/// Stops the installed worker, if it runs.
fn stop() -> io::Result<()> {
    if cfg!(target_os = "linux") {
        run("systemctl", &["--user", "stop", SERVICE])
    } else if cfg!(target_os = "macos") {
        #[cfg(target_os = "macos")]
        let _ = run("launchctl", &["bootout", &format!("{}/{LAUNCH_AGENT}", launch_domain())]);
        Ok(())
    } else if cfg!(windows) {
        let _ = run("schtasks", &["/End", "/TN", TASK]);
        // Ending the task ends conhost; the agent it started is ended too, but only the installed copy: this very
        // program has the same name and is running the install.
        let installed = installed_program().to_string_lossy().replace('\'', "''");
        let script = format!("Get-Process mc-agent -ErrorAction SilentlyContinue | Where-Object {{ $_.Path -eq '{installed}' }} | Stop-Process -Force");
        let _ = run("powershell", &["-NoProfile", "-Command", &script]);
        std::thread::sleep(std::time::Duration::from_millis(800));
        Ok(())
    } else {
        Ok(())
    }
}

/// Takes the worker out of the session's start-up and removes what the install added. The agent's own folder (its
/// pairing, token and logs) is the caller's to remove.
pub fn uninstall() -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    let _ = stop();
    #[cfg(target_os = "linux")]
    {
        let _ = run("systemctl", &["--user", "disable", SERVICE]);
        let unit = crate::state::user_home().join(".config").join("systemd").join("user").join(format!("{SERVICE}.service"));
        if std::fs::remove_file(unit).is_ok() {
            let _ = run("systemctl", &["--user", "daemon-reload"]);
            done.push(format!("removed the user service {SERVICE}"));
        }
    }
    #[cfg(target_os = "macos")]
    {
        if std::fs::remove_file(launch_agent_file()).is_ok() {
            done.push(format!("removed the launch agent {LAUNCH_AGENT}"));
        }
        // What the worker printed while it ran.
        let _ = std::fs::remove_file(crate::state::user_home().join("Library").join("Logs").join("master-control-worker.log"));
    }
    #[cfg(windows)]
    {
        if run("schtasks", &["/Delete", "/TN", TASK, "/F"]).is_ok() {
            done.push(format!("removed the task \"{TASK}\""));
        }
        if run("netsh", &["advfirewall", "firewall", "delete", "rule", &format!("name={FIREWALL_RULE}")]).is_ok() {
            done.push(format!("removed the firewall rule \"{FIREWALL_RULE}\""));
        }
    }
    let program = installed_program();
    if std::env::current_exe().ok().as_ref() != Some(&program) && std::fs::remove_file(&program).is_ok() {
        done.push(format!("removed {}", program.display()));
    }
    if done.is_empty() {
        done.push("nothing was installed".into());
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_service_starts_the_worker_with_the_desktop_session() {
        let text = unit("/home/a b/.local/bin/mc-agent", Some("test \"rig\""));
        assert!(text.contains("ExecStart=\"/home/a b/.local/bin/mc-agent\" serve --worker --name \"test \\\"rig\\\"\"\n"), "{text}");
        assert!(text.contains("WantedBy=graphical-session.target") && text.contains("Restart=on-failure"));
        assert!(unit("/x/mc-agent", None).contains("ExecStart=\"/x/mc-agent\" serve --worker\n"));
    }

    #[test]
    fn the_launch_agent_starts_the_worker_at_sign_in_and_keeps_it_running() {
        let text = launch_agent("/Users/a & b/.local/bin/mc-agent", Some("studio <mac>"), "/Users/a & b/Library/Logs/w.log");
        assert!(text.contains("<key>Label</key><string>org.mastercontrol.worker</string>"));
        assert!(text.contains("        <string>/Users/a &amp; b/.local/bin/mc-agent</string>\n        <string>serve</string>\n        <string>--worker</string>\n        <string>--name</string>\n        <string>studio &lt;mac&gt;</string>\n"), "{text}");
        assert!(text.contains("<key>RunAtLoad</key><true/>") && text.contains("<key>LimitLoadToSessionType</key><string>Aqua</string>") && text.contains("<key>SuccessfulExit</key><false/>"));
        assert!(text.contains("<string>/Users/a &amp; b/Library/Logs/w.log</string>"));
        assert!(!launch_agent("/x/mc-agent", None, "/x/w.log").contains("--name"));
    }

    #[test]
    fn the_firewall_rule_is_for_private_networks_unless_told_otherwise() {
        assert_eq!((firewall_profiles(false), firewall_profiles(true)), ("private", "private,public"));
    }

    #[test]
    fn the_task_runs_at_sign_in_with_highest_privileges_and_no_window() {
        let text = task(r"C:\Users\A & B\mc-agent.exe", r"PC\A & B", None);
        assert!(text.contains("<RunLevel>HighestAvailable</RunLevel>") && text.contains("<LogonTrigger>") && text.contains("<LogonType>InteractiveToken</LogonType>"));
        assert!(text.contains(r"<UserId>PC\A &amp; B</UserId>"), "{text}");
        assert!(text.contains(r"<Command>conhost.exe</Command><Arguments>--headless &quot;C:\Users\A &amp; B\mc-agent.exe&quot; serve --worker</Arguments>"), "{text}");
        assert!(text.contains("<ExecutionTimeLimit>PT0S</ExecutionTimeLimit>") && text.contains("<DisallowStartIfOnBatteries>false</DisallowStartIfOnBatteries>"));
    }
}
