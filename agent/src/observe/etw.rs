//! The tracer on Windows: a helper process that starts the target and follows it and everything it starts through
//! Event Tracing for Windows (the kernel's process, file, registry and network providers), writing what they do into
//! the session's event store. Needs admin rights, which starting a kernel trace session requires.
//!
//! The providers report the whole machine; events are dropped here, by process id, before they are decoded.

use std::collections::{HashMap, HashSet};
use std::fs::{self, File};
use std::io;
use std::os::windows::process::CommandExt;
use std::path::Path;
use std::process::{Command, Stdio};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use mc_core::{Class, Event, Store};
use serde_json::{json, Value};
use windows_sys::core::GUID;
use windows_sys::Win32::Foundation::CloseHandle;
use windows_sys::Win32::Storage::FileSystem::QueryDosDeviceW;
use windows_sys::Win32::System::Diagnostics::Etw::{
    CloseTrace, ControlTraceW, EnableTraceEx2, OpenTraceW, ProcessTrace, StartTraceW, TdhGetProperty, TdhGetPropertySize, CONTROLTRACE_HANDLE,
    ENABLE_TRACE_PARAMETERS, EVENT_HEADER_EXTENDED_DATA_ITEM, EVENT_RECORD, EVENT_TRACE_LOGFILEW, EVENT_TRACE_PROPERTIES, PROPERTY_DATA_DESCRIPTOR,
};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{
    CreateToolhelp32Snapshot, Module32FirstW, Module32NextW, Process32FirstW, Process32NextW, MODULEENTRY32W, PROCESSENTRY32W, TH32CS_SNAPMODULE, TH32CS_SNAPMODULE32,
    TH32CS_SNAPPROCESS,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, QueryFullProcessImageNameW, TerminateProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE,
};

use super::{covers, Request};

const PROCESS: GUID = GUID::from_u128(0x22fb2cd6_0e7b_422b_a0c7_2fad1fd0e716);
const FILE: GUID = GUID::from_u128(0xedd08927_9cc4_4e65_b970_c2560fb5c289);
const REGISTRY: GUID = GUID::from_u128(0x70eb4f03_c1de_4f73_a051_33d13d5413bd);
const NETWORK: GUID = GUID::from_u128(0x7dd42a49_5329_4832_8dfd_43d979153a88);
/// The kernel's older event classes, which a session in system-logger mode gets by flags, not by provider. They
/// carry two things the newer providers leave out: a started program's command line, and memory allocations.
const CLASSIC_PROCESS: GUID = GUID::from_u128(0x3d6fa8d0_fe05_11d0_9dda_00c04fd7ba7c);
const CLASSIC_MEMORY: GUID = GUID::from_u128(0x3d6fa8d3_fe05_11d0_9dda_00c04fd7ba7c);
/// Process starts and ends, and virtual memory allocations and frees.
const SYSTEM_FLAGS: u32 = 0x1 | 0x4000;
const EVENT_TRACE_SYSTEM_LOGGER_MODE: u32 = 0x0200_0000;
/// The keywords asked of each provider: what the events below need, and no more (the providers are loud).
const KEYWORDS: [(GUID, u64); 4] = [
    (PROCESS, 0x10),
    // File names, I/O, the end of each operation, create, read, write, delete path, rename path, create new file.
    (FILE, 0x10 | 0x20 | 0x40 | 0x80 | 0x100 | 0x200 | 0x400 | 0x800 | 0x1000),
    // Set value, delete value, create key, open key, delete key.
    (REGISTRY, 0x100 | 0x200 | 0x1000 | 0x2000 | 0x4000),
    (NETWORK, 0x10 | 0x20),
];
const EVENT_TRACE_REAL_TIME_MODE: u32 = 0x100;
const PROCESS_TRACE_MODE_REAL_TIME_AND_EVENT_RECORD: u32 = 0x100 | 0x1000_0000;
const WNODE_FLAG_TRACED_GUID: u32 = 0x0002_0000;
const CONTROL_STOP: u32 = 1;
const CONTROL_FLUSH: u32 = 3;

/// A file operation that has started. Windows reports how it ended (its status, and how much was read or written)
/// in a later event with the same request pointer; the event is held until then.
struct Waiting {
    event: Event,
    /// What the operation changes if it succeeds, as (operation, path), for the guards.
    changes: Vec<(&'static str, String)>,
}

struct Tracer {
    store: Store,
    pending: Vec<Event>,
    request: Request,
    /// The target and everything it started that is still running.
    live: HashSet<u32>,
    /// Every process that was ever part of the session, with its program's name.
    names: HashMap<u32, String>,
    /// Open file objects and registry key objects of the target, by the kernel's pointer for them.
    files: HashMap<u64, String>,
    keys: HashMap<u64, String>,
    drives: Vec<(String, String)>,
    user_hive: String,
    reported: HashSet<(String, String)>,
    written: u64,
    last_event: Instant,
    awaiting: HashMap<u64, Waiting>,
    /// Command lines of programs started during the session, and programs whose command line hasn't arrived yet
    /// (the two come from different events, in either order).
    command_lines: HashMap<u32, String>,
    unnamed: HashMap<u32, (u64, String)>,
    /// With stacks asked for: what each process has loaded (where, how long, which file), and the stack of the
    /// event being handled, named, with the process it belongs to.
    modules: HashMap<u32, Vec<(u64, u64, String)>>,
    images: super::symbols::Images,
    stack: Vec<String>,
    stack_of: u32,
    /// Stacks whose user-mode half Windows sent apart from the event.
    split_stacks: u64,
}

/// The one tracer of this process, reached from the callback Windows calls for each event.
static TRACER: Mutex<Option<Tracer>> = Mutex::new(None);

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain([0]).collect()
}

/// One named field of an event, as raw bytes. TDH finds it by the provider's own description of the event, so this
/// keeps working when a Windows version adds fields.
fn field(record: *const EVENT_RECORD, name: &str) -> Option<Vec<u8>> {
    let name = wide(name);
    let descriptor = PROPERTY_DATA_DESCRIPTOR { PropertyName: name.as_ptr() as u64, ArrayIndex: u32::MAX, Reserved: 0 };
    let mut size = 0u32;
    // SAFETY: the record is the one Windows handed to the callback, valid for its duration; the buffer is as large as
    // TDH said the field is.
    unsafe {
        if TdhGetPropertySize(record, 0, std::ptr::null(), 1, &descriptor, &mut size) != 0 {
            return None;
        }
        let mut bytes = vec![0u8; size as usize];
        (TdhGetProperty(record, 0, std::ptr::null(), 1, &descriptor, size, bytes.as_mut_ptr()) == 0).then_some(bytes)
    }
}

fn number(record: *const EVENT_RECORD, name: &str) -> u64 {
    field(record, name).map_or(0, |bytes| {
        let mut eight = [0u8; 8];
        let length = bytes.len().min(8);
        eight[..length].copy_from_slice(&bytes[..length]);
        u64::from_le_bytes(eight)
    })
}

fn string(record: *const EVENT_RECORD, name: &str) -> String {
    field(record, name).map_or_else(String::new, |bytes| {
        let units: Vec<u16> = bytes.as_chunks::<2>().0.iter().map(|pair| u16::from_le_bytes(*pair)).take_while(|unit| *unit != 0).collect();
        String::from_utf16_lossy(&units)
    })
}

/// An address and port from a network event. Both are in network byte order, as they travel.
fn endpoint(record: *const EVENT_RECORD) -> String {
    let address = field(record, "daddr").unwrap_or_default();
    let port = field(record, "dport").filter(|bytes| bytes.len() == 2).map_or(0, |bytes| u16::from_be_bytes([bytes[0], bytes[1]]));
    match address.len() {
        4 => format!("{}.{}.{}.{}:{port}", address[0], address[1], address[2], address[3]),
        16 => {
            let mut sixteen = [0u8; 16];
            sixteen.copy_from_slice(&address);
            format!("[{}]:{port}", std::net::Ipv6Addr::from(sixteen))
        }
        _ => format!("unknown address:{port}"),
    }
}

/// What an exit code says when it is one of the system's own "the program went wrong" codes.
fn crash_name(code: u32) -> Option<&'static str> {
    Some(match code {
        0xC000_0005 => "access violation",
        0xC000_001D => "illegal instruction",
        0xC000_0094 => "integer division by zero",
        0xC000_00FD => "stack overflow",
        0xC000_0409 => "stack buffer overrun or fast fail",
        0xC000_0374 => "heap corruption",
        0xC000_0420 => "assertion failure",
        0x8000_0003 => "breakpoint",
        0xC000_0135 => "a DLL was not found",
        0xC000_0139 => "an entry point was not found in a DLL",
        // The top two bits say "error", the next says whether an application chose the code: only the system's count.
        other if other >> 30 == 3 && other & 0x2000_0000 == 0 && other != 0xC000_013A => "unhandled exception",
        _ => return None,
    })
}

/// The usual results of file operations, by the names Windows documents them under.
fn status_name(status: u32) -> String {
    let name = match status {
        0 => "ok",
        0x0000_0103 => "PENDING",
        0x8000_0005 => "BUFFER_OVERFLOW",
        0x8000_0006 => "NO_MORE_FILES",
        0xC000_0011 => "END_OF_FILE",
        0xC000_0022 => "ACCESS_DENIED",
        0xC000_0033 => "OBJECT_NAME_INVALID",
        0xC000_0034 => "OBJECT_NAME_NOT_FOUND",
        0xC000_0035 => "OBJECT_NAME_COLLISION",
        0xC000_003A => "OBJECT_PATH_NOT_FOUND",
        0xC000_0043 => "SHARING_VIOLATION",
        0xC000_0056 => "DELETE_PENDING",
        0xC000_00BA => "FILE_IS_A_DIRECTORY",
        0xC000_0101 => "DIRECTORY_NOT_EMPTY",
        0xC000_0103 => "NOT_A_DIRECTORY",
        other => return format!("{other:#010X}"),
    };
    name.to_string()
}

impl Tracer {
    /// Holds a file event until its operation ends.
    fn wait(&mut self, request: u64, event: Event, changes: Vec<(&'static str, String)>) {
        // An operation whose end never came (the session's buffers overflowed) must not be held for ever.
        if self.awaiting.len() > 8192 {
            self.release_all();
        }
        if let Some(earlier) = self.awaiting.insert(request, Waiting { event, changes }) {
            self.release(earlier, None);
        }
    }

    /// Files an event that was held, with how its operation ended when that is known.
    fn release(&mut self, mut waiting: Waiting, ended: Option<(u32, u64)>) {
        let (time_ns, pid) = (waiting.event.time_ns, waiting.event.pid);
        // Statuses below 0x80000000 are successes and plain information.
        let succeeded = ended.is_none_or(|(status, _)| status < 0x8000_0000);
        if let Some((status, extra)) = ended {
            waiting.event.result = status_name(status);
            match waiting.event.op.as_str() {
                "read" | "write" => {
                    waiting.event.args["asked"] = waiting.event.args["bytes"].take();
                    waiting.event.args["bytes"] = json!(extra);
                }
                // For an open, the extra information says what became of the file.
                "open" if succeeded => {
                    let (mode, change) = match extra {
                        0 => ("replace", Some("replace")),
                        2 => ("create", Some("create")),
                        3 => ("overwrite", Some("overwrite")),
                        _ => ("open", None),
                    };
                    waiting.event.args["mode"] = json!(mode);
                    waiting.changes = change.map(|change| (change, waiting.event.subject.clone())).into_iter().collect();
                }
                _ => {}
            }
        }
        self.pending.push(waiting.event);
        if succeeded {
            for (op, path) in waiting.changes {
                self.changed(time_ns, pid, op, &path, true);
            }
        }
    }

    fn release_all(&mut self) {
        let mut held: Vec<Waiting> = self.awaiting.drain().map(|(_, waiting)| waiting).collect();
        held.sort_by_key(|waiting| waiting.event.time_ns);
        for waiting in held {
            self.release(waiting, None);
        }
    }

    /// A file event of the target's, not yet filed.
    /// The user-mode part of the call stack Windows attached to an event, innermost first.
    fn addresses(&mut self, record: *const EVENT_RECORD) -> Vec<u64> {
        // SAFETY: the record and its extended data are valid for the duration of the callback; each item says how
        // long its data is.
        unsafe {
            let items: &[EVENT_HEADER_EXTENDED_DATA_ITEM] = match ((*record).ExtendedData.is_null(), (*record).ExtendedDataCount) {
                (false, count) if count > 0 => std::slice::from_raw_parts((*record).ExtendedData, usize::from(count)),
                _ => return Vec::new(),
            };
            // 6: a stack of 64-bit addresses, after an eight-byte number that pairs it with a second half sent apart.
            let Some(item) = items.iter().find(|item| item.ExtType == 6 && item.DataSize >= 16) else { return Vec::new() };
            let data = std::slice::from_raw_parts(item.DataPtr as *const u8, usize::from(item.DataSize));
            if u64::from_le_bytes(data[..8].try_into().expect("eight bytes")) != 0 {
                self.split_stacks += 1;
            }
            // Addresses in the upper half belong to Windows itself; the program's own start below it.
            data[8..].as_chunks::<8>().0.iter().map(|bytes| u64::from_le_bytes(*bytes)).filter(|address| *address < 0x8000_0000_0000_0000 && *address > 0xffff).collect()
        }
    }

    /// Addresses in a process's code as frames: `file!function+0x12`, or `file+0x1234` where no function is named.
    fn named(&mut self, pid: u32, addresses: &[u64]) -> Vec<String> {
        let loaded = self.modules.get(&pid).cloned().unwrap_or_default();
        addresses
            .iter()
            .enumerate()
            .map(|(depth, &address)| {
                // A caller's frame holds the address after its call; the call itself is one byte back at least.
                let inside = if depth == 0 { address } else { address - 1 };
                match loaded.iter().find(|(base, length, _)| (*base..base + length).contains(&inside)) {
                    Some((base, _, path)) => self.images.frame_loaded(path, inside - base, address - base),
                    None => format!("{address:#x}"),
                }
            })
            .collect()
    }

    fn file_event(&self, time_ns: u64, pid: u32, tid: u32, op: &str, subject: &str, args: Value) -> Event {
        let mut event = Event::new(time_ns, pid, Class::File, op, subject);
        event.tid = tid;
        event.process = self.names.get(&pid).cloned().unwrap_or_default();
        event.args = args;
        if pid == self.stack_of {
            event.stack = self.stack.clone();
        }
        event
    }

    /// Reports the program a process runs, with its command line when that is known.
    fn program(&mut self, time_ns: u64, pid: u32, image: &str) {
        let args = self.command_lines.get(&pid).map_or(Value::Null, |line| json!({ "command_line": line }));
        self.event(time_ns, pid, 0, Class::Process, "exec", image, args, "ok");
    }

    // One argument for each field of the event that differs from call to call.
    #[allow(clippy::too_many_arguments)]
    fn event(&mut self, time_ns: u64, pid: u32, tid: u32, class: Class, op: &str, subject: &str, args: Value, result: &str) {
        let mut event = Event::new(time_ns, pid, class, op, subject);
        event.tid = tid;
        event.process = self.names.get(&pid).cloned().unwrap_or_default();
        event.args = args;
        event.result = result.to_string();
        // The stack is the calling thread's: an event raised on a process's behalf by another thread has none.
        if pid == self.stack_of && class != Class::Problem {
            event.stack = self.stack.clone();
        }
        self.pending.push(event);
    }

    fn problem(&mut self, time_ns: u64, pid: u32, kind: &str, subject: &str, args: Value) {
        if self.reported.insert((kind.to_string(), subject.to_lowercase())) {
            self.event(time_ns, pid, 0, Class::Problem, kind, subject, args, "");
        }
    }

    fn flush(&mut self) {
        let request = &self.request;
        self.pending.retain(|event| request.keeps(event));
        if self.pending.is_empty() {
            return;
        }
        match self.store.insert(&self.pending) {
            Ok(()) => self.written += self.pending.len() as u64,
            Err(error) => eprintln!("observe-run: events were lost: {error}"),
        }
        self.pending.clear();
    }

    /// A change to a file, folder, key or value: checked against what the session guards and what it allows.
    /// Registry paths are only ever guarded: the allowed folders are about files.
    fn changed(&mut self, time_ns: u64, pid: u32, op: &str, path: &str, is_file: bool) {
        if self.request.guard.iter().any(|guarded| covers(guarded, path)) {
            self.problem(time_ns, pid, "guard_broken", path, json!({ "op": op }));
        } else if is_file && !self.request.allow.is_empty() && !self.request.allow.iter().any(|allowed| covers(allowed, path)) {
            self.problem(time_ns, pid, "write_outside_allowed", path, json!({ "op": op }));
        }
    }

    /// `\Device\HarddiskVolume3\Users\x` as `C:\Users\x`.
    fn file_path(&self, device_path: &str) -> String {
        self.drives
            .iter()
            .find_map(|(device, drive)| device_path.strip_prefix(device.as_str()).filter(|rest| rest.is_empty() || rest.starts_with('\\')).map(|rest| format!("{drive}{rest}")))
            .unwrap_or_else(|| device_path.to_string())
    }

    /// `\REGISTRY\MACHINE\Software\x` as `HKLM\Software\x`, and the signed-in user's hive as `HKCU`.
    fn key_path(&self, kernel_path: &str) -> String {
        let upper = kernel_path.to_uppercase();
        if !self.user_hive.is_empty() && upper.starts_with(&self.user_hive) {
            let rest = &kernel_path[self.user_hive.len()..];
            return match rest.strip_prefix("_Classes").or_else(|| rest.strip_prefix("_CLASSES")) {
                Some(classes) => format!("HKCU\\Software\\Classes{classes}"),
                None => format!("HKCU{rest}"),
            };
        }
        for (kernel, short) in [("\\REGISTRY\\MACHINE", "HKLM"), ("\\REGISTRY\\USER", "HKU")] {
            if upper.starts_with(kernel) {
                return format!("{short}{}", &kernel_path[kernel.len()..]);
            }
        }
        kernel_path.to_string()
    }

    fn handle(&mut self, record: *const EVENT_RECORD) {
        // SAFETY: Windows passes a valid record for the duration of the callback.
        let header = unsafe { &(*record).EventHeader };
        let provider = header.ProviderId;
        let same = |guid: &GUID| provider.data1 == guid.data1 && provider.data2 == guid.data2 && provider.data3 == guid.data3 && provider.data4 == guid.data4;
        let id = header.EventDescriptor.Id;
        // The time stamp counts 100 ns since 1601; events use nanoseconds since 1970.
        let time_ns = (header.TimeStamp as u64).saturating_sub(116_444_736_000_000_000) * 100;
        let (pid, tid) = (header.ProcessId, header.ThreadId);
        self.stack.clear();
        self.stack_of = 0;
        if self.request.stacks && self.names.contains_key(&pid) {
            let addresses = self.addresses(record);
            if !addresses.is_empty() {
                self.stack = self.named(pid, &addresses);
                self.stack_of = pid;
            }
        }

        if same(&FILE) && id == 24 {
            // The end of a file operation, raised for every program on the machine: read straight from the event's
            // bytes (request pointer, extra information, status) and matched against what is being held.
            // SAFETY: the record's data is valid for its stated length for the duration of the callback.
            let data = unsafe { std::slice::from_raw_parts((*record).UserData as *const u8, usize::from((*record).UserDataLength)) };
            if data.len() >= 20 && !self.awaiting.is_empty() {
                let at = |from: usize| u64::from_le_bytes(data[from..from + 8].try_into().expect("eight bytes"));
                if let Some(waiting) = self.awaiting.remove(&at(0)) {
                    let status = u32::from_le_bytes(data[16..20].try_into().expect("four bytes"));
                    self.release(waiting, Some((status, at(8))));
                    self.last_event = Instant::now();
                }
            }
            return;
        }
        if same(&CLASSIC_PROCESS) {
            // Opcode 1: a process started. Kept for every process, since this may arrive before the event that
            // tells us the process is one of ours.
            if header.EventDescriptor.Opcode == 1 {
                let started = number(record, "ProcessId") as u32;
                self.command_lines.insert(started, string(record, "CommandLine"));
                if let Some((time_ns, image)) = self.unnamed.remove(&started) {
                    self.program(time_ns, started, &image);
                }
            }
            return;
        }
        if same(&CLASSIC_MEMORY) {
            let owner = number(record, "ProcessId") as u32;
            let op = match header.EventDescriptor.Opcode {
                98 => "map",
                99 => "unmap",
                _ => return,
            };
            if self.names.contains_key(&owner) {
                let flags = number(record, "Flags");
                let kinds: Vec<&str> = [(0x1000, "commit"), (0x2000, "reserve"), (0x4000, "decommit"), (0x8000, "release")].iter().filter(|(bit, _)| flags & bit != 0).map(|(_, name)| *name).collect();
                self.event(time_ns, owner, tid, Class::Memory, op, &format!("{:#x}", number(record, "BaseAddress")), json!({ "length": number(record, "RegionSize"), "kind": kinds }), "ok");
                self.last_event = Instant::now();
            }
            return;
        }
        if same(&PROCESS) {
            match id {
                // A program file or library was loaded into a process, or taken out: what its addresses mean.
                5 | 6 => {
                    let owner = number(record, "ProcessID") as u32;
                    if self.request.stacks && self.names.contains_key(&owner) {
                        let base = number(record, "ImageBase");
                        let loaded = self.modules.entry(owner).or_default();
                        loaded.retain(|module| module.0 != base);
                        if id == 5 {
                            let path = self.file_path(&string(record, "ImageName"));
                            self.modules.entry(owner).or_default().push((base, number(record, "ImageSize"), path));
                        }
                    }
                    return;
                }
                1 => {
                    let (started, parent) = (number(record, "ProcessID") as u32, number(record, "ParentProcessID") as u32);
                    // Ours when its parent is ours; the target itself was put in the set when it was started.
                    if !self.live.contains(&started) && !self.live.contains(&parent) {
                        return;
                    }
                    let image = self.file_path(&string(record, "ImageName"));
                    self.live.insert(started);
                    self.names.insert(started, image.rsplit('\\').next().unwrap_or_default().to_string());
                    self.event(time_ns, started, 0, Class::Process, "start", "", json!({ "parent": parent }), "ok");
                    if self.command_lines.contains_key(&started) {
                        self.program(time_ns, started, &image);
                    } else {
                        self.unnamed.insert(started, (time_ns, image));
                    }
                }
                2 => {
                    let stopped = number(record, "ProcessID") as u32;
                    if !self.live.remove(&stopped) {
                        return;
                    }
                    if let Some((started_ns, image)) = self.unnamed.remove(&stopped) {
                        self.program(started_ns, stopped, &image);
                    }
                    let code = number(record, "ExitCode") as u32;
                    if let Some(what) = crash_name(code) {
                        let name = self.names.get(&stopped).cloned().unwrap_or_default();
                        self.problem(time_ns, stopped, "crash", &name, json!({ "code": format!("{code:#010X}"), "what": what }));
                    }
                    self.event(time_ns, stopped, 0, Class::Process, "exit", "", json!({ "exit_code": code }), "ok");
                }
                _ => return,
            }
        } else if same(&NETWORK) {
            // Network events are raised from the network stack's own threads; the process is in the event itself.
            let owner = number(record, "PID") as u32;
            if !self.names.contains_key(&owner) {
                return;
            }
            let op = match id {
                12 | 28 => "connect",
                15 | 31 => "accept",
                10 | 26 | 42 | 58 => "send",
                11 | 27 | 43 | 59 => "receive",
                _ => return,
            };
            let bytes = matches!(op, "send" | "receive").then(|| number(record, "size"));
            self.event(time_ns, owner, 0, Class::Network, op, &endpoint(record), json!({ "bytes": bytes }), "ok");
        } else if !self.names.contains_key(&pid) {
            return;
        } else if same(&FILE) {
            let object = number(record, "FileObject");
            match id {
                // 30 (a new file was made) repeats a create that 12 already reported; how 12 ends says the same.
                12 => {
                    let path = self.file_path(&string(record, "FileName"));
                    // The top byte of the options says what to do with a file that exists or doesn't.
                    let disposition = (number(record, "CreateOptions") >> 24) as u8;
                    let mode = match disposition {
                        0 | 4 | 5 => "overwrite",
                        2 => "create",
                        3 => "open or create",
                        _ => "open",
                    };
                    self.files.insert(object, path.clone());
                    // What it asked for; how it ended says what really happened to the file.
                    let changes = if matches!(disposition, 0 | 4 | 5) { vec![("overwrite", path.clone())] } else { Vec::new() };
                    let event = self.file_event(time_ns, pid, tid, "open", &path, json!({ "mode": mode }));
                    self.wait(number(record, "Irp"), event, changes);
                }
                15 | 16 => {
                    let Some(path) = self.files.get(&object).cloned() else { return };
                    let op = if id == 16 { "write" } else { "read" };
                    let event = self.file_event(time_ns, pid, tid, op, &path, json!({ "bytes": number(record, "IOSize"), "offset": number(record, "ByteOffset") }));
                    self.wait(number(record, "Irp"), event, if id == 16 { vec![("write", path)] } else { Vec::new() });
                }
                26 | 27 => {
                    let path = self.file_path(&string(record, "FilePath"));
                    if id == 26 {
                        let event = self.file_event(time_ns, pid, tid, "delete", &path, Value::Null);
                        self.wait(number(record, "Irp"), event, vec![("delete", path)]);
                    } else {
                        let from = self.files.get(&object).cloned().unwrap_or_default();
                        let event = self.file_event(time_ns, pid, tid, "rename", &from, json!({ "to": path }));
                        self.wait(number(record, "Irp"), event, vec![("rename", from), ("rename", path)]);
                    }
                }
                14 => {
                    self.files.remove(&object);
                    return;
                }
                _ => return,
            }
        } else if same(&REGISTRY) {
            let object = number(record, "KeyObject");
            let status = number(record, "Status") as u32;
            let result = if status == 0 { "ok".to_string() } else { format!("{status:#010X}") };
            match id {
                1 | 2 => {
                    // A key is named relative to the key it was opened under, which is given as the kernel's object for
                    // it; the full name comes from following those back to one opened by its whole name.
                    let relative = string(record, "RelativeName");
                    let key = if relative.starts_with('\\') {
                        self.key_path(relative.trim_end_matches('\\'))
                    } else {
                        let base = self.keys.get(&number(record, "BaseObject")).cloned().unwrap_or_else(|| self.key_path(&string(record, "BaseName")));
                        [base.as_str(), relative.as_str()].iter().filter(|part| !part.is_empty()).map(|part| part.trim_matches('\\')).collect::<Vec<_>>().join("\\")
                    };
                    if status == 0 {
                        self.keys.insert(object, key.clone());
                    }
                    // 1: the key did not exist and was made. Opening a key that exists is not worth an event.
                    if status == 0 && id == 1 && number(record, "Disposition") == 1 {
                        self.event(time_ns, pid, tid, Class::Registry, "create_key", &key, Value::Null, &result);
                        self.changed(time_ns, pid, "create_key", &key, false);
                    }
                }
                3 | 5 | 6 => {
                    let key = self.keys.get(&object).cloned().unwrap_or_else(|| self.key_path(&string(record, "KeyName")));
                    let (op, subject, args) = match id {
                        3 => ("delete_key", key, Value::Null),
                        5 => ("set_value", format!("{key}\\{}", string(record, "ValueName")), json!({ "type": number(record, "Type"), "bytes": number(record, "DataSize") })),
                        _ => ("delete_value", format!("{key}\\{}", string(record, "ValueName")), Value::Null),
                    };
                    self.event(time_ns, pid, tid, Class::Registry, op, &subject, args, &result);
                    if status == 0 {
                        self.changed(time_ns, pid, op, &subject, false);
                    }
                }
                _ => return,
            }
        } else {
            return;
        }
        self.last_event = Instant::now();
        if self.pending.len() >= 512 {
            self.flush();
        }
    }
}

unsafe extern "system" fn on_event(record: *mut EVENT_RECORD) {
    if let Ok(mut tracer) = TRACER.lock() {
        if let Some(tracer) = tracer.as_mut() {
            tracer.handle(record);
        }
    }
}

/// The device name behind each drive letter, for turning the kernel's file names into the ones people use.
fn drives() -> Vec<(String, String)> {
    ('A'..='Z')
        .filter_map(|letter| {
            let drive = format!("{letter}:");
            let mut device = [0u16; 512];
            // SAFETY: the buffer's real length is passed.
            let length = unsafe { QueryDosDeviceW(wide(&drive).as_ptr(), device.as_mut_ptr(), device.len() as u32) };
            let end = device.iter().position(|unit| *unit == 0).unwrap_or(0);
            (length != 0 && end != 0).then(|| (String::from_utf16_lossy(&device[..end]), drive))
        })
        .collect()
}

/// `\REGISTRY\USER\<the signed-in user's SID>`, in capitals, or empty when it can't be found.
fn user_hive() -> String {
    Command::new("whoami")
        .args(["/user", "/fo", "csv", "/nh"])
        .creation_flags(0x0800_0000)
        .output()
        .ok()
        .and_then(|output| String::from_utf8_lossy(&output.stdout).trim().rsplit(',').next().map(|sid| sid.trim_matches('"').to_uppercase()))
        .filter(|sid| sid.starts_with("S-1-"))
        .map_or_else(String::new, |sid| format!("\\REGISTRY\\USER\\{sid}"))
}

/// The properties block for a trace session: the structure, then room for the session's name.
fn properties(name: &[u16]) -> Vec<u64> {
    let size = size_of::<EVENT_TRACE_PROPERTIES>() + name.len() * 2;
    // In u64s so the structure at the front is aligned.
    let mut block = vec![0u64; size.div_ceil(8)];
    // SAFETY: the block is zeroed, aligned and at least as large as the structure.
    let header = unsafe { &mut *(block.as_mut_ptr() as *mut EVENT_TRACE_PROPERTIES) };
    header.Wnode.BufferSize = size as u32;
    header.Wnode.Flags = WNODE_FLAG_TRACED_GUID;
    // 1: time stamps from the high-resolution counter.
    header.Wnode.ClientContext = 1;
    header.LogFileMode = EVENT_TRACE_REAL_TIME_MODE | EVENT_TRACE_SYSTEM_LOGGER_MODE;
    header.EnableFlags = SYSTEM_FLAGS;
    header.BufferSize = 256;
    header.MinimumBuffers = 64;
    header.FlushTimer = 1;
    header.LoggerNameOffset = size_of::<EVENT_TRACE_PROPERTIES>() as u32;
    block
}

fn write_state(directory: &Path, state: &Value) {
    let _ = crate::jobs::write_whole(&directory.join("state.json"), state.to_string().as_bytes());
}

fn terminate(pid: u32) {
    // SAFETY: plain calls; the handle is checked and closed.
    unsafe {
        let process = OpenProcess(PROCESS_TERMINATE, 0, pid);
        if !process.is_null() {
            TerminateProcess(process, 1);
            CloseHandle(process);
        }
    }
}

/// A session on a program that was already running: it and what it has started are followed from now on, until it
/// ends or the session is stopped, and then left alone.
fn follow(directory: &Path, root: u32, session: CONTROLTRACE_HANDLE, mut block: Vec<u64>, consumer: u64, reading: std::thread::JoinHandle<()>) -> io::Result<()> {
    let close = |block: &mut Vec<u64>, reading: std::thread::JoinHandle<()>| {
        // SAFETY: the session handle is live and the block is the one it was started with; the consumer handle came
        // from OpenTrace and is closed once.
        unsafe {
            ControlTraceW(session, std::ptr::null(), block.as_mut_ptr().cast(), CONTROL_STOP);
            CloseTrace(windows_sys::Win32::System::Diagnostics::Etw::PROCESSTRACE_HANDLE { Value: consumer });
        }
        let _ = reading.join();
    };
    let found = family(root);
    // SAFETY: a plain call; the handle is checked below and closed at the end. 0x0010_0000: the right to wait for it.
    let process = unsafe { OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION | 0x0010_0000, 0, root) };
    if found.is_empty() || process.is_null() {
        close(&mut block, reading);
        *TRACER.lock().unwrap() = None;
        let why = if found.is_empty() { format!("there is no running program with the process id {root}") } else { format!("the program {root} can't be followed: {}", io::Error::last_os_error()) };
        write_state(directory, &json!({ "state": "failed", "error": why }));
        return Ok(());
    }
    File::create(directory.join("stdout.log"))?;
    File::create(directory.join("stderr.log"))?;
    {
        let mut guard = TRACER.lock().unwrap();
        let tracer = guard.as_mut().expect("set by run");
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |since| since.as_nanos() as u64);
        for (pid, parent, name) in &found {
            tracer.live.insert(*pid);
            tracer.names.insert(*pid, name.clone());
            if tracer.request.stacks {
                tracer.modules.insert(*pid, loaded_in(*pid));
            }
            tracer.event(now, *pid, 0, Class::Process, "attach", &image_of(*pid).unwrap_or_else(|| name.clone()), json!({ "parent": parent }), "ok");
        }
        tracer.flush();
    }
    write_state(directory, &json!({ "state": "running", "pid": root, "tracer": std::process::id(), "attached": true }));

    let mut stopped_at: Option<Instant> = None;
    let mut exit_code = None;
    let mut all_gone_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(50));
        if exit_code.is_none() {
            // SAFETY: the handle is live; the code is a local.
            unsafe {
                let mut code = 0u32;
                if WaitForSingleObject(process, 0) == 0 && GetExitCodeProcess(process, &mut code) != 0 {
                    exit_code = Some(code);
                }
            }
        }
        let mut guard = TRACER.lock().unwrap();
        let tracer = guard.as_mut().expect("set by run");
        tracer.flush();
        if stopped_at.is_none() && directory.join("stop").exists() {
            // Nothing is ended: the programs run on. The session's buffers get a moment to arrive.
            stopped_at = Some(Instant::now());
        }
        let let_go = stopped_at.is_some_and(|at| at.elapsed() > Duration::from_millis(1500));
        let ended = exit_code.is_some() && tracer.live.iter().all(|process| *process == root);
        if let_go {
            break;
        }
        if ended || stopped_at.is_some() {
            let since = *all_gone_since.get_or_insert_with(Instant::now);
            let quiet = tracer.last_event.elapsed() > Duration::from_millis(1500);
            if ended && ((since.elapsed() > Duration::from_millis(1500) && quiet) || since.elapsed() > Duration::from_secs(10)) {
                break;
            }
            drop(guard);
            // SAFETY: the session handle is live; the block is the one it was started with.
            unsafe { ControlTraceW(session, std::ptr::null(), block.as_mut_ptr().cast(), CONTROL_FLUSH) };
        } else {
            all_gone_since = None;
        }
    }
    // SAFETY: the handle was opened above and is closed once.
    unsafe { CloseHandle(process) };
    close(&mut block, reading);
    let mut tracer = TRACER.lock().unwrap().take().expect("set by run");
    tracer.release_all();
    for (pid, (time_ns, image)) in std::mem::take(&mut tracer.unnamed) {
        tracer.program(time_ns, pid, &image);
    }
    tracer.flush();
    let stopped = stopped_at.is_some() && exit_code.is_none();
    write_state(directory, &json!({ "state": if stopped { "stopped" } else { "exited" }, "pid": root, "exit_code": exit_code, "events": tracer.written, "attached": true }));
    Ok(())
}

/// A running program and everything it has started that is still running: each with its parent and its program's
/// name. Empty when there is no such program.
fn family(root: u32) -> Vec<(u32, u32, String)> {
    let mut everyone = Vec::new();
    // SAFETY: the entry is zeroed and told its own size, as the calls expect; the snapshot is closed.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snapshot as isize == -1 {
            return Vec::new();
        }
        let mut entry: PROCESSENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<PROCESSENTRY32W>() as u32;
        let mut more = Process32FirstW(snapshot, &mut entry) != 0;
        while more {
            let length = entry.szExeFile.iter().position(|unit| *unit == 0).unwrap_or(entry.szExeFile.len());
            everyone.push((entry.th32ProcessID, entry.th32ParentProcessID, String::from_utf16_lossy(&entry.szExeFile[..length])));
            more = Process32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    let mut found: Vec<(u32, u32, String)> = everyone.iter().filter(|(pid, _, _)| *pid == root).cloned().collect();
    let mut at = 0;
    while at < found.len() {
        let parent = found[at].0;
        // A process id is reused once its owner is gone, so a "child" may be older than its parent; one that is
        // already in the list can't be its own descendant either.
        let more: Vec<_> = everyone.iter().filter(|(pid, of, _)| *of == parent && !found.iter().any(|(known, _, _)| known == pid)).cloned().collect();
        found.extend(more);
        at += 1;
    }
    found
}

/// What a running process has loaded: where, how long, and which file.
fn loaded_in(pid: u32) -> Vec<(u64, u64, String)> {
    let mut loaded = Vec::new();
    // SAFETY: the entry is zeroed and told its own size, as the calls expect; the snapshot is closed.
    unsafe {
        let snapshot = CreateToolhelp32Snapshot(TH32CS_SNAPMODULE | TH32CS_SNAPMODULE32, pid);
        if snapshot as isize == -1 {
            return loaded;
        }
        let mut entry: MODULEENTRY32W = std::mem::zeroed();
        entry.dwSize = size_of::<MODULEENTRY32W>() as u32;
        let mut more = Module32FirstW(snapshot, &mut entry) != 0;
        while more {
            let length = entry.szExePath.iter().position(|unit| *unit == 0).unwrap_or(entry.szExePath.len());
            loaded.push((entry.modBaseAddr as u64, u64::from(entry.modBaseSize), String::from_utf16_lossy(&entry.szExePath[..length])));
            more = Module32NextW(snapshot, &mut entry) != 0;
        }
        CloseHandle(snapshot);
    }
    loaded
}

/// The full path of a running program, or nothing when it can't be asked.
fn image_of(pid: u32) -> Option<String> {
    // SAFETY: the buffer's length is given and updated by the call; the handle is checked and closed.
    unsafe {
        let process = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if process.is_null() {
            return None;
        }
        let mut path = [0u16; 1024];
        let mut length = path.len() as u32;
        let named = QueryFullProcessImageNameW(process, 0, path.as_mut_ptr(), &mut length) != 0;
        CloseHandle(process);
        named.then(|| String::from_utf16_lossy(&path[..length as usize]))
    }
}

/// Runs the session whose request is in `directory`. Returns when the target and everything it started have ended.
pub fn run(directory: &Path) -> io::Result<()> {
    let request: Request = serde_json::from_slice(&fs::read(directory.join("request.json"))?).map_err(io::Error::other)?;
    let store = Store::open(&directory.join("events.sqlite")).map_err(io::Error::other)?;
    let fail = |error: String| {
        write_state(directory, &json!({ "state": "failed", "error": error }));
        Ok(())
    };

    let name = wide(&format!("MasterControl-{}-{}", directory.file_name().and_then(|name| name.to_str()).unwrap_or("session"), std::process::id()));
    let mut block = properties(&name);
    let mut session = CONTROLTRACE_HANDLE { Value: 0 };
    // SAFETY: the name is zero-ended and the block is laid out as StartTrace expects, with room for the name.
    let mut started = unsafe { StartTraceW(&mut session, name.as_ptr(), block.as_mut_ptr().cast()) };
    // Windows runs only a handful of sessions that carry the kernel's own events at once (error 1450 when they are
    // taken): another session may be ending, so wait a little for a place.
    let waiting = Instant::now();
    while started == 1450 && waiting.elapsed() < Duration::from_secs(8) {
        std::thread::sleep(Duration::from_millis(250));
        block = properties(&name);
        started = unsafe { StartTraceW(&mut session, name.as_ptr(), block.as_mut_ptr().cast()) };
    }
    if started != 0 {
        let why = match started {
            5 => "it needs admin rights: run the agent elevated".to_string(),
            1450 => "Windows runs only a few such sessions at once and all are taken: stop another observation session first".to_string(),
            _ => io::Error::from_raw_os_error(started as i32).to_string(),
        };
        return fail(format!("the trace session couldn't be started: {why}"));
    }
    let stop_session = |block: &mut Vec<u64>| unsafe {
        ControlTraceW(session, std::ptr::null(), block.as_mut_ptr().cast(), CONTROL_STOP);
    };
    // With stacks asked for: every event carries the stack it was raised from (4 in EnableProperty), and the
    // process provider also tells of every program file and library loaded (0x40), which is what makes an address
    // in a stack mean something.
    let stacks = request.stacks;
    // SAFETY: the structure is plain data, zeroed and then filled as EnableTraceEx2 expects for version 2.
    let mut with_stacks: ENABLE_TRACE_PARAMETERS = unsafe { std::mem::zeroed() };
    with_stacks.Version = 2;
    with_stacks.EnableProperty = 4;
    for (provider, keywords) in KEYWORDS {
        let is_process = provider.data1 == PROCESS.data1;
        let keywords = if stacks && is_process { keywords | 0x40 } else { keywords };
        let parameters = if stacks { &with_stacks as *const ENABLE_TRACE_PARAMETERS } else { std::ptr::null() };
        // 1: enable the provider. 4: the "information" level, which every event used here is at or below.
        // SAFETY: the session handle is live; the provider id and the parameters outlive the call.
        let enabled = unsafe { EnableTraceEx2(session, &provider, 1, 4, keywords, 0, 0, parameters) };
        if enabled != 0 {
            stop_session(&mut block);
            return fail(format!("a kernel provider couldn't be switched on: {}", io::Error::from_raw_os_error(enabled as i32)));
        }
    }

    *TRACER.lock().unwrap() = Some(Tracer {
        store,
        pending: Vec::new(),
        request,
        live: HashSet::new(),
        names: HashMap::new(),
        files: HashMap::new(),
        keys: HashMap::new(),
        drives: drives(),
        user_hive: user_hive(),
        reported: HashSet::new(),
        written: 0,
        last_event: Instant::now(),
        awaiting: HashMap::new(),
        command_lines: HashMap::new(),
        unnamed: HashMap::new(),
        modules: HashMap::new(),
        images: Default::default(),
        stack: Vec::new(),
        stack_of: 0,
        split_stacks: 0,
    });
    // SAFETY: the structure is zeroed and then filled as OpenTrace expects; the name outlives the trace.
    let consumer = unsafe {
        let mut log: EVENT_TRACE_LOGFILEW = std::mem::zeroed();
        log.LoggerName = name.as_ptr() as *mut u16;
        log.Anonymous1.ProcessTraceMode = PROCESS_TRACE_MODE_REAL_TIME_AND_EVENT_RECORD;
        log.Anonymous2.EventRecordCallback = Some(on_event);
        OpenTraceW(&mut log)
    };
    if consumer.Value == u64::MAX {
        stop_session(&mut block);
        return fail(format!("the trace session couldn't be read: {}", io::Error::last_os_error()));
    }
    let consumer_value = consumer.Value;
    // ProcessTrace only returns when the session stops, so it gets a thread of its own.
    let reading = std::thread::spawn(move || unsafe {
        let handle = windows_sys::Win32::System::Diagnostics::Etw::PROCESSTRACE_HANDLE { Value: consumer_value };
        ProcessTrace(&handle, 1, std::ptr::null(), std::ptr::null());
    });
    // Events from before the reader was ready are lost; give it a moment before the target's first instruction.
    std::thread::sleep(Duration::from_millis(300));

    let (argv, cwd, env, attach) = {
        let tracer = TRACER.lock().unwrap();
        let request = &tracer.as_ref().expect("just set").request;
        (request.argv.clone(), request.cwd.clone(), request.env.clone(), request.attach)
    };
    if let Some(root) = attach {
        return follow(directory, root, session, block, consumer_value, reading);
    }
    let mut command = Command::new(&argv[0]);
    command
        .args(&argv[1..])
        .envs(env)
        .stdin(Stdio::null())
        .stdout(File::create(directory.join("stdout.log"))?)
        .stderr(File::create(directory.join("stderr.log"))?)
        // No console window of its own.
        .creation_flags(0x0800_0000);
    if let Some(cwd) = cwd {
        command.current_dir(cwd);
    }
    // The set must hold the target before its start event arrives, so the lock is held across the spawn.
    let spawned = {
        let mut tracer = TRACER.lock().unwrap();
        let spawned = command.spawn();
        if let (Ok(child), Some(tracer)) = (&spawned, tracer.as_mut()) {
            tracer.live.insert(child.id());
            tracer.names.insert(child.id(), Path::new(&argv[0]).file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default());
        }
        spawned
    };
    let mut child = match spawned {
        Ok(child) => child,
        Err(error) => {
            stop_session(&mut block);
            let _ = reading.join();
            return fail(format!("the command couldn't be started: {error}"));
        }
    };
    let pid = child.id();
    write_state(directory, &json!({ "state": "running", "pid": pid, "tracer": std::process::id() }));

    let mut stopped = false;
    let mut exit_code = None;
    let mut all_gone_since: Option<Instant> = None;
    loop {
        std::thread::sleep(Duration::from_millis(50));
        if exit_code.is_none() {
            exit_code = child.try_wait().ok().flatten().map(|status| status.code().unwrap_or(-1) as u32);
        }
        let mut guard = TRACER.lock().unwrap();
        let tracer = guard.as_mut().expect("set above");
        tracer.flush();
        if !stopped && directory.join("stop").exists() {
            stopped = true;
            for process in tracer.live.clone() {
                terminate(process);
            }
        }
        // Done when the target has ended, every process seen starting has been seen stopping, and the session has
        // been quiet long enough for its buffers (flushed every second) to have arrived.
        if exit_code.is_some() && tracer.live.iter().all(|process| *process == pid) {
            let since = *all_gone_since.get_or_insert_with(Instant::now);
            let quiet = tracer.last_event.elapsed() > Duration::from_millis(1500);
            if (since.elapsed() > Duration::from_millis(1500) && quiet) || since.elapsed() > Duration::from_secs(10) {
                break;
            }
            drop(guard);
            // SAFETY: the session handle is live; the block is the one it was started with.
            unsafe { ControlTraceW(session, std::ptr::null(), block.as_mut_ptr().cast(), CONTROL_FLUSH) };
        } else {
            all_gone_since = None;
        }
    }
    stop_session(&mut block);
    // SAFETY: the handle came from OpenTrace and is closed once.
    unsafe { CloseTrace(windows_sys::Win32::System::Diagnostics::Etw::PROCESSTRACE_HANDLE { Value: consumer_value }) };
    let _ = reading.join();
    let mut tracer = TRACER.lock().unwrap().take().expect("set above");
    tracer.release_all();
    for (pid, (time_ns, image)) in std::mem::take(&mut tracer.unnamed) {
        tracer.program(time_ns, pid, &image);
    }
    tracer.flush();
    if tracer.split_stacks > 0 {
        eprintln!("observe-run: {} stacks came with their user-mode half sent apart", tracer.split_stacks);
    }
    write_state(directory, &json!({ "state": if stopped { "stopped" } else { "exited" }, "pid": pid, "exit_code": exit_code, "events": tracer.written }));
    Ok(())
}
