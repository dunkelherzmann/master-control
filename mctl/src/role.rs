//! The master's Start and Cleanup (specs.md §6). Start puts the program in its place and gives the AI agents found on
//! this machine their access to it (`agents`); Cleanup takes all of that away again, the pairings included.

use std::io;
use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::Value;

use crate::Master;

/// The folder the installed master lives in: `MC_INSTALL_DIR` if set, otherwise the user's own programs folder.
pub fn install_folder() -> PathBuf {
    if let Some(folder) = std::env::var_os("MC_INSTALL_DIR").filter(|value| !value.is_empty()) {
        return PathBuf::from(folder);
    }
    let user = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
    if cfg!(windows) {
        std::env::var_os("LOCALAPPDATA").map(PathBuf::from).unwrap_or_else(|| user.join("AppData").join("Local")).join("Programs").join("Master Control")
    } else {
        user.join(".local").join("opt").join("master-control")
    }
}

pub fn installed_program() -> PathBuf {
    install_folder().join(if cfg!(windows) { "mctl.exe" } else { "mctl" })
}

/// Puts a copy of this program at `program`: as a new file moved into place, so a copy that is running goes on
/// undisturbed, and on a Mac with a signature of its own (one cut out of an app carries the app's, which no longer
/// fits it).
pub fn place(current: &Path, program: &Path) -> io::Result<()> {
    let failed = |what: &str, error: io::Error| io::Error::other(format!("the program couldn't be {what} {}: {error}", program.display()));
    std::fs::create_dir_all(program.parent().unwrap_or(Path::new("."))).map_err(|error| failed("given its folder for", error))?;
    let mut name = program.file_name().unwrap_or_default().to_os_string();
    name.push(".new");
    let new = program.with_file_name(name);
    std::fs::copy(current, &new).map_err(|error| failed("copied to", error))?;
    if cfg!(target_os = "macos") {
        let _ = Command::new("/usr/bin/codesign").args(["--force", "--sign", "-"]).arg(&new).output();
    }
    std::fs::rename(&new, program).map_err(|error| failed("moved to", error))
}

const FIREWALL_RULE: &str = "Master Control pairing";

/// The command lines that let workers reach the master's pairing window through Windows Firewall, or take that
/// away again. Without the rule, Windows asks the user the first time Pair is opened, with a prompt that takes
/// the keyboard from everything else until it is answered.
pub fn firewall_commands(allow: bool, any_network: bool) -> Vec<Vec<String>> {
    let named = format!("name={FIREWALL_RULE}");
    let mut commands = vec![["advfirewall", "firewall", "delete", "rule", &named].map(str::to_string).to_vec()];
    if allow {
        let port = format!("localport={}", crate::PAIR_PORT);
        let profile = if any_network { "profile=private,public" } else { "profile=private" };
        commands.push(["advfirewall", "firewall", "add", "rule", &named, "dir=in", "action=allow", "protocol=TCP", &port, profile].map(str::to_string).to_vec());
    }
    commands
}

fn firewall(allow: bool, any_network: bool) -> Vec<String> {
    if !cfg!(windows) {
        return Vec::new();
    }
    let mut worked = true;
    for (index, command) in firewall_commands(allow, any_network).iter().enumerate() {
        let done = Command::new("netsh").args(command).output().is_ok_and(|output| output.status.success());
        // Taking out a rule that isn't there fails, which is fine; adding one must work.
        worked &= done || index == 0;
    }
    match (allow, worked) {
        (true, true) => vec![format!("added the firewall rule \"{FIREWALL_RULE}\": TCP port {}, {}", crate::PAIR_PORT, if any_network { "private and public networks" } else { "private networks only" })],
        (true, false) => vec![format!("the firewall rule \"{FIREWALL_RULE}\" could NOT be added (this needs an elevated prompt): Windows will ask when Pair is first opened")],
        (false, _) => Vec::new(),
    }
}

/// Installs what isn't installed yet. Safe to run again: it repairs and updates. Returns what it did, line by line.
pub fn start(any_network: bool) -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    let program = installed_program();
    let current = std::env::current_exe()?;
    if current != program {
        place(&current, &program)?;
        done.push(format!("the program is at {}", program.display()));
    }
    done.extend(firewall(true, any_network));
    // The guide for the agents, beside the program: one that can read files but not run commands finds it there.
    std::fs::write(program.with_file_name(crate::agents::GUIDE_FILE), crate::agents::GUIDE)?;
    done.extend(crate::agents::register(&crate::agents::Places::here(), &program));
    Ok(done)
}

/// Removes everything Start installed and everything pairing left: the workers are told to forget this master, the
/// AI agents lose their access, and the master's folder and program are deleted.
pub fn cleanup(master: Master) -> io::Result<Vec<String>> {
    let mut done = Vec::new();
    for machine in master.machines() {
        match master.call(&machine, "forget_master", &Value::Null) {
            Ok(_) => done.push(format!("{} has forgotten this master", machine.name)),
            Err(failure) => done.push(format!("{} could NOT be told to forget this master ({}); on that machine, run its Cleanup", machine.name, failure.message)),
        }
    }
    done.extend(crate::agents::unregister(&crate::agents::Places::here()));
    done.extend(firewall(false, false));
    let home = master.home().to_path_buf();
    drop(master);
    if std::fs::remove_dir_all(&home).is_ok() {
        done.push(format!("removed {}", home.display()));
    }
    let program = installed_program();
    let _ = std::fs::remove_file(program.with_file_name(crate::agents::GUIDE_FILE));
    if std::env::current_exe().ok().as_ref() != Some(&program) && std::fs::remove_file(&program).is_ok() {
        let _ = std::fs::remove_dir(install_folder());
        done.push(format!("removed {}", program.display()));
    }
    Ok(done)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_pairing_rule_is_for_the_pairing_port_and_the_networks_asked_for() {
        let private = firewall_commands(true, false);
        assert_eq!(private[0], ["advfirewall", "firewall", "delete", "rule", "name=Master Control pairing"], "taken out first, so it is never doubled");
        assert_eq!(private[1], ["advfirewall", "firewall", "add", "rule", "name=Master Control pairing", "dir=in", "action=allow", "protocol=TCP", &format!("localport={}", crate::PAIR_PORT), "profile=private"]);
        assert_eq!(firewall_commands(true, true)[1].last().unwrap(), "profile=private,public");
        assert_eq!(firewall_commands(false, true).len(), 1, "Cleanup only takes it out");
    }
}
