//! The AI agents on this machine, and their access to Master Control (specs.md §6).
//!
//! Every agent that speaks MCP keeps a list of the servers it starts, each in its own file and its own shape. Start
//! adds Master Control to the list of every agent found here; Cleanup takes it out again. An agent counts as found
//! when its settings folder exists, its app is installed, or its command is on this machine — whether or not this
//! program was itself started with a search path that includes it (an app opened from the desktop has a short one).
//! Nothing else in those files is touched, and the file as it was is kept beside it the first time it is changed.

use std::path::{Path, PathBuf};
use std::process::Command;

use serde_json::{json, Map, Value};

/// How to connect to Master Control and use it well, written for the agents themselves. It travels inside the
/// program: `mctl guide` prints it, the MCP tool `guide` returns it, and Start leaves a copy beside the program.
pub const GUIDE: &str = include_str!("../../AGENT-GUIDE.md");
pub const GUIDE_FILE: &str = "AGENT-GUIDE.md";

/// The name Master Control goes by in every agent's list.
pub const NAME: &str = "master-control";

/// Where to look. Tests point it at a folder of their own, and then nothing outside that folder counts.
pub struct Places {
    home: PathBuf,
    /// Also look at the programs and apps installed on the machine.
    system: bool,
}

impl Places {
    /// This machine: the user's own folder — or, with `MC_AGENTS_HOME` set, that folder and nothing else.
    pub fn here() -> Places {
        if let Some(folder) = std::env::var_os("MC_AGENTS_HOME").filter(|value| !value.is_empty()) {
            return Places { home: PathBuf::from(folder), system: false };
        }
        let home = std::env::var_os(if cfg!(windows) { "USERPROFILE" } else { "HOME" }).map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."));
        Places { home, system: true }
    }

    /// Where apps keep their settings: `~/Library/Application Support` on a Mac, `%APPDATA%` on Windows, `~/.config`
    /// elsewhere.
    fn app_data(&self, app: &str) -> PathBuf {
        if cfg!(target_os = "macos") {
            self.home.join("Library").join("Application Support").join(app)
        } else if cfg!(windows) {
            let roaming = std::env::var_os("APPDATA").filter(|_| self.system).map(PathBuf::from).unwrap_or_else(|| self.home.join("AppData").join("Roaming"));
            roaming.join(app)
        } else {
            self.home.join(".config").join(app)
        }
    }

    /// A command, wherever such commands are usually put: the search path first, then the folders installers use,
    /// which a program started from the desktop doesn't have on its path.
    fn command(&self, name: &str) -> Option<PathBuf> {
        if !self.system {
            return None;
        }
        let home = &self.home;
        let mut folders: Vec<PathBuf> = std::env::var_os("PATH").map(|path| std::env::split_paths(&path).collect()).unwrap_or_default();
        folders.extend([home.join(".local").join("bin"), home.join("bin"), home.join(".npm-global").join("bin"), home.join(".bun").join("bin"), home.join(".cargo").join("bin"), home.join(".claude").join("local")]);
        if cfg!(windows) {
            folders.extend(std::env::var_os("APPDATA").map(|roaming| PathBuf::from(roaming).join("npm")));
        } else {
            folders.extend(["/opt/homebrew/bin", "/usr/local/bin", "/usr/bin"].map(PathBuf::from));
        }
        let endings: &[&str] = if cfg!(windows) { &[".exe", ".cmd", ".bat"] } else { &[""] };
        folders.iter().flat_map(|folder| endings.iter().map(move |ending| folder.join(format!("{name}{ending}")))).find(|path| path.is_file())
    }

    fn app(&self, name: &str) -> Option<PathBuf> {
        if !self.system || !cfg!(target_os = "macos") {
            return None;
        }
        [PathBuf::from("/Applications"), self.home.join("Applications")].into_iter().map(|folder| folder.join(format!("{name}.app"))).find(|path| path.exists())
    }

    /// A path as people write it: with `~` for the user's own folder.
    fn shown(&self, path: &Path) -> String {
        match path.strip_prefix(&self.home) {
            Ok(rest) => format!("~{}{}", std::path::MAIN_SEPARATOR, rest.display()),
            Err(_) => path.display().to_string(),
        }
    }
}

/// Where an agent keeps its list of servers.
enum List {
    /// A JSON file with the servers under `key`. `typed`: each entry says `"type": "stdio"`.
    Json { path: PathBuf, key: &'static str, typed: bool },
    /// Codex's `config.toml`, with a `[mcp_servers.NAME]` table per server.
    Toml { path: PathBuf },
}

impl List {
    fn path(&self) -> &Path {
        match self {
            List::Json { path, .. } | List::Toml { path } => path,
        }
    }
}

struct Agent {
    name: &'static str,
    /// Why it counts as being on this machine, when it is.
    found: Option<String>,
    lists: Vec<List>,
    /// Claude Code's own command, which registers a server for it when it can be run.
    command: Option<PathBuf>,
}

/// Every agent this version knows, found here or not.
fn agents(places: &Places) -> Vec<Agent> {
    let home = &places.home;
    let found = |folders: &[PathBuf], apps: &[&str], commands: &[&str]| -> Option<String> {
        folders
            .iter()
            .find(|folder| folder.exists())
            .map(|folder| format!("its settings are at {}", places.shown(folder)))
            .or_else(|| apps.iter().find_map(|app| places.app(app)).map(|app| format!("the app is at {}", app.display())))
            .or_else(|| commands.iter().find_map(|command| places.command(command)).map(|command| format!("its command is at {}", places.shown(&command))))
    };
    let json = |path: PathBuf| List::Json { path, key: "mcpServers", typed: false };
    let gemini = home.join(".gemini");
    // Antigravity has moved its list between versions: every place that is in use gets the entry, and when none
    // is, the one its documentation names.
    let mut antigravity: Vec<List> = [gemini.join("config"), gemini.join("antigravity"), gemini.join("antigravity-cli")].into_iter().filter(|folder| folder.exists()).map(|folder| json(folder.join("mcp_config.json"))).collect();
    if antigravity.is_empty() {
        antigravity.push(json(gemini.join("config").join("mcp_config.json")));
    }
    let claude_command = std::env::var_os("MC_CLAUDE_PROGRAM").filter(|value| !value.is_empty()).map(PathBuf::from).or_else(|| places.command("claude"));
    let code_user = places.app_data("Code").join("User");
    vec![
        Agent {
            name: "Claude Code",
            found: claude_command.as_ref().map(|command| format!("its command is at {}", places.shown(command))).or_else(|| found(&[home.join(".claude.json"), home.join(".claude")], &[], &[])),
            lists: vec![List::Json { path: home.join(".claude.json"), key: "mcpServers", typed: true }],
            command: claude_command,
        },
        Agent { name: "Claude app", found: found(&[places.app_data("Claude")], &["Claude"], &[]), lists: vec![json(places.app_data("Claude").join("claude_desktop_config.json"))], command: None },
        Agent { name: "Codex", found: found(&[home.join(".codex")], &["Codex"], &["codex"]), lists: vec![List::Toml { path: home.join(".codex").join("config.toml") }], command: None },
        Agent {
            name: "Antigravity",
            found: found(&[gemini.join("config"), gemini.join("antigravity"), gemini.join("antigravity-cli"), home.join(".antigravity")], &["Antigravity"], &["antigravity", "agy"]),
            lists: antigravity,
            command: None,
        },
        Agent { name: "Gemini CLI", found: found(&[gemini.join("settings.json")], &[], &["gemini"]), lists: vec![json(gemini.join("settings.json"))], command: None },
        Agent { name: "Cursor", found: found(&[home.join(".cursor")], &["Cursor"], &["cursor-agent"]), lists: vec![json(home.join(".cursor").join("mcp.json"))], command: None },
        Agent {
            name: "Windsurf",
            found: found(&[home.join(".codeium").join("windsurf")], &["Windsurf"], &[]),
            lists: vec![json(home.join(".codeium").join("windsurf").join("mcp_config.json"))],
            command: None,
        },
        Agent { name: "VS Code", found: found(std::slice::from_ref(&code_user), &[], &[]), lists: vec![List::Json { path: code_user.join("mcp.json"), key: "servers", typed: true }], command: None },
    ]
}

// ------------------------------------------------------------------------------------------------ the files

/// Writes a file whole, so a reader never sees half of it; keeps the file's permissions, and the first time keeps
/// the file as it was beside it.
fn replace(path: &Path, text: &str) -> Result<(), String> {
    let said = |what: &str, error: std::io::Error| format!("{} {what}: {error}", path.display());
    if let Some(folder) = path.parent() {
        std::fs::create_dir_all(folder).map_err(|error| said("couldn't be given a folder", error))?;
    }
    let mut name = path.file_name().unwrap_or_default().to_os_string();
    let kept = path.with_file_name({
        let mut kept = name.clone();
        kept.push(".before-master-control");
        kept
    });
    if path.exists() && !kept.exists() {
        std::fs::copy(path, &kept).map_err(|error| said("couldn't be kept as it was", error))?;
    }
    name.push(".master-control-new");
    let new = path.with_file_name(name);
    std::fs::write(&new, text).map_err(|error| said("couldn't be written", error))?;
    if let Ok(was) = std::fs::metadata(path) {
        let _ = std::fs::set_permissions(&new, was.permissions());
    }
    std::fs::rename(&new, path).map_err(|error| said("couldn't be replaced", error))
}

fn read_json(path: &Path) -> Result<Option<Map<String, Value>>, String> {
    let text = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(format!("{} couldn't be read: {error}", path.display())),
    };
    if text.trim().is_empty() {
        return Ok(Some(Map::new()));
    }
    match serde_json::from_str(&text) {
        Ok(Value::Object(fields)) => Ok(Some(fields)),
        // Left alone: a file with comments in it, or of a shape not known here, is the user's to edit.
        Ok(_) => Err(format!("{} isn't a list of settings", path.display())),
        Err(error) => Err(format!("{} couldn't be understood ({error})", path.display())),
    }
}

/// Puts the entry in a JSON list, or with `None` takes it out. Says whether the file changed.
fn set_json(path: &Path, key: &str, entry: Option<Value>) -> Result<bool, String> {
    let mut fields = match (read_json(path)?, &entry) {
        (Some(fields), _) => fields,
        (None, Some(_)) => Map::new(),
        (None, None) => return Ok(false),
    };
    let before = fields.clone();
    match entry {
        Some(entry) => {
            let servers = fields.entry(key).or_insert_with(|| json!({}));
            let servers = servers.as_object_mut().ok_or_else(|| format!("{}: {key} isn't a list of servers", path.display()))?;
            servers.insert(NAME.to_string(), entry);
        }
        None => {
            if let Some(servers) = fields.get_mut(key).and_then(Value::as_object_mut) {
                servers.remove(NAME);
            }
        }
    }
    if fields == before {
        return Ok(false);
    }
    let mut text = serde_json::to_string_pretty(&Value::Object(fields)).expect("plain data");
    text.push('\n');
    replace(path, &text).map(|()| true)
}

fn in_json(path: &Path, key: &str) -> bool {
    read_json(path).ok().flatten().is_some_and(|fields| fields.get(key).and_then(|servers| servers.get(NAME)).is_some())
}

/// A table's first line that is Master Control's, or one of its own sub-tables (`[mcp_servers.master-control.env]`).
fn is_ours(line: &str) -> bool {
    let header = line.trim();
    [format!("[mcp_servers.{NAME}"), format!("[mcp_servers.\"{NAME}\""), format!("[mcp_servers.'{NAME}'")].iter().any(|start| header.strip_prefix(start.as_str()).is_some_and(|rest| rest.starts_with(']') || rest.starts_with('.')))
}

/// Codex's settings without Master Control's table, and with it again at the end when a program is given.
fn toml_with(text: &str, program: Option<&Path>) -> String {
    let mut kept: Vec<&str> = Vec::new();
    let mut skipping = false;
    for line in text.lines() {
        if line.trim_start().starts_with('[') {
            skipping = is_ours(line);
        }
        if !skipping {
            kept.push(line);
        }
    }
    while kept.last().is_some_and(|line| line.trim().is_empty()) {
        kept.pop();
    }
    let mut text = kept.join("\n");
    if !text.is_empty() {
        text.push('\n');
    }
    if let Some(program) = program {
        let program = program.to_string_lossy();
        // In single quotes a path is taken as it is, backslashes and all; only one that has such a quote in it
        // needs the other kind.
        let quoted = if program.contains('\'') { format!("\"{}\"", program.replace('\\', "\\\\").replace('"', "\\\"")) } else { format!("'{program}'") };
        if !text.is_empty() {
            text.push('\n');
        }
        text.push_str(&format!("[mcp_servers.{NAME}]\ncommand = {quoted}\nargs = [\"mcp\"]\n"));
    }
    text
}

fn set_toml(path: &Path, program: Option<&Path>) -> Result<bool, String> {
    let before = match std::fs::read_to_string(path) {
        Ok(text) => text,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            if program.is_none() {
                return Ok(false);
            }
            String::new()
        }
        Err(error) => return Err(format!("{} couldn't be read: {error}", path.display())),
    };
    let after = toml_with(&before, program);
    if after == before {
        return Ok(false);
    }
    replace(path, &after).map(|()| true)
}

// ------------------------------------------------------------------------------------------------ what Start and Cleanup do

/// One agent found on this machine.
pub struct Found {
    pub name: &'static str,
    /// Why it counts as being here.
    pub how: String,
    /// Whether Master Control is in its list of servers.
    pub registered: bool,
}

/// The agents on this machine, and whether each has Master Control.
pub fn found(places: &Places) -> Vec<Found> {
    agents(places)
        .into_iter()
        .filter_map(|agent| {
            let registered = agent.lists.iter().any(|list| match list {
                List::Json { path, key, .. } => in_json(path, key),
                List::Toml { path } => std::fs::read_to_string(path).is_ok_and(|text| text.lines().any(is_ours)),
            });
            Some(Found { name: agent.name, how: agent.found?, registered })
        })
        .collect()
}

/// Whether an app of this name is running now: on a Mac its program inside the app bundle, elsewhere a process
/// of that name.
fn is_running(name: &str) -> bool {
    let found = if cfg!(windows) {
        Command::new("tasklist").args(["/FI", &format!("IMAGENAME eq {name}.exe"), "/NH"]).output().map(|output| String::from_utf8_lossy(&output.stdout).contains(&format!("{name}.exe")))
    } else {
        let inside_app = format!("/{name}.app/Contents/MacOS/{name}");
        Command::new("ps").args(["-axo", "comm="]).output().map(|output| String::from_utf8_lossy(&output.stdout).lines().any(|line| line.trim() == name || line.trim().ends_with(&inside_app)))
    };
    found.unwrap_or(false)
}

fn ran(program: &Path, args: &[&str]) -> bool {
    Command::new(program).args(args).stdin(std::process::Stdio::null()).output().is_ok_and(|output| output.status.success())
}

/// Gives every agent found here Master Control, run as `program mcp`. Returns what it did, line by line.
pub fn register(places: &Places, program: &Path) -> Vec<String> {
    let mut done = Vec::new();
    let mut absent = Vec::new();
    for agent in agents(places) {
        if agent.found.is_none() {
            absent.push(agent.name);
            continue;
        }
        // Claude Code's own command does it best, where it can be run: it knows its file's every rule.
        if let Some(command) = &agent.command {
            // Taken out first, so an entry that points somewhere else is replaced.
            ran(command, &["mcp", "remove", NAME, "-s", "user"]);
            if ran(command, &["mcp", "add", "-s", "user", NAME, "--", &program.to_string_lossy(), "mcp"]) {
                done.push(format!("{}: registered (through {})", agent.name, places.shown(command)));
                continue;
            }
        }
        for list in &agent.lists {
            let outcome = match list {
                List::Json { path, key, typed } => {
                    let mut entry = json!({ "command": program.to_string_lossy(), "args": ["mcp"] });
                    if *typed {
                        entry = json!({ "type": "stdio", "command": program.to_string_lossy(), "args": ["mcp"], "env": {} });
                    }
                    set_json(path, key, Some(entry))
                }
                List::Toml { path } => set_toml(path, Some(program)),
            };
            // The Claude app keeps its settings in memory while it is open and writes them back, without what was
            // added behind its back (seen 2026-11-11: the entry was gone half an hour later).
            if outcome.is_ok() && agent.name == "Claude app" && places.system && is_running("Claude") {
                done.push("Claude app: it is open, and will write its settings back without Master Control. Quit the Claude app, press Start again, then open it.".into());
                continue;
            }
            done.push(match outcome {
                Ok(_) => format!("{}: registered in {}", agent.name, places.shown(list.path())),
                Err(why) => format!("{}: could NOT be registered: {why}. Add it there by hand: the command is {} with the one argument mcp", agent.name, program.display()),
            });
        }
    }
    if !done.is_empty() {
        done.push("An agent that is open now sees the tools after it is started again.".into());
    }
    if !absent.is_empty() {
        done.push(format!("Not found on this machine: {}.", absent.join(", ")));
    }
    done.push(format!("Any other agent that speaks MCP: add a server that runs {} with the one argument mcp.", program.display()));
    done.push(format!("A guide for the agents themselves is at {} (and from `mctl guide`, and the `guide` tool).", program.with_file_name(GUIDE_FILE).display()));
    done
}

/// Takes Master Control out of every agent's list, found or not: what Start put there, and nothing else.
pub fn unregister(places: &Places) -> Vec<String> {
    let mut done = Vec::new();
    for agent in agents(places) {
        let mut removed = agent.command.as_ref().is_some_and(|command| ran(command, &["mcp", "remove", NAME, "-s", "user"]));
        for list in &agent.lists {
            let outcome = match list {
                List::Json { path, key, .. } => set_json(path, key, None),
                List::Toml { path } => set_toml(path, None),
            };
            match outcome {
                Ok(changed) => removed |= changed,
                Err(why) => done.push(format!("{}: could NOT be taken out: {why}", agent.name)),
            }
        }
        if removed {
            done.push(format!("{}: the master-control MCP server is removed", agent.name));
        }
    }
    done
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn codex_settings_gain_and_lose_only_the_one_table() {
        let before = "model = \"gpt\"\n\n[mcp_servers.node_repl]\nargs = []\ncommand = \"/x/node_repl\"\n\n[mcp_servers.node_repl.env]\nA = \"1\"\n";
        let with = toml_with(before, Some(Path::new("/opt/mc/mctl")));
        assert_eq!(with, format!("{before}\n[mcp_servers.master-control]\ncommand = '/opt/mc/mctl'\nargs = [\"mcp\"]\n"));
        assert_eq!(toml_with(&with, Some(Path::new("/opt/mc/mctl"))), with, "again changes nothing");
        assert_eq!(toml_with(&with, None), before, "and out again leaves what was there");
        // An entry someone wrote by hand, with a table of its own under it and another server after it.
        let by_hand = "[mcp_servers.\"master-control\"]\ncommand = \"old\"\n[mcp_servers.\"master-control\".env]\nX = \"1\"\n[mcp_servers.master-control-two]\ncommand = \"other\"\n";
        assert_eq!(toml_with(by_hand, None), "[mcp_servers.master-control-two]\ncommand = \"other\"\n", "a server whose name only starts the same is someone else's");
        assert_eq!(toml_with("", Some(Path::new("C:\\Program Files\\mc\\mctl.exe"))), "[mcp_servers.master-control]\ncommand = 'C:\\Program Files\\mc\\mctl.exe'\nargs = [\"mcp\"]\n");
        assert!(toml_with("", Some(Path::new("/it's/mctl"))).contains("command = \"/it's/mctl\""));
    }

    #[test]
    fn this_very_test_is_a_running_program_and_a_made_up_one_is_not() {
        let own = std::env::current_exe().unwrap();
        let name = own.file_stem().unwrap().to_str().unwrap();
        // ps cuts long names short on some systems; a name it shows whole is enough to know the look works.
        if name.len() <= 15 {
            assert!(is_running(name));
        }
        assert!(!is_running("no-such-program-anywhere-9431"));
    }
}
