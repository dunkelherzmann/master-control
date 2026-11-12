//! Catching a crash on macOS as it happens, to read the stack that led to it.
//!
//! The system's own crash report has that stack, but macOS writes no report for a program that crashes again and
//! again, which is exactly what a program under development does. So the recorder asks to be told first: it sets
//! itself as the handler for fatal crashes, and everything it starts inherits that. When the target (or anything
//! the target starts) is about to die of a crash, the kernel hands the recorder the crashed thread and its
//! process; the stack is read there and then, and the crash is passed on, to go the way it always goes.
//!
//! This needs no admin rights: the handler is inherited, and the kernel itself hands over the right to look. It
//! doesn't reach a program that was already running when the session started (nothing was inherited), nor one
//! that changes user on the way (the system resets the handler there).

use std::collections::HashMap;
use std::ffi::{c_void, CStr};
use std::sync::mpsc::{self, Receiver};

use super::symbols::{short, Images};

extern "C" {
    static mach_task_self_: u32;
    fn mach_port_allocate(task: u32, right: i32, name: *mut u32) -> i32;
    fn mach_port_insert_right(task: u32, name: u32, right: u32, right_type: u32) -> i32;
    fn mach_port_deallocate(task: u32, name: u32) -> i32;
    fn task_set_exception_ports(task: u32, mask: u32, port: u32, behavior: u32, flavor: i32) -> i32;
    fn mach_msg(message: *mut u8, option: i32, send_size: u32, receive_size: u32, receive_on: u32, timeout: u32, notify: u32) -> i32;
    fn thread_get_state(thread: u32, flavor: i32, state: *mut u32, count: *mut u32) -> i32;
    fn pid_for_task(task: u32, pid: *mut i32) -> i32;
    fn task_info(task: u32, flavor: u32, info: *mut i32, count: *mut u32) -> i32;
    fn mach_vm_read_overwrite(task: u32, address: u64, size: u64, into: u64, read: *mut u64) -> i32;
}

/// A process is dying of a crash: sent after its own signal handlers have had their say.
const EXC_MASK_CRASH: u32 = 1 << 10;
/// The message carries the thread and the task; codes are 64 bits wide.
const BEHAVIOUR: u32 = 1 | 0x8000_0000;
const ARM_THREAD_STATE64: i32 = 6;
const TASK_DYLD_INFO: u32 = 17;
const KERN_FAILURE: i32 = 5;

/// Bytes of another process's memory. None where it has none there.
fn read(task: u32, address: u64, length: usize) -> Option<Vec<u8>> {
    let mut bytes = vec![0u8; length];
    let mut got = 0u64;
    // SAFETY: the buffer is ours and its length is given; the kernel checks the other process's side.
    let outcome = unsafe { mach_vm_read_overwrite(task, address, length as u64, bytes.as_mut_ptr() as u64, &mut got) };
    (outcome == 0 && got as usize == length).then_some(bytes)
}

fn word(task: u32, address: u64) -> Option<u64> {
    read(task, address, 8).map(|bytes| u64::from_le_bytes(bytes.try_into().expect("eight bytes")))
}

/// What a process has loaded: where each program file or library starts, and its path.
fn images(task: u32) -> Vec<(u64, String)> {
    // The process's own list, kept by the loader: its address comes from the kernel.
    let mut info = [0i32; 5];
    let mut count = 5u32;
    // SAFETY: the array is the size the call is told.
    if unsafe { task_info(task, TASK_DYLD_INFO, info.as_mut_ptr(), &mut count) } != 0 {
        return Vec::new();
    }
    let list = u64::from(info[0] as u32) | (u64::from(info[1] as u32) << 32);
    // version (4), count (4), then where the entries are; each entry: where it is loaded, where its path is, a date.
    let Some(head) = read(task, list, 16) else { return Vec::new() };
    let entries = u32::from_le_bytes(head[4..8].try_into().expect("four bytes")).min(4096) as u64;
    let array = u64::from_le_bytes(head[8..16].try_into().expect("eight bytes"));
    let mut found: Vec<(u64, String)> = (0..entries)
        .filter_map(|index| {
            let entry = read(task, array + index * 24, 16)?;
            let at = u64::from_le_bytes(entry[..8].try_into().expect("eight bytes"));
            let path_at = u64::from_le_bytes(entry[8..].try_into().expect("eight bytes"));
            // A path, up to its zero; read in a piece that can't run off the end of its page by much.
            let path = [512usize, 128, 32].iter().find_map(|length| read(task, path_at, *length)).map(|bytes| {
                let end = bytes.iter().position(|byte| *byte == 0).unwrap_or(bytes.len());
                String::from_utf8_lossy(&bytes[..end]).into_owned()
            })?;
            Some((at, path))
        })
        .collect();
    found.sort_by_key(|image| image.0);
    found
}

/// The name of a place in one of the system's own libraries, which live in a store shared by every process at the
/// same addresses: what this process calls that address is what the crashed one calls it.
fn shared_name(address: u64) -> Option<(String, u64)> {
    let mut info: libc::Dl_info = unsafe { std::mem::zeroed() };
    // SAFETY: the structure is filled by the call; the names it points to belong to loaded libraries.
    unsafe {
        if libc::dladdr(address as *const c_void, &mut info) == 0 || info.dli_sname.is_null() {
            return None;
        }
        let name = CStr::from_ptr(info.dli_sname).to_string_lossy().into_owned();
        Some((name, address - info.dli_saddr as u64))
    }
}

/// A user-space address as it is: on some Macs return addresses carry a signature in their upper bits.
fn plain(address: u64) -> u64 {
    address & 0x0000_7fff_ffff_ffff
}

/// The stack of a crashed thread, innermost frame first, as `file!function+0x12`.
fn stack(task: u32, thread: u32, files: &mut Images) -> Vec<String> {
    // x0 to x28, the frame pointer, the link register, the stack pointer, the program counter, then flags.
    let mut state = [0u64; 34];
    let mut count = 68u32;
    // SAFETY: the array is the size the call is told (in 32-bit units).
    if unsafe { thread_get_state(thread, ARM_THREAD_STATE64, state.as_mut_ptr().cast(), &mut count) } != 0 {
        return Vec::new();
    }
    let (mut frame, link, counter) = (state[29], plain(state[30]), plain(state[32]));
    let loaded = images(task);
    let mut name = |address: u64, innermost: bool| -> (String, Option<String>) {
        // A caller's frame holds the address after its call; the call itself is before it.
        let inside = if innermost { address } else { address.saturating_sub(1) };
        let Some((at, path)) = loaded.iter().rev().find(|image| image.0 <= inside) else { return (format!("{address:#x}"), None) };
        let file = short(path);
        let function = if std::path::Path::new(path).is_file() { files.function_loaded(path, inside - at) } else { shared_name(inside) };
        match function {
            Some((function, into)) => (format!("{file}!{}+{into:#x}", function.trim_start_matches('_')), Some(function)),
            None => (format!("{file}+{:#x}", address - at), None),
        }
    };
    let (first, first_function) = name(counter, true);
    let mut frames = vec![first];
    // The callers, by the chain of frames every function on this system keeps: each holds the frame before it and
    // where to return to.
    let mut callers = Vec::new();
    for _ in 0..96 {
        let (Some(next), Some(back)) = (word(task, frame), word(task, frame.wrapping_add(8))) else { break };
        let back = plain(back);
        if back < 0x1000 {
            break;
        }
        callers.push(back);
        if next <= frame || next - frame > 0x100_0000 {
            break;
        }
        frame = next;
    }
    // A function that calls nothing keeps no frame of its own: its caller is then only in the link register. That
    // register is believed when it names another function than the one that crashed and isn't the chain's first;
    // where neither place has a name, when it is well away from where the crash is (a stale value points back into
    // the same function).
    if link >= 0x1000 && callers.first() != Some(&link) {
        let (shown, function) = name(link, false);
        let elsewhere = match (&function, &first_function) {
            (None, None) => link.abs_diff(counter) > 0x400,
            _ => function != first_function,
        };
        if elsewhere {
            frames.push(shown);
        }
    }
    frames.extend(callers.into_iter().map(|address| name(address, false).0));
    frames
}

/// Starts catching fatal crashes of everything this process starts from now on. What comes out is, for each crash,
/// the process and the stack of its crashed thread. None where this can't be set up.
pub fn listen() -> Option<Receiver<(u32, Vec<String>)>> {
    let mut port = 0u32;
    // SAFETY: plain calls on this process's own task; the port is made, given a send right, and set as the handler.
    unsafe {
        let own = mach_task_self_;
        // 1: a right to receive. 20: make a send right from it.
        if mach_port_allocate(own, 1, &mut port) != 0 || mach_port_insert_right(own, port, port, 20) != 0 {
            return None;
        }
        if task_set_exception_ports(own, EXC_MASK_CRASH, port, BEHAVIOUR, ARM_THREAD_STATE64) != 0 {
            return None;
        }
    }
    let (sender, caught) = mpsc::channel();
    std::thread::spawn(move || {
        let mut files = Images::default();
        let mut seen: HashMap<u32, ()> = HashMap::new();
        loop {
            // The header (24 bytes), a count of two port rights, the thread and the task (12 bytes each), how
            // numbers are written (8), the kind of fault (4), how many codes (4), the codes.
            let mut message = [0u8; 1024];
            // SAFETY: the buffer's length is given. 2: receive.
            if unsafe { mach_msg(message.as_mut_ptr(), 2, 0, message.len() as u32, port, 0, 0) } != 0 {
                break;
            }
            let field = |at: usize| u32::from_le_bytes(message[at..at + 4].try_into().expect("four bytes"));
            let (bits, reply_to, id, thread, task) = (field(0), field(8), field(20), field(28), field(40));
            let mut pid = 0i32;
            // SAFETY: the task right came in the message; pid is a local.
            if unsafe { pid_for_task(task, &mut pid) } == 0 && pid > 0 && seen.insert(pid as u32, ()).is_none() {
                let frames = stack(task, thread, &mut files);
                if !frames.is_empty() {
                    let _ = sender.send((pid as u32, frames));
                }
            }
            // "Not handled": the crash goes on to whoever is next, and the process dies of it as it would have.
            let mut reply = [0u8; 36];
            reply[0..4].copy_from_slice(&(bits & 0x1f).to_le_bytes());
            reply[4..8].copy_from_slice(&36u32.to_le_bytes());
            reply[8..12].copy_from_slice(&reply_to.to_le_bytes());
            reply[20..24].copy_from_slice(&(id + 100).to_le_bytes());
            reply[24..32].copy_from_slice(&message[52..60]);
            reply[32..36].copy_from_slice(&KERN_FAILURE.to_le_bytes());
            // SAFETY: the reply is laid out as the kernel expects and its length is given. 1: send. The rights that
            // came with the message are given back.
            unsafe {
                mach_msg(reply.as_mut_ptr(), 1, 36, 0, 0, 0, 0);
                mach_port_deallocate(mach_task_self_, thread);
                mach_port_deallocate(mach_task_self_, task);
            }
        }
    });
    Some(caught)
}
