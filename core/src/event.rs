use serde::{Deserialize, Serialize};
use serde_json::Value;

/// The kind of thing an event describes (specs.md §4.2).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Class {
    Process,
    Thread,
    Module,
    Syscall,
    Api,
    File,
    Registry,
    Memory,
    Network,
    Ipc,
    Ui,
    Log,
    Problem,
}

impl Class {
    pub const ALL: [Class; 13] = [
        Class::Process,
        Class::Thread,
        Class::Module,
        Class::Syscall,
        Class::Api,
        Class::File,
        Class::Registry,
        Class::Memory,
        Class::Network,
        Class::Ipc,
        Class::Ui,
        Class::Log,
        Class::Problem,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Class::Process => "process",
            Class::Thread => "thread",
            Class::Module => "module",
            Class::Syscall => "syscall",
            Class::Api => "api",
            Class::File => "file",
            Class::Registry => "registry",
            Class::Memory => "memory",
            Class::Network => "network",
            Class::Ipc => "ipc",
            Class::Ui => "ui",
            Class::Log => "log",
            Class::Problem => "problem",
        }
    }

    pub fn parse(text: &str) -> Option<Class> {
        Class::ALL.into_iter().find(|class| class.as_str() == text)
    }
}

/// One thing the target did, in the same shape whatever source it came from.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Event {
    /// Nanoseconds since the Unix epoch.
    pub time_ns: u64,
    pub pid: u32,
    #[serde(default)]
    pub tid: u32,
    /// The process's name, as the platform shows it.
    #[serde(default)]
    pub process: String,
    pub class: Class,
    /// What was done: `open`, `write`, `connect`, `set_value`, `start`, `exit`...
    pub op: String,
    /// What it was done to: a path, a registry key, an address, an endpoint.
    #[serde(default)]
    pub subject: String,
    /// Decoded arguments and details, by name.
    #[serde(default)]
    pub args: Value,
    /// How it ended: `ok`, an error name or a return value. Empty when the source gives none.
    #[serde(default)]
    pub result: String,
    #[serde(default)]
    pub duration_ns: u64,
    /// The call stack, innermost frame first, when the source has one.
    #[serde(default)]
    pub stack: Vec<String>,
    /// True when a process outside the target's tree did this to something of the target's.
    #[serde(default)]
    pub outside: bool,
}

impl Event {
    pub fn new(time_ns: u64, pid: u32, class: Class, op: &str, subject: &str) -> Event {
        Event {
            time_ns,
            pid,
            tid: 0,
            process: String::new(),
            class,
            op: op.to_string(),
            subject: subject.to_string(),
            args: Value::Null,
            result: String::new(),
            duration_ns: 0,
            stack: Vec::new(),
            outside: false,
        }
    }
}
