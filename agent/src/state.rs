//! The agent's folder: where it is, the token in it, and the file that tells clients which port to use.

use std::fs;
use std::io::{self, Write};
use std::path::{Path, PathBuf};

use serde_json::json;

/// `MC_HOME` if set; otherwise the platform's place for an app's own data.
pub fn default_home() -> PathBuf {
    if let Some(home) = std::env::var_os("MC_HOME").filter(|value| !value.is_empty()) {
        return PathBuf::from(home);
    }
    #[cfg(windows)]
    {
        let base = std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| user_home().join("AppData").join("Local"));
        base.join("Master Control")
    }
    #[cfg(target_os = "macos")]
    {
        user_home().join("Library").join("Application Support").join("Master Control")
    }
    #[cfg(all(unix, not(target_os = "macos")))]
    {
        let base = std::env::var_os("XDG_DATA_HOME")
            .filter(|value| !value.is_empty())
            .map(PathBuf::from)
            .unwrap_or_else(|| user_home().join(".local").join("share"));
        base.join("master-control")
    }
}

pub fn user_home() -> PathBuf {
    let name = if cfg!(windows) { "USERPROFILE" } else { "HOME" };
    std::env::var_os(name).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

/// Makes a folder or file readable by its owner only. On Windows the folder sits in the user's profile, which other
/// users can't read.
pub fn restrict_to_user(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = if path.is_dir() { 0o700 } else { 0o600 };
        fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    }
    #[cfg(not(unix))]
    let _ = path;
    Ok(())
}

/// The token every request must carry. Made once and kept, so clients survive an agent restart.
pub fn load_or_create_token(home: &Path) -> io::Result<String> {
    let path = home.join("token");
    if let Ok(text) = fs::read_to_string(&path) {
        let token = text.trim();
        if token.len() >= 32 && token.bytes().all(|byte| byte.is_ascii_hexdigit()) {
            restrict_to_user(&path)?;
            return Ok(token.to_string());
        }
    }
    let mut bytes = [0u8; 32];
    getrandom::getrandom(&mut bytes).map_err(|error| io::Error::other(format!("no random bytes for the token: {error}")))?;
    let token: String = bytes.iter().map(|byte| format!("{byte:02x}")).collect();
    write_private(&path, token.as_bytes())?;
    Ok(token)
}

/// Writes a file that only the user can read, without a moment where it is readable by others.
pub fn write_private(path: &Path, bytes: &[u8]) -> io::Result<()> {
    let mut options = fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let mut file = options.open(path)?;
    restrict_to_user(path)?;
    file.write_all(bytes)
}

pub fn write_agent_file(home: &Path, port: u16) -> io::Result<()> {
    let text = json!({ "port": port, "pid": std::process::id(), "version": crate::VERSION }).to_string();
    write_private(&home.join("agent.json"), text.as_bytes())
}

/// Turns a path a client sent into a real one: `~` is the user's home, and a relative path starts there too.
pub fn resolve(path: &str) -> PathBuf {
    if path == "~" {
        return user_home();
    }
    if let Some(rest) = path.strip_prefix("~/").or_else(|| path.strip_prefix("~\\")) {
        return user_home().join(rest);
    }
    let path = Path::new(path);
    if path.is_absolute() {
        path.to_path_buf()
    } else {
        user_home().join(path)
    }
}
