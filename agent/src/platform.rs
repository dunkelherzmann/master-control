//! The few things each operating system does its own way.

use std::process::Command;

/// A command line run by the system's shell: `sh -c` or `cmd /C`.
pub fn shell(command_line: &str) -> Command {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let mut command = Command::new("cmd");
        // /S with the whole line in one pair of quotes: cmd strips that pair and leaves the rest as written.
        command.raw_arg("/D /S /C").raw_arg(format!("\"{command_line}\""));
        command
    }
    #[cfg(not(windows))]
    {
        let mut command = Command::new("sh");
        command.arg("-c").arg(command_line);
        command
    }
}

/// On Linux, when the agent runs as a systemd user service: the command run in a scope of its own
/// (`systemd-run --user --scope`, which keeps the process id), so systemd doesn't stop what it started when it stops
/// or restarts the agent's service (it stops everything in a service's cgroup, `setsid` or not). Elsewhere, and where
/// systemd can't do it, the command as it is.
pub fn outside_the_agents_service(command: Command) -> std::io::Result<Command> {
    #[cfg(target_os = "linux")]
    {
        static SYSTEMD_RUN: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
        let systemd_run = SYSTEMD_RUN.get_or_init(|| {
            let cgroup = std::fs::read_to_string("/proc/self/cgroup").unwrap_or_default();
            if !in_a_user_service(&cgroup) {
                return None;
            }
            let program = ["/usr/bin/systemd-run", "/bin/systemd-run"].into_iter().map(std::path::PathBuf::from).find(|path| path.exists())?;
            // `--expand-environment=no` (systemd 254) keeps a `$` in the command as written; without it, no scope.
            let help = Command::new(&program).arg("--help").output().ok()?;
            String::from_utf8_lossy(&help.stdout).contains("--expand-environment").then_some(program)
        });
        if let Some(systemd_run) = systemd_run {
            // A program that isn't there is refused here, as starting it directly would be, not by systemd-run later.
            if !findable(&command) {
                return Err(std::io::Error::new(std::io::ErrorKind::NotFound, format!("{} wasn't found", command.get_program().to_string_lossy())));
            }
            return Ok(in_a_scope(systemd_run, &command));
        }
    }
    Ok(command)
}

/// Whether the command's program is there to run: a path to it, or a name found in the folders of its PATH.
#[cfg(unix)]
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn findable(command: &Command) -> bool {
    use std::os::unix::fs::PermissionsExt;
    let runnable = |path: &std::path::Path| path.metadata().is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0);
    let program = std::path::Path::new(command.get_program());
    if program.components().count() > 1 {
        return runnable(&command.get_current_dir().map_or_else(|| program.to_path_buf(), |dir| dir.join(program)));
    }
    let path = command.get_envs().find(|(name, _)| *name == "PATH").and_then(|(_, value)| value.map(|value| value.to_os_string())).or_else(|| std::env::var_os("PATH")).unwrap_or_default();
    std::env::split_paths(&path).any(|folder| runnable(&folder.join(program)))
}

/// Whether a process's `/proc/self/cgroup` puts it in a service of a user's systemd.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn in_a_user_service(cgroup: &str) -> bool {
    cgroup.lines().any(|line| {
        let path = line.rsplit(':').next().unwrap_or_default();
        let last = path.rsplit('/').next().unwrap_or_default();
        path.contains("/user@") && last.ends_with(".service") && !last.starts_with("user@")
    })
}

/// The same command (program, words, folder and environment), run through `systemd-run` in a scope of its own.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
pub fn in_a_scope(systemd_run: &std::path::Path, command: &Command) -> Command {
    let mut scoped = Command::new(systemd_run);
    scoped.args(["--user", "--scope", "--collect", "--quiet", "--expand-environment=no", "--"]).arg(command.get_program()).args(command.get_args());
    if let Some(folder) = command.get_current_dir() {
        scoped.current_dir(folder);
    }
    for (name, value) in command.get_envs() {
        match value {
            Some(value) => scoped.env(name, value),
            None => scoped.env_remove(name),
        };
    }
    scoped
}

/// Makes the child the head of its own group, so the whole tree it starts can be stopped at once.
pub fn own_group(command: &mut Command) {
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        const CREATE_NEW_PROCESS_GROUP: u32 = 0x0000_0200;
        command.creation_flags(CREATE_NO_WINDOW | CREATE_NEW_PROCESS_GROUP);
    }
}

/// Stops a process started with [`own_group`] and everything it started.
pub fn kill_tree(pid: u32) {
    #[cfg(unix)]
    unsafe {
        // The group's id is the child's pid. SAFETY: kill takes plain integers and touches no memory of ours.
        libc::kill(-(pid as libc::pid_t), libc::SIGKILL);
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        let _ = Command::new("taskkill")
            .args(["/T", "/F", "/PID", &pid.to_string()])
            .creation_flags(0x0800_0000)
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .status();
    }
}

/// The exit code, or on Unix the negative signal number when a signal ended the process.
pub fn exit_code(status: std::process::ExitStatus) -> i64 {
    if let Some(code) = status.code() {
        return i64::from(code);
    }
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return -i64::from(signal);
        }
    }
    -1
}

/// Whether the agent runs with admin or root rights.
pub fn elevated() -> bool {
    #[cfg(unix)]
    {
        // SAFETY: geteuid has no arguments and cannot fail.
        unsafe { libc::geteuid() == 0 }
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        // The high and system integrity levels, as whoami lists them for the current token.
        Command::new("whoami")
            .args(["/groups", "/fo", "csv", "/nh"])
            .creation_flags(0x0800_0000)
            .output()
            .map(|output| {
                let text = String::from_utf8_lossy(&output.stdout);
                text.contains("S-1-16-12288") || text.contains("S-1-16-16384")
            })
            .unwrap_or(false)
    }
}

pub fn hostname() -> String {
    #[cfg(unix)]
    {
        let mut buffer = [0u8; 256];
        // SAFETY: the buffer outlives the call and its real length is passed.
        let ok = unsafe { libc::gethostname(buffer.as_mut_ptr().cast(), buffer.len()) } == 0;
        if ok {
            let end = buffer.iter().position(|&byte| byte == 0).unwrap_or(buffer.len());
            return String::from_utf8_lossy(&buffer[..end]).into_owned();
        }
        String::new()
    }
    #[cfg(windows)]
    {
        std::env::var("COMPUTERNAME").unwrap_or_default()
    }
}

pub fn user() -> String {
    std::env::var(if cfg!(windows) { "USERNAME" } else { "USER" }).unwrap_or_default()
}

/// Whether the user's runtime folder holds a Wayland compositor's socket.
fn wayland_socket() -> bool {
    let folder = std::env::var_os("XDG_RUNTIME_DIR").map(std::path::PathBuf::from);
    folder.and_then(|folder| std::fs::read_dir(folder).ok()).is_some_and(|entries| {
        entries.flatten().any(|entry| entry.file_name().to_string_lossy().starts_with("wayland-") && !entry.file_name().to_string_lossy().ends_with(".lock"))
    })
}

/// What kind of desktop session the agent is in: `x11`, `wayland`, `windows`, `macos`, or `none` when it has no
/// display to work with.
pub fn session() -> &'static str {
    if cfg!(windows) {
        "windows"
    } else if cfg!(target_os = "macos") {
        "macos"
    } else if std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty()) {
        "wayland"
    } else if std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty()) {
        "x11"
    } else if wayland_socket() {
        // A user service started before the desktop has neither variable, and still reaches it over the session bus.
        "wayland"
    } else {
        "none"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_user_service_is_told_from_the_rest() {
        assert!(in_a_user_service("0::/user.slice/user-1000.slice/user@1000.service/app.slice/master-control.service\n"));
        for elsewhere in [
            "0::/user.slice/user-1000.slice/user@1000.service/app.slice/run-p4184745-i4191452.scope",
            "0::/user.slice/user-1000.slice/user@1000.service/init.scope",
            "0::/user.slice/user-1000.slice/user@1000.service",
            "0::/system.slice/ssh.service",
            "0::/user.slice/user-1000.slice/session-2.scope",
            "",
        ] {
            assert!(!in_a_user_service(elsewhere), "{elsewhere}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn a_program_is_found_as_the_system_would() {
        assert!(findable(&Command::new("sh")));
        assert!(findable(&Command::new("/bin/sh")));
        assert!(!findable(&Command::new("no-such-program-mc")));
        let mut elsewhere = Command::new("sh");
        elsewhere.env("PATH", "/nowhere");
        assert!(!findable(&elsewhere));
    }

    #[test]
    fn a_scoped_command_keeps_its_words() {
        let mut command = Command::new("sh");
        command.args(["-c", "echo $HOME"]).current_dir("/tmp").env("MC_X", "1");
        let scoped = in_a_scope(std::path::Path::new("/usr/bin/systemd-run"), &command);
        assert_eq!(scoped.get_program(), "/usr/bin/systemd-run");
        let words: Vec<_> = scoped.get_args().map(|word| word.to_string_lossy().into_owned()).collect();
        assert_eq!(words, ["--user", "--scope", "--collect", "--quiet", "--expand-environment=no", "--", "sh", "-c", "echo $HOME"]);
        assert_eq!(scoped.get_current_dir(), Some(std::path::Path::new("/tmp")));
        assert_eq!(scoped.get_envs().count(), 1);
    }

    /// Run where a user's systemd answers (the Linux machine): the command keeps its process id and its `$`, and runs
    /// in a scope of its own, outside whatever service started it.
    #[cfg(target_os = "linux")]
    #[test]
    fn a_scoped_command_runs_outside_the_service() {
        let reachable = Command::new("systemctl").args(["--user", "is-system-running"]).output().is_ok_and(|out| !out.stdout.is_empty());
        let Some(systemd_run) = ["/usr/bin/systemd-run", "/bin/systemd-run"].into_iter().map(std::path::PathBuf::from).find(|path| path.exists()) else { return };
        if !reachable {
            eprintln!("skipped: no user systemd here");
            return;
        }
        let mut command = Command::new("sh");
        command.args(["-c", "echo $$ $HOME; cat /proc/self/cgroup"]);
        let mut scoped = in_a_scope(&systemd_run, &command);
        scoped.stdout(std::process::Stdio::piped());
        let child = scoped.spawn().unwrap();
        let pid = child.id();
        let out = child.wait_with_output().unwrap();
        let text = String::from_utf8_lossy(&out.stdout);
        let mut lines = text.lines();
        assert_eq!(lines.next().unwrap(), format!("{pid} {}", std::env::var("HOME").unwrap()), "{text}");
        assert!(lines.next().unwrap().ends_with(".scope"), "{text}");
    }
}
