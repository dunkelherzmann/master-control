//! Walking a stopped thread's stack on Linux: from where it is, back through every caller, using the unwind tables
//! every program and library carries (`.eh_frame`) — so it works whether or not the code keeps frame pointers.
//! The walking itself is the `framehop` library's; this module tells it what is loaded where and reads the
//! thread's memory for it.

use std::collections::HashMap;
use std::ops::Range;
use std::sync::Arc;

use framehop::x86_64::{CacheX86_64, UnwindRegsX86_64, UnwinderX86_64};
use framehop::{ExplicitModuleSectionInfo, Module, Unwinder};
use object::{Object, ObjectSection};

use super::symbols::Images;

/// What the walker needs of one program file, read once and shared by every process that has it loaded.
struct Tables {
    base: u64,
    text: Option<Range<u64>>,
    got: Option<Range<u64>>,
    frames: Option<(Range<u64>, Arc<[u8]>)>,
    index: Option<(Range<u64>, Arc<[u8]>)>,
}

fn tables(path: &str) -> Option<Tables> {
    if std::fs::metadata(path).ok()?.len() > 1 << 30 {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let file = object::File::parse(&*data).ok()?;
    let place = |name: &str| file.section_by_name(name).map(|section| section.address()..section.address() + section.size());
    let held = |name: &str| {
        let section = file.section_by_name(name)?;
        Some((section.address()..section.address() + section.size(), Arc::from(section.uncompressed_data().ok()?.into_owned())))
    };
    Some(Tables { base: file.relative_address_base(), text: place(".text"), got: place(".got"), frames: held(".eh_frame"), index: held(".eh_frame_hdr") })
}

/// A file as it is loaded in one process.
struct Loaded {
    range: Range<u64>,
    path: String,
    /// Where the file's own address `base` is in this process.
    at: u64,
    base: u64,
}

#[derive(Default)]
struct Process {
    walker: UnwinderX86_64<Arc<[u8]>>,
    cache: CacheX86_64,
    loaded: Vec<Loaded>,
}

#[derive(Default)]
pub struct Stacks {
    files: HashMap<String, Option<Arc<Tables>>>,
    processes: HashMap<u32, Process>,
}

impl Stacks {
    /// Forgets what a process had loaded: it ended, or became another program.
    pub fn forget(&mut self, pid: u32) {
        self.processes.remove(&pid);
    }

    /// Takes in whatever the process has loaded that isn't known yet. True when something was new.
    fn load(&mut self, pid: u32) -> bool {
        let maps = std::fs::read_to_string(format!("/proc/{pid}/maps")).unwrap_or_default();
        // Each file's whole extent, and where its first byte would be.
        let mut found: Vec<(String, Range<u64>, u64, u64)> = Vec::new();
        for line in maps.lines() {
            let mut fields = line.splitn(6, ' ');
            let (Some(range), _, Some(offset), _, _, Some(file)) = (fields.next(), fields.next(), fields.next(), fields.next(), fields.next(), fields.next()) else { continue };
            let file = file.trim().trim_end_matches(" (deleted)");
            let (Some((start, end)), Ok(offset)) = (range.split_once('-'), u64::from_str_radix(offset, 16)) else { continue };
            let (Ok(start), Ok(end)) = (u64::from_str_radix(start, 16), u64::from_str_radix(end, 16)) else { continue };
            if !file.starts_with('/') {
                continue;
            }
            match found.iter_mut().find(|known| known.0 == file) {
                Some(known) => {
                    known.1 = known.1.start.min(start)..known.1.end.max(end);
                    if offset < known.3 {
                        (known.2, known.3) = (start.wrapping_sub(offset), offset);
                    }
                }
                None => found.push((file.to_string(), start..end, start.wrapping_sub(offset), offset)),
            }
        }
        let process = self.processes.entry(pid).or_default();
        let mut new = false;
        for (path, range, first_byte, _) in found {
            if process.loaded.iter().any(|loaded| loaded.path == path && loaded.range == range) {
                continue;
            }
            let Some(tables) = self.files.entry(path.clone()).or_insert_with(|| tables(&path).map(Arc::new)).clone() else { continue };
            let info = ExplicitModuleSectionInfo {
                base_svma: tables.base,
                text_svma: tables.text.clone(),
                got_svma: tables.got.clone(),
                eh_frame_svma: tables.frames.as_ref().map(|held| held.0.clone()),
                eh_frame: tables.frames.as_ref().map(|held| held.1.clone()),
                eh_frame_hdr_svma: tables.index.as_ref().map(|held| held.0.clone()),
                eh_frame_hdr: tables.index.as_ref().map(|held| held.1.clone()),
                ..Default::default()
            };
            // The file's first byte holds its lowest address: that is where its own `base` lies in the process.
            process.walker.add_module(Module::new(path.clone(), range.clone(), first_byte, info));
            process.loaded.push(Loaded { range, path, at: first_byte, base: tables.base });
            new = true;
        }
        new
    }

    /// The stack of a stopped thread, innermost frame first, as `file!function+0x12` (or `file+0x1234` where the
    /// file names no function there). `read` gives one word of the thread's memory.
    pub fn of(&mut self, pid: u32, registers: (u64, u64, u64), read: &mut dyn FnMut(u64) -> Option<u64>, images: &mut Images) -> Vec<String> {
        let (ip, sp, bp) = registers;
        if !self.processes.get(&pid).is_some_and(|process| process.loaded.iter().any(|loaded| loaded.range.contains(&ip))) {
            self.load(pid);
        }
        let mut frames = self.walk(pid, ip, sp, bp, read);
        // A caller in a library loaded since the last look: take it in and walk again.
        let known = |process: &Process, address: u64| process.loaded.iter().any(|loaded| loaded.range.contains(&address));
        if frames.iter().any(|(_, lookup)| !self.processes.get(&pid).is_some_and(|process| known(process, *lookup))) && self.load(pid) {
            frames = self.walk(pid, ip, sp, bp, read);
        }
        let Some(process) = self.processes.get(&pid) else { return Vec::new() };
        frames
            .into_iter()
            .map(|(address, lookup)| match process.loaded.iter().find(|loaded| loaded.range.contains(&lookup)) {
                Some(loaded) => images.frame(&loaded.path, lookup - loaded.at + loaded.base, false, address - loaded.at),
                None => format!("{address:#x}"),
            })
            .collect()
    }

    /// Each frame's address, and the address to look its function up by (a caller's is just before where it will
    /// return to, so it falls inside the call).
    fn walk(&mut self, pid: u32, ip: u64, sp: u64, bp: u64, read: &mut dyn FnMut(u64) -> Option<u64>) -> Vec<(u64, u64)> {
        let Some(process) = self.processes.get_mut(&pid) else { return vec![(ip, ip)] };
        let mut read_stack = |address: u64| read(address).ok_or(());
        let mut walking = process.walker.iter_frames(ip, UnwindRegsX86_64::new(ip, sp, bp), &mut process.cache, &mut read_stack);
        let mut frames = Vec::new();
        while let Ok(Some(frame)) = walking.next() {
            frames.push((frame.address(), frame.address_for_lookup()));
            if frames.len() >= 96 {
                break;
            }
        }
        if frames.is_empty() {
            frames.push((ip, ip));
        }
        frames
    }
}
