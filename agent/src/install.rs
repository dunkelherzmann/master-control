//! Installing the agent as a worker that starts with the user's desktop session and has admin rights (specs.md §6).
//!
//! - Linux: a systemd user service tied to the graphical session. The install first puts on the system what the
//!   program can't carry (`system_setup`): missing packages and a sudo rule for admin rights, asking for the password
//!   once.
//! - macOS: a launch agent in the user's desktop session, kept running by launchd. Admin rights come from sudo, as
//!   on Linux.
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
    let mut done = system_setup();
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
        if let Some(note) = gnome_extension()? {
            done.push(note);
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

/// What a Linux desktop has to have that Master Control doesn't carry in its own file, and the package that brings
/// it with each package manager (apt, dnf, pacman, zypper). The program itself is static and needs no libraries; it
/// finds out for itself whether the session is X11 or GNOME on Wayland.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
struct Need {
    what: &'static str,
    packages: [&'static str; 4],
}

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SUDO: Need = Need { what: "sudo, for admin rights", packages: ["sudo"; 4] };
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const ACCESSIBILITY_BUS: Need = Need { what: "the accessibility bus, for what is inside windows", packages: ["at-spi2-core"; 4] };
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const GSETTINGS: Need = Need { what: "gsettings, to switch on the GNOME Shell extension", packages: ["libglib2.0-bin", "glib2", "glib2", "glib2-tools"] };

/// The package managers, in the order of `Need::packages`, with the command that installs without asking.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const MANAGERS: [(&str, &str); 4] = [
    ("apt-get", "DEBIAN_FRONTEND=noninteractive apt-get install -y"),
    ("dnf", "dnf install -y"),
    ("pacman", "pacman -S --needed --noconfirm"),
    ("zypper", "zypper --non-interactive install"),
];

#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
const SUDO_RULE: &str = "/etc/sudoers.d/master-control";

/// The commands that, run as root, install what is missing and give `user` admin rights without a password; None
/// when nothing is to be done. `manager` is the index into `MANAGERS` of the one this system has.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn root_script(missing: &[&Need], manager: Option<usize>, rule_for: Option<&str>) -> Option<String> {
    let mut lines = Vec::new();
    if let (false, Some(manager)) = (missing.is_empty(), manager) {
        let mut packages: Vec<&str> = missing.iter().map(|need| need.packages[manager]).collect();
        packages.dedup();
        if manager == 0 {
            lines.push("apt-get update -q".to_string());
        }
        lines.push(format!("{} {}", MANAGERS[manager].1, packages.join(" ")));
    }
    if let Some(user) = rule_for {
        // Checked before it is put in place: a broken file in sudoers.d stops sudo for everyone.
        lines.push(format!("printf '%s\\n' '{user} ALL=(ALL) NOPASSWD: ALL' > {SUDO_RULE}.new"));
        lines.push(format!("chmod 440 {SUDO_RULE}.new"));
        lines.push(format!("if command -v visudo >/dev/null; then visudo -cqf {SUDO_RULE}.new; fi"));
        lines.push(format!("mv {SUDO_RULE}.new {SUDO_RULE}"));
    }
    (!lines.is_empty()).then(|| format!("set -e\n{}\n", lines.join("\n")))
}

/// A user name that can go into a sudo rule and a shell line as it is.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn plain_user_name(user: &str) -> bool {
    !user.is_empty() && user.len() <= 32 && user.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')) && !user.starts_with('-')
}

#[cfg(target_os = "linux")]
fn on_path(program: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|folder| folder.join(program).is_file()))
}

/// Linux: puts on the system what the program needs and can't carry (`Need`), and the sudo rule that gives this
/// user admin rights, as root: through sudo when it asks no password, else through the desktop's password prompt
/// (pkexec), else sudo in the terminal. Says what it did, or what is left for the user to do. `MC_NO_SYSTEM_SETUP`
/// leaves the system alone. Elsewhere it has nothing to do.
pub fn system_setup() -> Vec<String> {
    #[cfg(target_os = "linux")]
    {
        use std::io::IsTerminal;
        if std::env::var_os("MC_NO_SYSTEM_SETUP").is_some() {
            return vec!["left the system as it is (MC_NO_SYSTEM_SETUP is set)".into()];
        }
        let mut missing = Vec::new();
        if !on_path("sudo") {
            missing.push(&SUDO);
        }
        let bus = ["/usr/share/dbus-1/services/org.a11y.Bus.service", "/usr/libexec/at-spi-bus-launcher", "/usr/lib/at-spi-bus-launcher", "/usr/lib/at-spi2-core/at-spi-bus-launcher"];
        if !bus.iter().any(|file| std::path::Path::new(file).exists()) {
            missing.push(&ACCESSIBILITY_BUS);
        }
        if std::path::Path::new("/usr/bin/gnome-shell").exists() && !on_path("gsettings") {
            missing.push(&GSETTINGS);
        }
        let manager = MANAGERS.iter().position(|(program, _)| on_path(program));
        let user = crate::platform::user();
        // SAFETY: geteuid has no arguments and cannot fail.
        let root = unsafe { libc::geteuid() } == 0;
        let sudo_asks = !root && !(on_path("sudo") && run("sudo", &["-n", "true"]).is_ok());
        let rule_for = (sudo_asks && plain_user_name(&user)).then_some(user.as_str());
        let mut done = Vec::new();
        if !missing.is_empty() && manager.is_none() {
            done.push(format!(
                "this system's package manager isn't one Master Control knows (apt, dnf, pacman, zypper): install {} yourself",
                missing.iter().map(|need| need.what).collect::<Vec<_>>().join(", ")
            ));
        }
        let Some(script) = root_script(&missing, manager, rule_for) else {
            done.push("the system has everything Master Control needs, and admin rights work without a password".into());
            return done;
        };
        let graphical = std::env::var_os("DISPLAY").is_some() || std::env::var_os("WAYLAND_DISPLAY").is_some();
        let ran = if root {
            Some(("as root", run("sh", &["-c", &script])))
        } else if !sudo_asks {
            Some(("through sudo", run("sudo", &["-n", "sh", "-c", &script])))
        } else if on_path("pkexec") && graphical {
            Some(("after the system asked for the password", run("pkexec", &["/bin/sh", "-c", &script])))
        } else if on_path("sudo") && std::io::stdin().is_terminal() {
            let status = Command::new("sudo").args(["sh", "-c", &script]).status();
            Some(("through sudo", status.and_then(|status| if status.success() { Ok(()) } else { Err(io::Error::other(format!("sudo ended with {status}"))) })))
        } else {
            None
        };
        let what = missing.iter().map(|need| need.what).chain(rule_for.map(|_| "admin rights without a password (a sudo rule)")).collect::<Vec<_>>().join(", ");
        match ran {
            Some((how, Ok(()))) => done.push(format!("set up {what}, {how}")),
            Some((_, Err(error))) => done.push(format!("could NOT set up {what} ({error}); as root, run:\n{script}")),
            None => done.push(format!("{what} still to set up: as root, run:\n{script}")),
        }
        done
    }
    #[cfg(not(target_os = "linux"))]
    Vec::new()
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

/// Puts the GNOME Shell extension that serves the screen, input and window list on Wayland in place and enables it,
/// on a machine with GNOME Shell. Says what was done, and when it starts working.
#[cfg(target_os = "linux")]
fn gnome_extension() -> io::Result<Option<String>> {
    use crate::desktop::gnome;
    let has_shell = std::path::Path::new("/usr/bin/gnome-shell").exists();
    if !has_shell {
        return Ok(None);
    }
    let folder = gnome::folder();
    std::fs::create_dir_all(&folder)?;
    let changed = std::fs::read_to_string(folder.join("extension.js")).ok().as_deref() != Some(gnome::EXTENSION);
    std::fs::write(folder.join("metadata.json"), gnome::METADATA)?;
    std::fs::write(folder.join("extension.js"), gnome::EXTENSION)?;
    // Enabled through its setting, which the shell reads at sign-in and watches while it runs.
    let enabled = Command::new("gsettings").args(["get", "org.gnome.shell", "enabled-extensions"]).output();
    let list = enabled.as_ref().ok().filter(|output| output.status.success()).map(|output| String::from_utf8_lossy(&output.stdout).trim().to_string());
    let Some(list) = list else {
        return Ok(Some(format!(
            "put the GNOME Shell extension {} in place, but couldn't enable it (gsettings failed): enable it in the session with `gnome-extensions enable {}`",
            gnome::UUID,
            gnome::UUID
        )));
    };
    if let Some(new) = with_extension(&list, gnome::UUID) {
        run("gsettings", &["set", "org.gnome.shell", "enabled-extensions", &new])?;
    }
    let running = gnome::loaded_api().is_some();
    Ok(Some(match (running, changed) {
        (true, false) => format!("the GNOME Shell extension {} is in place and running", gnome::UUID),
        (true, true) => format!("updated the GNOME Shell extension {}: the shell runs the new one after the user next signs in", gnome::UUID),
        (false, _) => format!(
            "installed and enabled the GNOME Shell extension {} (screen, input and windows on Wayland): GNOME Shell loads it when the user next signs in",
            gnome::UUID
        ),
    }))
}

/// GNOME's list of enabled extensions (as `gsettings get` prints it) with one more; None when it is there already.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn with_extension(list: &str, uuid: &str) -> Option<String> {
    if list.contains(&format!("'{uuid}'")) {
        return None;
    }
    let names = list.trim().trim_start_matches("@as").trim().trim_start_matches('[').trim_end_matches(']').trim();
    Some(if names.is_empty() { format!("['{uuid}']") } else { format!("[{names}, '{uuid}']") })
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
        let folder = crate::desktop::gnome::folder();
        if folder.exists() && std::fs::remove_dir_all(&folder).is_ok() {
            let uuid = crate::desktop::gnome::UUID;
            if let Ok(output) = Command::new("gsettings").args(["get", "org.gnome.shell", "enabled-extensions"]).output() {
                let list = String::from_utf8_lossy(&output.stdout).replace(&format!(", '{uuid}'"), "").replace(&format!("'{uuid}', "), "").replace(&format!("'{uuid}'"), "");
                let _ = run("gsettings", &["set", "org.gnome.shell", "enabled-extensions", list.trim()]);
            }
            done.push(format!("removed the GNOME Shell extension {uuid} (the shell lets go of it when the user next signs in)"));
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
    fn linux_set_up_installs_what_is_missing_and_the_sudo_rule() {
        assert_eq!(root_script(&[], Some(0), None), None, "nothing to do");
        let apt = root_script(&[&ACCESSIBILITY_BUS, &GSETTINGS], Some(0), None).unwrap();
        assert!(apt.contains("apt-get update") && apt.contains("apt-get install -y at-spi2-core libglib2.0-bin"), "{apt}");
        let dnf = root_script(&[&SUDO, &GSETTINGS], Some(1), Some("herb")).unwrap();
        assert!(dnf.contains("dnf install -y sudo glib2"), "{dnf}");
        assert!(dnf.contains("'herb ALL=(ALL) NOPASSWD: ALL' > /etc/sudoers.d/master-control.new"), "{dnf}");
        let (check, place) = (dnf.find("visudo -cqf").unwrap(), dnf.find("mv /etc/sudoers.d/master-control.new").unwrap());
        assert!(check < place && dnf.starts_with("set -e"), "the rule is checked before it is put in place: {dnf}");
        let rule_only = root_script(&[&SUDO], None, Some("herb")).unwrap();
        assert!(!rule_only.contains("install"), "no package manager: only the rule: {rule_only}");
        assert!(plain_user_name("herb") && plain_user_name("first.last") && !plain_user_name("a b") && !plain_user_name("x'y") && !plain_user_name("-r"));
    }

    #[test]
    fn an_extension_is_added_to_gnomes_list_once() {
        assert_eq!(with_extension("@as []", "a@b").as_deref(), Some("['a@b']"));
        assert_eq!(with_extension("['ding@x', 'dock@y']\n", "a@b").as_deref(), Some("['ding@x', 'dock@y', 'a@b']"));
        assert_eq!(with_extension("['ding@x', 'a@b']", "a@b"), None);
    }

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
