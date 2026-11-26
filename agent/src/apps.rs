//! The apps a person can start from the system's own launcher, by the names the launcher shows (specs.md §3.1):
//! the Start menu on Windows, the Applications folders on macOS, the desktop entries on Linux.

#[cfg(target_os = "linux")]
use std::path::Path;
#[cfg(not(windows))]
use std::path::PathBuf;
use std::process::Command;

use crate::tools::ToolError;

/// An app as the launcher lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct App {
    /// What the launcher shows, in the system's language.
    pub name: String,
    /// What starts it: the Start menu's AppID on Windows, the bundle's path on macOS, the desktop entry's file on
    /// Linux.
    pub id: String,
}

/// Every app the launcher offers, sorted by name.
pub fn list() -> Result<Vec<App>, ToolError> {
    let mut apps = listed()?;
    apps.sort_by_key(|app| app.name.to_lowercase());
    apps.dedup_by(|one, other| one.id == other.id);
    Ok(apps)
}

/// The one app a name means: the app of exactly that name (in any case), or the only one whose name contains it, or
/// the one with that id.
pub fn choose<'a>(apps: &'a [App], wanted: &str) -> Result<&'a App, ToolError> {
    let lower = wanted.to_lowercase();
    if let Some(app) = apps.iter().find(|app| app.id == wanted || app.name.to_lowercase() == lower) {
        return Ok(app);
    }
    let partial: Vec<&App> = apps.iter().filter(|app| app.name.to_lowercase().contains(&lower)).collect();
    match partial.as_slice() {
        [] => Err(ToolError::not_found(format!("no app the launcher lists is called \"{wanted}\"; `apps` lists them"))),
        [one] => Ok(one),
        several => Err(ToolError::bad(format!(
            "\"{wanted}\" matches several apps: {}; give the whole name or the id",
            several.iter().take(10).map(|app| format!("\"{}\"", app.name)).collect::<Vec<_>>().join(", ")
        ))),
    }
}

/// Starts the app the way its launcher would, without waiting for it.
pub fn start(app: &App) -> Result<(), ToolError> {
    let mut command = starter(app);
    let status = command.status().map_err(|error| ToolError::failed(format!("the app couldn't be started: {error}")))?;
    // explorer.exe answers 1 even when it did start the app; whether it did shows in its window.
    if !status.success() && !cfg!(windows) {
        return Err(ToolError::failed(format!("the launcher refused to start \"{}\" ({status})", app.name)));
    }
    Ok(())
}

#[cfg(windows)]
fn listed() -> Result<Vec<App>, ToolError> {
    use std::os::windows::process::CommandExt;
    // The Start menu's own list: desktop programs and Store apps alike, by the names the Start menu shows.
    let output = Command::new("powershell")
        .args(["-NoProfile", "-NonInteractive", "-Command", "[Console]::OutputEncoding=[Text.Encoding]::UTF8; Get-StartApps | ConvertTo-Json -Compress"])
        .creation_flags(0x0800_0000)
        .output()
        .map_err(|error| ToolError::failed(format!("the Start menu couldn't be read: {error}")))?;
    let text = String::from_utf8_lossy(&output.stdout);
    let value: serde_json::Value = serde_json::from_str(text.trim()).map_err(|error| ToolError::failed(format!("the Start menu's list couldn't be read: {error}")))?;
    let entries = match value {
        serde_json::Value::Array(entries) => entries,
        one => vec![one],
    };
    Ok(entries
        .iter()
        .filter_map(|entry| Some(App { name: entry["Name"].as_str()?.to_string(), id: entry["AppID"].as_str()?.to_string() }))
        .collect())
}

#[cfg(windows)]
fn starter(app: &App) -> Command {
    // Through the shell, as the Start menu does: the app starts in the user's own session and rights, not the
    // agent's.
    let mut command = Command::new("explorer.exe");
    command.arg(format!("shell:AppsFolder\\{}", app.id));
    command
}

#[cfg(target_os = "macos")]
fn listed() -> Result<Vec<App>, ToolError> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let folders = [
        PathBuf::from("/Applications"),
        PathBuf::from("/Applications/Utilities"),
        PathBuf::from("/System/Applications"),
        PathBuf::from("/System/Applications/Utilities"),
        home.join("Applications"),
    ];
    let mut apps = Vec::new();
    for folder in folders.iter().chain(extra_folders().iter()) {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "app") {
                let name = path.file_stem().map(|stem| stem.to_string_lossy().into_owned()).unwrap_or_default();
                apps.push(App { name, id: path.to_string_lossy().into_owned() });
            }
        }
    }
    Ok(apps)
}

#[cfg(target_os = "macos")]
fn starter(app: &App) -> Command {
    let mut command = Command::new("/usr/bin/open");
    command.args(["-a", &app.id]);
    command
}

#[cfg(target_os = "linux")]
fn listed() -> Result<Vec<App>, ToolError> {
    let mut apps = Vec::new();
    for folder in application_folders().iter().chain(extra_folders().iter()) {
        let Ok(entries) = std::fs::read_dir(folder) else { continue };
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|extension| extension == "desktop") {
                if let Some(name) = std::fs::read_to_string(&path).ok().and_then(|text| desktop_entry_name(&text)) {
                    apps.push(App { name, id: path.to_string_lossy().into_owned() });
                }
            }
        }
    }
    // An entry in the user's own folder hides one of the same file name in the system's.
    let mut seen = std::collections::HashSet::new();
    apps.retain(|app| seen.insert(Path::new(&app.id).file_name().map(|name| name.to_owned())));
    Ok(apps)
}

/// The folders desktop entries are in, the user's own first (the XDG base directory rules), with Flatpak's and
/// Snap's.
#[cfg(target_os = "linux")]
fn application_folders() -> Vec<PathBuf> {
    let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
    let data_home = std::env::var_os("XDG_DATA_HOME").filter(|value| !value.is_empty()).map(PathBuf::from).unwrap_or_else(|| home.join(".local/share"));
    let data_dirs = std::env::var("XDG_DATA_DIRS").ok().filter(|value| !value.is_empty()).unwrap_or_else(|| "/usr/local/share:/usr/share".into());
    let mut folders = vec![data_home.join("applications")];
    folders.extend(data_dirs.split(':').filter(|dir| !dir.is_empty()).map(|dir| Path::new(dir).join("applications")));
    folders.extend([home.join(".local/share/flatpak/exports/share/applications"), PathBuf::from("/var/lib/flatpak/exports/share/applications"), PathBuf::from("/var/lib/snapd/desktop/applications")]);
    folders
}

/// The name a desktop entry shows, when it is an app the launcher offers.
#[cfg_attr(not(target_os = "linux"), allow(dead_code))]
fn desktop_entry_name(text: &str) -> Option<String> {
    let mut in_entry = false;
    let (mut name, mut application, mut shown) = (None, false, true);
    for line in text.lines().map(str::trim) {
        if line.starts_with('[') {
            in_entry = line == "[Desktop Entry]";
            continue;
        }
        if !in_entry {
            continue;
        }
        match line.split_once('=').map(|(key, value)| (key.trim(), value.trim())) {
            Some(("Name", value)) => name = Some(value.to_string()),
            Some(("Type", value)) => application = value == "Application",
            Some(("NoDisplay" | "Hidden", "true")) => shown = false,
            _ => {}
        }
    }
    name.filter(|name| !name.is_empty() && application && shown)
}

#[cfg(target_os = "linux")]
fn starter(app: &App) -> Command {
    // GLib's launcher reads the entry as the desktop does: its Exec line, field codes and all.
    let mut command = Command::new("gio");
    command.args(["launch", &app.id]);
    command
}

/// More folders to look in, from `MC_APPS_FOLDERS` (separated as PATH is): for tests, which add apps of their own.
#[cfg_attr(windows, allow(dead_code))]
fn extra_folders() -> Vec<PathBuf> {
    std::env::var_os("MC_APPS_FOLDERS").map(|value| std::env::split_paths(&value).collect()).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app(name: &str, id: &str) -> App {
        App { name: name.into(), id: id.into() }
    }

    #[test]
    fn a_name_means_one_app() {
        let apps = [app("Calculator", "calc"), app("Text Editor", "gedit"), app("Text Viewer", "viewer"), app("Terminal", "term")];
        assert_eq!(choose(&apps, "calculator").unwrap().id, "calc", "exact, in any case");
        assert_eq!(choose(&apps, "calc").unwrap().id, "calc", "an id");
        assert_eq!(choose(&apps, "Calcul").unwrap().id, "calc", "the only one that contains it");
        assert_eq!(choose(&apps, "Text").unwrap_err().code, "bad_input", "two contain it");
        assert_eq!(choose(&apps, "Paint").unwrap_err().code, "not_found");
        let exact_wins = [app("Mail", "mail"), app("Mail Merge", "merge")];
        assert_eq!(choose(&exact_wins, "mail").unwrap().id, "mail");
    }

    #[test]
    fn desktop_entries_are_read_as_the_launcher_reads_them() {
        assert_eq!(desktop_entry_name("[Desktop Entry]\nType=Application\nName=Files\nExec=nautilus\n"), Some("Files".into()));
        assert_eq!(desktop_entry_name("[Desktop Entry]\nType=Application\nName=Hidden\nNoDisplay=true\n"), None);
        assert_eq!(desktop_entry_name("[Desktop Entry]\nType=Link\nName=A link\n"), None);
        // Only the entry's own group counts: an action's name is not the app's.
        assert_eq!(desktop_entry_name("[Desktop Entry]\nType=Application\nName=Web\n[Desktop Action new]\nName=New Window\n"), Some("Web".into()));
        assert_eq!(desktop_entry_name("[Desktop Entry]\nType=Application\n"), None);
    }
}
