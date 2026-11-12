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
    } else {
        "none"
    }
}
