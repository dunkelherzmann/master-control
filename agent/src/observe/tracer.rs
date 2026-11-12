//! The tracer: a helper process that starts the target, follows it and everything it starts with ptrace, and writes
//! what they do into the session's event store. It runs as its own process (`mc-agent observe-run DIR`) because a
//! tracer has to wait for all of its tracees, and that must not swallow the exits of the agent's other children.
//!
//! Linux on x86-64. No root is needed for a command the tracer starts itself.

use std::collections::{HashMap, HashSet};
use std::ffi::c_void;
use std::fs::{self, File};
use std::io;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use mc_core::{Class, Event, Store};
use serde_json::{json, Value};

use super::syscalls_x86_64::NAMES;
use super::{covers, Request};

static STOP: AtomicBool = AtomicBool::new(false);

extern "C" fn on_stop(_signal: libc::c_int) {
    STOP.store(true, Ordering::SeqCst);
}

const OPTIONS: libc::c_int = libc::PTRACE_O_TRACESYSGOOD
    | libc::PTRACE_O_TRACEFORK
    | libc::PTRACE_O_TRACEVFORK
    | libc::PTRACE_O_TRACECLONE
    | libc::PTRACE_O_TRACEEXEC
    | libc::PTRACE_O_EXITKILL;
/// For a program that was already running: the same, except that it outlives the tracer.
const ATTACH_OPTIONS: libc::c_int = OPTIONS & !libc::PTRACE_O_EXITKILL;
const PTRACE_SEIZE: libc::c_uint = 0x4206;
const PTRACE_INTERRUPT: libc::c_uint = 0x4207;
const PTRACE_LISTEN: libc::c_uint = 0x4208;
/// What a seized thread's stops carry in place of a signal: a stop the tracer asked for, a new thread's first
/// stop, or the program being stopped by a signal.
const PTRACE_EVENT_STOP: libc::c_int = 128;
const PTRACE_GET_SYSCALL_INFO: libc::c_uint = 0x420e;
const SYSCALL_ENTRY: u8 = 1;
const SYSCALL_EXIT: u8 = 2;

/// What the kernel says about the system call a stopped tracee is entering or leaving.
#[repr(C)]
#[derive(Default)]
struct SyscallInfo {
    op: u8,
    pad: [u8; 3],
    arch: u32,
    instruction_pointer: u64,
    stack_pointer: u64,
    /// On entry: the number and six arguments. On exit: the return value, then whether it is an error.
    data: [u64; 7],
}

/// A system call a tracee is in the middle of, with what had to be read before it ran.
struct Call {
    number: u64,
    args: [u64; 6],
    path: Option<String>,
    second_path: Option<String>,
    argv: Vec<String>,
    address: Option<String>,
    /// Where the program was when it made the call, innermost frame first; empty unless stacks were asked for.
    stack: Vec<String>,
}

struct Tracee {
    /// The process (thread group) this thread belongs to.
    pid: u32,
    call: Option<Call>,
    /// A thread or process the tracer hasn't resumed yet: its first stop is the kernel's own SIGSTOP, not a signal
    /// for the program.
    new: bool,
    /// Where the program was when a signal that usually kills arrived.
    /// The signal that is about to end it, where it was, and the stack that led there.
    fatal: Option<(i32, String, Vec<String>)>,
}

struct Tracer {
    images: super::symbols::Images,
    stacks: super::unwind::Stacks,
    store: Store,
    pending: Vec<Event>,
    request: Request,
    tracees: HashMap<u32, Tracee>,
    names: HashMap<u32, String>,
    /// What a socket of a process is connected or bound to.
    sockets: HashMap<(u32, u64), String>,
    reported: HashSet<(String, String)>,
    written: u64,
}

fn now_ns() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |since| since.as_nanos() as u64)
}

fn syscall_name(number: u64) -> String {
    NAMES.iter().find(|(known, _)| u64::from(*known) == number).map_or_else(|| format!("syscall_{number}"), |(_, name)| name.to_string())
}

fn errno_name(error: i64) -> String {
    let name = match error as i32 {
        libc::EPERM => "EPERM",
        libc::ENOENT => "ENOENT",
        libc::EINTR => "EINTR",
        libc::EIO => "EIO",
        libc::EBADF => "EBADF",
        libc::EAGAIN => "EAGAIN",
        libc::ENOMEM => "ENOMEM",
        libc::EACCES => "EACCES",
        libc::EEXIST => "EEXIST",
        libc::ENOTDIR => "ENOTDIR",
        libc::EISDIR => "EISDIR",
        libc::EINVAL => "EINVAL",
        libc::ENOSPC => "ENOSPC",
        libc::EROFS => "EROFS",
        libc::EPIPE => "EPIPE",
        libc::ENOTEMPTY => "ENOTEMPTY",
        libc::ECONNREFUSED => "ECONNREFUSED",
        libc::ECONNRESET => "ECONNRESET",
        libc::ETIMEDOUT => "ETIMEDOUT",
        libc::EINPROGRESS => "EINPROGRESS",
        libc::ENETUNREACH => "ENETUNREACH",
        libc::EHOSTUNREACH => "EHOSTUNREACH",
        libc::EADDRINUSE => "EADDRINUSE",
        libc::ENOSYS => "ENOSYS",
        other => return format!("E{other}"),
    };
    name.to_string()
}

fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGABRT => "SIGABRT",
        libc::SIGBUS => "SIGBUS",
        libc::SIGFPE => "SIGFPE",
        libc::SIGILL => "SIGILL",
        libc::SIGSYS => "SIGSYS",
        libc::SIGKILL => "SIGKILL",
        libc::SIGTERM => "SIGTERM",
        libc::SIGINT => "SIGINT",
        libc::SIGHUP => "SIGHUP",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGTRAP => "SIGTRAP",
        other => return format!("signal {other}"),
    };
    name.to_string()
}

/// Signals that mean the program went wrong, as opposed to being told to stop.
fn is_crash(signal: i32) -> bool {
    matches!(signal, libc::SIGSEGV | libc::SIGABRT | libc::SIGBUS | libc::SIGFPE | libc::SIGILL | libc::SIGSYS)
}

// The request numbers are unsigned in one C library (glibc) and signed in another (musl): take either.
fn ptrace(request: impl Into<i64>, tid: u32, address: usize, data: usize) -> libc::c_long {
    // SAFETY: the arguments are plain numbers; where a request takes a pointer, the caller passes one that is valid
    // for that request's structure.
    unsafe { libc::ptrace(request.into() as _, tid as libc::pid_t, address, data) }
}

/// Reads the tracee's memory. Stops early where the memory ends.
fn read_memory(tid: u32, address: u64, buffer: &mut [u8]) -> usize {
    let local = libc::iovec { iov_base: buffer.as_mut_ptr().cast::<c_void>(), iov_len: buffer.len() };
    let remote = libc::iovec { iov_base: address as *mut c_void, iov_len: buffer.len() };
    // SAFETY: the local vector describes `buffer`; the remote one is checked by the kernel against the tracee's memory.
    let read = unsafe { libc::process_vm_readv(tid as libc::pid_t, &local, 1, &remote, 1, 0) };
    read.max(0) as usize
}

/// A zero-ended string from the tracee's memory, read page by page so a string near the end of a mapping still comes.
fn read_string(tid: u32, address: u64) -> Option<String> {
    if address == 0 {
        return None;
    }
    let mut bytes = Vec::new();
    while bytes.len() < 4096 {
        let at = address + bytes.len() as u64;
        let to_page_end = 4096 - (at % 4096) as usize;
        let mut chunk = vec![0u8; to_page_end.min(512)];
        let read = read_memory(tid, at, &mut chunk);
        if read == 0 {
            break;
        }
        if let Some(end) = chunk[..read].iter().position(|byte| *byte == 0) {
            bytes.extend(&chunk[..end]);
            return Some(String::from_utf8_lossy(&bytes).into_owned());
        }
        bytes.extend(&chunk[..read]);
    }
    (!bytes.is_empty()).then(|| String::from_utf8_lossy(&bytes).into_owned())
}

fn read_strings(tid: u32, address: u64) -> Vec<String> {
    let mut strings = Vec::new();
    while address != 0 && strings.len() < 64 {
        let mut pointer = [0u8; 8];
        if read_memory(tid, address + strings.len() as u64 * 8, &mut pointer) != 8 || u64::from_ne_bytes(pointer) == 0 {
            break;
        }
        strings.push(read_string(tid, u64::from_ne_bytes(pointer)).unwrap_or_default());
    }
    strings
}

/// A socket address from the tracee's memory, as text: `93.184.216.34:80`, `[::1]:8080`, `unix:/run/x.sock`.
fn read_address(tid: u32, address: u64, length: u64) -> Option<String> {
    let mut bytes = vec![0u8; (length as usize).clamp(2, 128)];
    let read = read_memory(tid, address, &mut bytes);
    if address == 0 || read < 2 {
        return None;
    }
    bytes.truncate(read);
    let port = |bytes: &[u8]| u16::from_be_bytes([bytes[2], bytes[3]]);
    Some(match i32::from(u16::from_ne_bytes([bytes[0], bytes[1]])) {
        libc::AF_INET if bytes.len() >= 8 => format!("{}.{}.{}.{}:{}", bytes[4], bytes[5], bytes[6], bytes[7], port(&bytes)),
        libc::AF_INET6 if bytes.len() >= 24 => {
            let mut address = [0u8; 16];
            address.copy_from_slice(&bytes[8..24]);
            format!("[{}]:{}", std::net::Ipv6Addr::from(address), port(&bytes))
        }
        libc::AF_UNIX => {
            let path = &bytes[2..];
            let end = path.iter().skip(1).position(|byte| *byte == 0).map_or(path.len(), |end| end + 1);
            match path.first() {
                None => "unix:".to_string(),
                // An abstract socket: a name that is not a file, written with a leading @.
                Some(0) => format!("unix:@{}", String::from_utf8_lossy(&path[1..end])),
                Some(_) => format!("unix:{}", String::from_utf8_lossy(&path[..end])),
            }
        }
        libc::AF_NETLINK => "netlink".to_string(),
        libc::AF_UNSPEC => "unspecified".to_string(),
        family => format!("address family {family}"),
    })
}

fn link(path: String) -> Option<String> {
    fs::read_link(path).ok().map(|target| target.to_string_lossy().into_owned())
}

/// A path without `.` and `..` parts, worked out from the text alone.
fn tidy(path: &Path) -> String {
    let mut parts: Vec<&std::ffi::OsStr> = Vec::new();
    for part in path.components() {
        match part {
            Component::ParentDir => {
                parts.pop();
            }
            Component::Normal(name) => parts.push(name),
            _ => {}
        }
    }
    let mut tidy = PathBuf::from("/");
    tidy.extend(parts);
    tidy.to_string_lossy().into_owned()
}

/// The full path a program meant by a path it gave relative to a folder (the "at" family of system calls).
fn full_path(tid: u32, folder: u64, path: &str) -> String {
    if path.starts_with('/') {
        return tidy(Path::new(path));
    }
    let base = if folder as i32 == libc::AT_FDCWD { link(format!("/proc/{tid}/cwd")) } else { link(format!("/proc/{tid}/fd/{}", folder as i32)) };
    tidy(&Path::new(&base.unwrap_or_else(|| "/".into())).join(path))
}

/// Where in the program an address is: the mapped file and the offset in it.
fn read_word(tid: u32, address: u64) -> Option<u64> {
    let mut bytes = [0u8; 8];
    (read_memory(tid, address, &mut bytes) == 8).then(|| u64::from_le_bytes(bytes))
}

fn locate(tid: u32, address: u64) -> String {
    let maps = fs::read_to_string(format!("/proc/{tid}/maps")).unwrap_or_default();
    for line in maps.lines() {
        let mut fields = line.split_whitespace();
        let (Some(range), _, Some(offset), _, _, file) = (fields.next(), fields.next(), fields.next(), fields.next(), fields.next(), fields.next()) else {
            continue;
        };
        let Some((start, end)) = range.split_once('-').and_then(|(start, end)| Some((u64::from_str_radix(start, 16).ok()?, u64::from_str_radix(end, 16).ok()?))) else {
            continue;
        };
        if (start..end).contains(&address) {
            let offset = u64::from_str_radix(offset, 16).unwrap_or(0) + (address - start);
            let name = file.map_or("anonymous memory", |file| file.rsplit('/').next().unwrap_or(file));
            return format!("{name}+{offset:#x}");
        }
    }
    format!("{address:#x}")
}

fn open_flags(flags: u64) -> Vec<&'static str> {
    let mut names = Vec::new();
    match flags as i32 & libc::O_ACCMODE {
        libc::O_WRONLY => names.push("write"),
        libc::O_RDWR => names.extend(["read", "write"]),
        _ => names.push("read"),
    }
    for (bit, name) in [(libc::O_CREAT, "create"), (libc::O_TRUNC, "truncate"), (libc::O_APPEND, "append"), (libc::O_EXCL, "exclusive"), (libc::O_DIRECTORY, "directory")] {
        if flags as i32 & bit == bit {
            names.push(name);
        }
    }
    names
}

impl Tracer {
    fn name(&mut self, pid: u32) -> String {
        self.names
            .entry(pid)
            .or_insert_with(|| fs::read_to_string(format!("/proc/{pid}/comm")).map(|name| name.trim().to_string()).unwrap_or_default())
            .clone()
    }

    // One argument for each field of the event that differs from call to call.
    #[allow(clippy::too_many_arguments)]
    fn event(&mut self, pid: u32, tid: u32, class: Class, op: &str, subject: &str, args: Value, result: &str) {
        let mut event = Event::new(now_ns(), pid, class, op, subject);
        event.tid = tid;
        event.process = self.name(pid);
        event.args = args;
        event.result = result.to_string();
        self.pending.push(event);
    }

    fn problem(&mut self, pid: u32, tid: u32, kind: &str, subject: &str, args: Value) {
        // Once per kind and subject: a program that writes a stray file a thousand times has one problem.
        if self.reported.insert((kind.to_string(), subject.to_string())) {
            self.event(pid, tid, Class::Problem, kind, subject, args, "");
        }
    }

    fn flush(&mut self) {
        let request = &self.request;
        self.pending.retain(|event| request.keeps(event));
        if self.pending.is_empty() {
            return;
        }
        if let Err(error) = self.store.insert(&self.pending) {
            eprintln!("observe-run: events were lost: {error}");
        } else {
            self.written += self.pending.len() as u64;
        }
        self.pending.clear();
    }

    /// A change to a file or folder: checked against what the session guards and what it allows.
    fn changed(&mut self, pid: u32, tid: u32, op: &str, path: &str) {
        if !path.starts_with('/') {
            return;
        }
        if self.request.guard.iter().any(|guarded| covers(guarded, path)) {
            self.problem(pid, tid, "guard_broken", path, json!({ "op": op }));
        } else if !self.request.allow.is_empty()
            && !self.request.allow.iter().any(|allowed| covers(allowed, path))
            && !["/dev", "/proc"].iter().any(|system| covers(system, path))
        {
            self.problem(pid, tid, "write_outside_allowed", path, json!({ "op": op }));
        }
    }

    /// Notes what must be read before the call runs: the kernel may change or discard it (exec replaces the memory).
    fn entered(&mut self, tid: u32, number: u64, args: [u64; 6]) {
        let name = syscall_name(number);
        let stack = if self.request.stacks { self.stack_of(tid) } else { Vec::new() };
        let mut call = Call { number, args, path: None, second_path: None, argv: Vec::new(), address: None, stack };
        let plain = |index: usize| read_string(tid, args[index]).map(|path| full_path(tid, libc::AT_FDCWD as u64, &path));
        let at = |folder: usize, index: usize| read_string(tid, args[index]).map(|path| full_path(tid, args[folder], &path));
        match name.as_str() {
            "open" | "creat" | "unlink" | "mkdir" | "rmdir" | "chmod" | "truncate" | "chdir" => call.path = plain(0),
            "openat" | "openat2" | "unlinkat" | "mkdirat" | "fchmodat" => call.path = at(0, 1),
            "rename" | "link" | "symlink" => {
                call.path = plain(0);
                call.second_path = plain(1);
            }
            "renameat" | "renameat2" | "linkat" => {
                call.path = at(0, 1);
                call.second_path = at(2, 3);
            }
            "execve" => {
                call.path = plain(0);
                call.argv = read_strings(tid, args[1]);
            }
            "execveat" => {
                call.path = at(0, 1);
                call.argv = read_strings(tid, args[2]);
            }
            "connect" | "bind" => call.address = read_address(tid, args[1], args[2]),
            "sendto" if args[4] != 0 => call.address = read_address(tid, args[4], args[5]),
            _ => {}
        }
        if let Some(tracee) = self.tracees.get_mut(&tid) {
            tracee.call = Some(call);
        }
    }

    /// Turns a finished system call into events.
    /// A call has returned: the events it makes are written, each with the stack the call was made from.
    fn left(&mut self, tid: u32, value: i64) {
        let stack = self.tracees.get_mut(&tid).and_then(|tracee| tracee.call.as_mut()).map(|call| std::mem::take(&mut call.stack)).unwrap_or_default();
        let before = self.pending.len();
        self.returned(tid, value);
        if !stack.is_empty() {
            for event in self.pending[before..].iter_mut().filter(|event| event.class != Class::Problem) {
                event.stack = stack.clone();
            }
        }
    }

    fn returned(&mut self, tid: u32, value: i64) {
        let Some((pid, Some(call))) = self.tracees.get_mut(&tid).map(|tracee| (tracee.pid, tracee.call.take())) else { return };
        let name = syscall_name(call.number);
        let failed = (-4095..0).contains(&value);
        let result = if failed { errno_name(-value) } else { "ok".to_string() };
        let args = call.args;
        let descriptor = |fd: u64| link(format!("/proc/{tid}/fd/{}", fd as i32)).unwrap_or_default();
        if self.request.syscalls == "full" {
            let shown: Vec<String> = args.iter().map(|arg| format!("{arg:#x}")).collect();
            self.event(pid, tid, Class::Syscall, &name, call.path.as_deref().unwrap_or_default(), json!({ "args": shown, "returned": value }), &result);
        }
        match name.as_str() {
            "open" | "openat" | "openat2" | "creat" => {
                let flags = match name.as_str() {
                    "open" => args[1],
                    "openat" => args[2],
                    "creat" => (libc::O_WRONLY | libc::O_CREAT | libc::O_TRUNC) as u64,
                    // openat2 takes its flags in a structure; its first eight bytes are the same flags.
                    _ => {
                        let mut how = [0u8; 8];
                        read_memory(tid, args[2], &mut how);
                        u64::from_ne_bytes(how)
                    }
                };
                let flags = open_flags(flags);
                // The kernel's own name for what was opened, links followed; the path as asked when it failed.
                let path = if failed { call.path.unwrap_or_default() } else { Some(descriptor(value as u64)).filter(|path| path.starts_with('/')).or(call.path).unwrap_or_default() };
                let writes = flags.iter().any(|flag| matches!(*flag, "write" | "create" | "truncate" | "append"));
                self.event(pid, tid, Class::File, "open", &path, json!({ "mode": flags, "fd": (!failed).then_some(value) }), &result);
                if writes && !failed && !flags.contains(&"directory") {
                    self.changed(pid, tid, "open for writing", &path);
                }
            }
            "read" | "pread64" | "readv" | "preadv" | "write" | "pwrite64" | "writev" | "pwritev" | "sendto" | "sendmsg" | "recvfrom" | "recvmsg" => {
                let target = descriptor(args[0]);
                let writing = matches!(name.as_str(), "write" | "pwrite64" | "writev" | "pwritev" | "sendto" | "sendmsg");
                let bytes = (!failed).then_some(value);
                if target.starts_with('/') && !target.starts_with("/dev/") {
                    let mut detail = json!({ "fd": args[0], "bytes": bytes });
                    if matches!(name.as_str(), "pread64" | "pwrite64") {
                        detail["offset"] = json!(args[3]);
                    }
                    self.event(pid, tid, Class::File, if writing { "write" } else { "read" }, &target, detail, &result);
                    if writing && !failed {
                        self.changed(pid, tid, "write", &target);
                    }
                } else if target.starts_with("socket:") {
                    let peer = call.address.or_else(|| self.sockets.get(&(pid, args[0])).cloned()).unwrap_or_else(|| "socket".into());
                    self.event(pid, tid, Class::Network, if writing { "send" } else { "receive" }, &peer, json!({ "fd": args[0], "bytes": bytes }), &result);
                }
            }
            "close" => {
                self.sockets.remove(&(pid, args[0]));
            }
            "unlink" | "unlinkat" | "rmdir" | "mkdir" | "mkdirat" | "chmod" | "fchmodat" | "truncate" => {
                let path = call.path.unwrap_or_default();
                let removes_folder = name == "rmdir" || (name == "unlinkat" && args[2] as i32 & libc::AT_REMOVEDIR != 0);
                let op = match name.as_str() {
                    _ if removes_folder => "delete_dir",
                    "unlink" | "unlinkat" => "delete",
                    "mkdir" | "mkdirat" => "create_dir",
                    "truncate" => "truncate",
                    _ => "set_mode",
                };
                self.event(pid, tid, Class::File, op, &path, Value::Null, &result);
                if !failed {
                    self.changed(pid, tid, op, &path);
                }
            }
            "ftruncate" | "fchmod" => {
                let path = descriptor(args[0]);
                let op = if name == "ftruncate" { "truncate" } else { "set_mode" };
                self.event(pid, tid, Class::File, op, &path, json!({ "fd": args[0] }), &result);
                if !failed {
                    self.changed(pid, tid, op, &path);
                }
            }
            "rename" | "renameat" | "renameat2" | "link" | "linkat" | "symlink" => {
                let (from, to) = (call.path.unwrap_or_default(), call.second_path.unwrap_or_default());
                let op = if name.starts_with("rename") { "rename" } else { "link" };
                self.event(pid, tid, Class::File, op, &from, json!({ "to": to }), &result);
                if !failed {
                    if op == "rename" {
                        self.changed(pid, tid, "rename", &from);
                    }
                    self.changed(pid, tid, op, &to);
                }
            }
            "execve" | "execveat" if failed => {
                // A successful exec is reported when the kernel announces it, with the new program's name.
                self.event(pid, tid, Class::Process, "exec", &call.path.unwrap_or_default(), json!({ "argv": call.argv }), &result);
            }
            "connect" | "bind" => {
                let address = call.address.unwrap_or_else(|| "unknown address".into());
                // A socket that doesn't block answers "in progress"; the connection is then under way, not refused.
                if !failed || value == -i64::from(libc::EINPROGRESS) {
                    self.sockets.insert((pid, args[0]), address.clone());
                }
                if address != "unspecified" {
                    self.event(pid, tid, Class::Network, &name, &address, json!({ "fd": args[0] }), &result);
                }
            }
            "listen" => {
                let address = self.sockets.get(&(pid, args[0])).cloned().unwrap_or_else(|| "socket".into());
                self.event(pid, tid, Class::Network, "listen", &address, json!({ "fd": args[0] }), &result);
            }
            "accept" | "accept4" if !failed => {
                let mut length = [0u8; 4];
                read_memory(tid, args[2], &mut length);
                let peer = read_address(tid, args[1], u64::from(u32::from_ne_bytes(length))).unwrap_or_else(|| "unknown address".into());
                self.sockets.insert((pid, value as u64), peer.clone());
                self.event(pid, tid, Class::Network, "accept", &peer, json!({ "fd": value }), &result);
            }
            "mmap" | "munmap" | "mprotect" | "brk" => {
                let protection = |bits: u64| {
                    [(libc::PROT_READ, 'r'), (libc::PROT_WRITE, 'w'), (libc::PROT_EXEC, 'x')].iter().map(|(bit, letter)| if bits as i32 & bit != 0 { *letter } else { '-' }).collect::<String>()
                };
                let (op, at, detail) = match name.as_str() {
                    "mmap" => {
                        let file = (args[3] as i32 & libc::MAP_ANONYMOUS == 0).then(|| descriptor(args[4]));
                        ("map", if failed { args[0] } else { value as u64 }, json!({ "length": args[1], "protection": protection(args[2]), "file": file }))
                    }
                    "munmap" => ("unmap", args[0], json!({ "length": args[1] })),
                    "mprotect" => ("protect", args[0], json!({ "length": args[1], "protection": protection(args[2]) })),
                    _ => ("grow", value as u64, Value::Null),
                };
                self.event(pid, tid, Class::Memory, op, &format!("{at:#x}"), detail, &result);
            }
            _ => {}
        }
    }

    /// Registers a thread or process the tracer has just learned of.
    /// Where a stopped thread is and how it got there, innermost frame first.
    fn stack_of(&mut self, tid: u32) -> Vec<String> {
        // SAFETY: user_regs_struct is plain numbers, and GETREGS fills exactly one.
        let mut registers: libc::user_regs_struct = unsafe { std::mem::zeroed() };
        if ptrace(libc::PTRACE_GETREGS, tid, 0, &mut registers as *mut libc::user_regs_struct as usize) == -1 {
            return Vec::new();
        }
        self.walked(tid, &registers)
    }

    /// The stack a thread with these registers is on, with every frame named.
    fn walked(&mut self, tid: u32, registers: &libc::user_regs_struct) -> Vec<String> {
        let pid = self.tracees.get(&tid).map_or(tid, |tracee| tracee.pid);
        self.stacks.of(pid, (registers.rip, registers.rsp, registers.rbp), &mut |address| read_word(tid, address), &mut self.images)
    }

    fn meet(&mut self, tid: u32) {
        if self.tracees.contains_key(&tid) {
            return;
        }
        let status = fs::read_to_string(format!("/proc/{tid}/status")).unwrap_or_default();
        let pid = status.lines().find_map(|line| line.strip_prefix("Tgid:")).and_then(|value| value.trim().parse().ok()).unwrap_or(tid);
        self.tracees.insert(tid, Tracee { pid, call: None, new: true, fatal: None });
    }

    fn stopped(&mut self, tid: u32, status: libc::c_int) {
        let signal = libc::WSTOPSIG(status);
        let happening = (status >> 16) & 0xffff;
        self.meet(tid);
        if happening == PTRACE_EVENT_STOP {
            self.tracees.get_mut(&tid).expect("just met").new = false;
            if signal == libc::SIGTRAP {
                // The stop we asked for when taking the thread over, or a new thread's first: on it goes.
                ptrace(libc::PTRACE_SYSCALL, tid, 0, 0);
            } else {
                // Stopped by a signal, as its user or its shell wanted: it stays stopped until they continue it.
                ptrace(PTRACE_LISTEN, tid, 0, 0);
            }
            return;
        }
        let pid = self.tracees[&tid].pid;
        let was_new = std::mem::replace(&mut self.tracees.get_mut(&tid).expect("just met").new, false);
        let mut deliver = 0;
        if signal == libc::SIGTRAP | 0x80 {
            let mut info = SyscallInfo::default();
            ptrace(PTRACE_GET_SYSCALL_INFO, tid, size_of::<SyscallInfo>(), &mut info as *mut SyscallInfo as usize);
            match info.op {
                SYSCALL_ENTRY => self.entered(tid, info.data[0], [info.data[1], info.data[2], info.data[3], info.data[4], info.data[5], info.data[6]]),
                SYSCALL_EXIT => self.left(tid, info.data[0] as i64),
                _ => {}
            }
        } else if signal == libc::SIGTRAP && happening != 0 {
            let mut message: libc::c_ulong = 0;
            ptrace(libc::PTRACE_GETEVENTMSG, tid, 0, &mut message as *mut libc::c_ulong as usize);
            match happening {
                libc::PTRACE_EVENT_FORK | libc::PTRACE_EVENT_VFORK | libc::PTRACE_EVENT_CLONE => {
                    let child = message as u32;
                    self.meet(child);
                    // A new thread shares its process's id; only a new process is a start.
                    if self.tracees[&child].pid == child {
                        self.event(child, child, Class::Process, "start", "", json!({ "parent": pid }), "ok");
                    }
                }
                libc::PTRACE_EVENT_EXEC => {
                    // When a thread that isn't the first one execs, the kernel gives the new program the process's id
                    // and reports under it; the call it was in is still filed under the thread's old id.
                    let former = message as u32;
                    let call = if former != tid { self.tracees.remove(&former).and_then(|tracee| tracee.call) } else { None };
                    let tracee = self.tracees.get_mut(&tid).expect("just met");
                    if call.is_some() {
                        tracee.call = call;
                    }
                    let (path, argv) = tracee.call.as_ref().map(|call| (call.path.clone().unwrap_or_default(), call.argv.clone())).unwrap_or_default();
                    self.names.remove(&pid);
                    self.stacks.forget(pid);
                    self.sockets.retain(|(owner, _), _| *owner != pid);
                    self.event(pid, tid, Class::Process, "exec", &path, json!({ "argv": argv }), "ok");
                }
                _ => {}
            }
        } else if signal == libc::SIGSTOP && was_new {
            // The kernel's stop for a newly followed thread; the program never asked for it and must not see it.
        } else if signal != libc::SIGTRAP {
            if is_crash(signal) {
                // SAFETY: user_regs_struct is plain numbers, and GETREGS fills exactly one.
                let mut registers: libc::user_regs_struct = unsafe { std::mem::zeroed() };
                ptrace(libc::PTRACE_GETREGS, tid, 0, &mut registers as *mut libc::user_regs_struct as usize);
                let at = locate(tid, registers.rip);
                let stack = self.walked(tid, &registers);
                self.tracees.get_mut(&tid).expect("just met").fatal = Some((signal, at, stack));
            }
            deliver = signal;
        }
        ptrace(libc::PTRACE_SYSCALL, tid, 0, deliver as usize);
    }

    /// A thread ended. Returns the process's exit code when it was the last word on a whole process.
    fn ended(&mut self, tid: u32, status: libc::c_int) -> Option<Value> {
        let tracee = self.tracees.remove(&tid)?;
        if tracee.pid != tid {
            return None;
        }
        let pid = tracee.pid;
        self.sockets.retain(|(owner, _), _| *owner != pid);
        let how = if libc::WIFEXITED(status) {
            json!({ "exit_code": libc::WEXITSTATUS(status) })
        } else {
            let signal = libc::WTERMSIG(status);
            if is_crash(signal) {
                // The thread that took the signal may not be the one whose exit is reported; any thread's note will do.
                let fatal = tracee.fatal.or_else(|| self.tracees.values_mut().filter(|other| other.pid == pid).find_map(|other| other.fatal.take()));
                let (at, stack) = fatal.map(|(_, at, stack)| (Some(at), stack)).unwrap_or_default();
                let name = self.name(pid);
                self.problem(pid, tid, "crash", &name, json!({ "signal": signal_name(signal), "at": at, "stack": stack }));
            }
            json!({ "signal": signal_name(signal) })
        };
        self.event(pid, tid, Class::Process, "exit", "", how.clone(), "ok");
        self.names.remove(&pid);
        self.stacks.forget(pid);
        Some(how)
    }
}

/// Whether following a program that is already running takes root on this machine: the kernel may let a program
/// be traced only by the one that started it.
pub fn attach_needs_root() -> bool {
    // SAFETY: geteuid has no arguments and cannot fail.
    let root = unsafe { libc::geteuid() } == 0;
    !root && fs::read_to_string("/proc/sys/kernel/yama/ptrace_scope").is_ok_and(|scope| scope.trim() != "0")
}

fn numbered(folder: &str) -> Vec<u32> {
    fs::read_dir(folder).map(|entries| entries.filter_map(|entry| entry.ok()?.file_name().to_str()?.parse().ok()).collect()).unwrap_or_default()
}

/// A running program and everything it has started that is still running, each with its parent.
fn family(root: u32) -> Vec<(u32, u32)> {
    let parent_of = |pid: u32| -> Option<u32> {
        // The name in the second field may hold spaces and brackets: the parent is the second field after its end.
        let stat = fs::read_to_string(format!("/proc/{pid}/stat")).ok()?;
        stat.rsplit_once(')')?.1.split_whitespace().nth(1)?.parse().ok()
    };
    let everyone: Vec<(u32, u32)> = numbered("/proc").into_iter().filter_map(|pid| Some((pid, parent_of(pid)?))).collect();
    let mut found = vec![(root, parent_of(root).unwrap_or(0))];
    let mut at = 0;
    while at < found.len() {
        let parent = found[at].0;
        found.extend(everyone.iter().filter(|(_, of)| *of == parent).copied());
        at += 1;
    }
    found
}

/// The threads taken over, and the processes they belong to, each with its parent.
type Taken = (HashMap<u32, Tracee>, Vec<(u32, u32)>);

/// Takes over a running program: every thread of it and of what it has started. They keep running; each reports
/// one stop, which the main loop sees like any other.
fn seize(root: u32) -> Result<Taken, String> {
    let mut tracees = HashMap::new();
    let mut taken = Vec::new();
    for (pid, parent) in family(root) {
        let mut any = false;
        // Until a pass finds no new thread: one may start while the others are being taken.
        loop {
            let mut new = false;
            for tid in numbered(&format!("/proc/{pid}/task")) {
                if tracees.contains_key(&tid) {
                    continue;
                }
                if ptrace(PTRACE_SEIZE, tid, 0, ATTACH_OPTIONS as usize) == -1 {
                    let error = io::Error::last_os_error();
                    if pid == root && tid == root {
                        return Err(match error.raw_os_error() {
                            Some(libc::ESRCH) => format!("there is no running program with the process id {root}"),
                            Some(libc::EPERM) => format!("the program {root} can't be followed: it belongs to someone else, is already being traced, or the system forbids it"),
                            _ => format!("the program {root} couldn't be followed: {error}"),
                        });
                    }
                    continue;
                }
                ptrace(PTRACE_INTERRUPT, tid, 0, 0);
                tracees.insert(tid, Tracee { pid, call: None, new: false, fatal: None });
                (new, any) = (true, true);
            }
            if !new {
                break;
            }
        }
        if any {
            taken.push((pid, parent));
        }
    }
    if !tracees.contains_key(&root) {
        return Err(format!("there is no running program with the process id {root}"));
    }
    Ok((tracees, taken))
}

fn write_state(directory: &Path, state: &Value) {
    let _ = crate::jobs::write_whole(&directory.join("state.json"), state.to_string().as_bytes());
}

/// A session on a program that was already running: taken over as it is, followed until it ends or the session is
/// stopped, and then let go, still running.
fn follow(directory: &Path, request: Request, store: Store, root: u32) -> io::Result<()> {
    let (tracees, taken) = match seize(root) {
        Ok(taken) => taken,
        Err(error) => {
            write_state(directory, &json!({ "state": "failed", "error": error }));
            return Ok(());
        }
    };
    // Nothing of its output passes through here; the files are there so the tools find them, empty.
    File::create(directory.join("stdout.log"))?;
    File::create(directory.join("stderr.log"))?;
    let mut tracer = Tracer { images: Default::default(), stacks: Default::default(), store, pending: Vec::new(), request, tracees, names: HashMap::new(), sockets: HashMap::new(), reported: HashSet::new(), written: 0 };
    for (pid, parent) in taken {
        let program = fs::read_link(format!("/proc/{pid}/exe")).map(|path| path.to_string_lossy().into_owned()).unwrap_or_default();
        let argv: Vec<String> = fs::read(format!("/proc/{pid}/cmdline")).unwrap_or_default().split(|byte| *byte == 0).filter(|part| !part.is_empty()).map(|part| String::from_utf8_lossy(part).into_owned()).collect();
        tracer.event(pid, pid, Class::Process, "attach", &program, json!({ "argv": argv, "parent": parent }), "ok");
    }
    tracer.flush();
    write_state(directory, &json!({ "state": "running", "pid": root, "tracer": std::process::id(), "attached": true }));

    let mut outcome = Value::Null;
    let mut stopped = false;
    let mut status = 0;
    loop {
        // A tracer started through sudo can't be signalled by the agent: it is told to stop by a file as well.
        if STOP.swap(false, Ordering::SeqCst) || directory.join("stop").exists() {
            stopped = true;
            break;
        }
        // SAFETY: status is a local; -1 means anything this process traces.
        let tid = unsafe { libc::waitpid(-1, &mut status, libc::__WALL | libc::WNOHANG) };
        if tid == 0 {
            // Nothing is happening: write out what is held, and look again in a moment.
            tracer.flush();
            std::thread::sleep(std::time::Duration::from_millis(2));
            continue;
        }
        if tid < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        let tid = tid as u32;
        if libc::WIFSTOPPED(status) {
            tracer.stopped(tid, status);
        } else if let Some(how) = tracer.ended(tid, status) {
            if tid == root {
                outcome = how;
            }
        }
        if tracer.pending.len() >= 512 {
            tracer.flush();
        }
    }
    tracer.flush();
    // Leaving lets go of every thread: with the tracer gone they run on as if it had never been there.
    let mut state = json!({ "state": if stopped { "stopped" } else { "exited" }, "pid": root, "events": tracer.written, "attached": true });
    if let Some(how) = outcome.as_object() {
        for (name, value) in how {
            state[name] = value.clone();
        }
    }
    write_state(directory, &state);
    Ok(())
}

/// Runs the session whose request is in `directory`. Returns when the target and everything it started have ended.
pub fn run(directory: &Path) -> io::Result<()> {
    // Started through sudo, to follow a program that was already running: what is written must still belong to the
    // user. Only the identity used for files changes; the right to trace stays.
    // SAFETY: plain calls with plain numbers.
    if unsafe { libc::geteuid() } == 0 {
        let invoker = |name: &str| std::env::var(name).ok().and_then(|id| id.parse::<u32>().ok());
        if let (Some(user), Some(group)) = (invoker("SUDO_UID"), invoker("SUDO_GID")) {
            unsafe {
                libc::setfsgid(group);
                libc::setfsuid(user);
            }
        }
    }
    let request: Request = serde_json::from_slice(&fs::read(directory.join("request.json"))?).map_err(io::Error::other)?;
    let store = Store::open(&directory.join("events.sqlite")).map_err(io::Error::other)?;
    // Without SA_RESTART: a stop request must wake the tracer out of its wait.
    // SAFETY: the handler only sets an atomic flag; the structure is zeroed and then filled as sigaction expects.
    unsafe {
        let mut action: libc::sigaction = std::mem::zeroed();
        action.sa_sigaction = on_stop as extern "C" fn(libc::c_int) as usize;
        libc::sigaction(libc::SIGTERM, &action, std::ptr::null_mut());
        libc::sigaction(libc::SIGINT, &action, std::ptr::null_mut());
    }
    if let Some(pid) = request.attach {
        return follow(directory, request, store, pid);
    }

    let mut command = Command::new(&request.argv[0]);
    command
        .args(&request.argv[1..])
        .envs(request.env.iter().map(|(name, value)| (name, value)))
        .stdin(Stdio::null())
        .stdout(File::create(directory.join("stdout.log"))?)
        .stderr(File::create(directory.join("stderr.log"))?);
    if let Some(cwd) = &request.cwd {
        command.current_dir(cwd);
    }
    // SAFETY: the closure makes one system call and touches no memory. It asks to be traced by this process; the exec
    // that follows then stops the child before the new program runs a single instruction.
    unsafe {
        command.pre_exec(|| if libc::ptrace(libc::PTRACE_TRACEME, 0, 0, 0) == -1 { Err(io::Error::last_os_error()) } else { Ok(()) });
    }
    let child = match command.spawn() {
        Ok(child) => child.id(),
        Err(error) => {
            write_state(directory, &json!({ "state": "failed", "error": format!("the command couldn't be started: {error}") }));
            return Ok(());
        }
    };
    let mut status = 0;
    // SAFETY: waits for our own child; status is a local.
    unsafe { libc::waitpid(child as libc::pid_t, &mut status, 0) };
    if !libc::WIFSTOPPED(status) {
        write_state(directory, &json!({ "state": "failed", "error": "the target ended before it could be followed" }));
        return Ok(());
    }
    ptrace(libc::PTRACE_SETOPTIONS, child, 0, OPTIONS as usize);

    let mut tracer = Tracer {
        images: Default::default(),
        stacks: Default::default(),
        store,
        pending: Vec::new(),
        request,
        tracees: HashMap::from([(child, Tracee { pid: child, call: None, new: false, fatal: None })]),
        names: HashMap::new(),
        sockets: HashMap::new(),
        reported: HashSet::new(),
        written: 0,
    };
    let program = fs::read_link(format!("/proc/{child}/exe")).map(|path| path.to_string_lossy().into_owned()).unwrap_or_else(|_| tracer.request.argv[0].clone());
    let argv = tracer.request.argv.clone();
    tracer.event(child, child, Class::Process, "start", "", json!({ "parent": std::process::id() }), "ok");
    tracer.event(child, child, Class::Process, "exec", &program, json!({ "argv": argv }), "ok");
    tracer.flush();
    write_state(directory, &json!({ "state": "running", "pid": child, "tracer": std::process::id() }));
    ptrace(libc::PTRACE_SYSCALL, child, 0, 0);

    let mut outcome = Value::Null;
    let mut stopped = false;
    loop {
        if STOP.swap(false, Ordering::SeqCst) {
            stopped = true;
            for pid in tracer.tracees.values().map(|tracee| tracee.pid).collect::<HashSet<_>>() {
                // SAFETY: a plain call. The tracees are ours; a pid that is already gone is refused harmlessly.
                unsafe { libc::kill(pid as libc::pid_t, libc::SIGKILL) };
            }
        }
        // First without waiting: when nothing is ready the target is idle, a good moment to write out what is held.
        // SAFETY: status is a local; -1 means any child, and this process has no children but tracees.
        let mut tid = unsafe { libc::waitpid(-1, &mut status, libc::__WALL | libc::WNOHANG) };
        if tid == 0 {
            tracer.flush();
            tid = unsafe { libc::waitpid(-1, &mut status, libc::__WALL) };
        }
        if tid < 0 {
            if io::Error::last_os_error().raw_os_error() == Some(libc::EINTR) {
                continue;
            }
            break;
        }
        let tid = tid as u32;
        if libc::WIFSTOPPED(status) {
            tracer.stopped(tid, status);
        } else if let Some(how) = tracer.ended(tid, status) {
            if tid == child {
                outcome = how;
            }
        }
        if tracer.pending.len() >= 512 {
            tracer.flush();
        }
    }
    tracer.flush();
    let mut state = json!({ "state": if stopped { "stopped" } else { "exited" }, "pid": child, "events": tracer.written });
    if let Some(how) = outcome.as_object() {
        for (name, value) in how {
            state[name] = value.clone();
        }
    }
    write_state(directory, &state);
    Ok(())
}
