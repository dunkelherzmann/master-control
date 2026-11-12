//! Names for places in a program's code: which function an address in a program file or a library belongs to, read
//! from the file's own symbol tables. Each file is read once per session.

use std::collections::HashMap;

use object::{Object, ObjectSegment, ObjectSymbol};

struct Image {
    /// The address the file's first byte has, as the file itself counts.
    base: u64,
    /// The parts of the file that are loaded: where each starts in the file, how long it is, and its address.
    segments: Vec<(u64, u64, u64)>,
    /// Functions by address, with their length where the file gives it.
    symbols: Vec<(u64, u64, String)>,
}

#[derive(Default)]
pub struct Images {
    files: HashMap<String, Option<Image>>,
}

/// The file's short name, as frames show it.
pub fn short(path: &str) -> &str {
    path.rsplit(['/', '\\']).next().unwrap_or(path)
}

fn read(path: &str) -> Option<Image> {
    // A program larger than this is not read for its names: the frames then say where in the file, which is enough
    // to look the name up later.
    if std::fs::metadata(path).ok()?.len() > 1 << 30 {
        return None;
    }
    let data = std::fs::read(path).ok()?;
    let file = object::File::parse(&*data).ok()?;
    let segments = file.segments().map(|segment| (segment.file_range().0, segment.file_range().1, segment.address())).collect();
    let mut symbols: Vec<(u64, u64, String)> = file
        .symbols()
        .chain(file.dynamic_symbols())
        .filter(|symbol| symbol.kind() == object::SymbolKind::Text && symbol.address() != 0)
        .filter_map(|symbol| Some((symbol.address(), symbol.size(), format!("{:#}", rustc_demangle::demangle(symbol.name().ok()?)))))
        .collect();
    // A Windows library names what it offers in its export table rather than in a symbol table.
    if let Ok(exports) = file.exports() {
        symbols.extend(exports.filter_map(|export| {
            let export = export.ok()?;
            match (export.name(), export.target()) {
                (object::NameOrOrdinal::Name(name), object::ExportTarget::Address { address, .. }) => Some((address, 0, String::from_utf8_lossy(name).into_owned())),
                _ => None,
            }
        }));
    }
    // A program built on Windows keeps its functions' names apart, in the PDB file it points to.
    #[cfg(windows)]
    symbols.extend(from_pdb(&file, path));
    // Where two sources name the same place, the one that says how long the function is comes first.
    symbols.sort_by_key(|symbol| (symbol.0, std::cmp::Reverse(symbol.1)));
    symbols.dedup_by_key(|symbol| symbol.0);
    // Where the file's first byte lies, as the file counts: for a Mac program that is its first loaded part's
    // address (the format has no base of its own), elsewhere what the format says.
    let segments: Vec<(u64, u64, u64)> = segments;
    let base = match file.relative_address_base() {
        0 => segments.iter().find(|(start, length, _)| *start == 0 && *length > 0).map_or(0, |segment| segment.2),
        base => base,
    };
    Some(Image { base, segments, symbols })
}

/// The functions a program's PDB file names, at the addresses the program file itself uses.
#[cfg(windows)]
fn from_pdb(file: &object::File, path: &str) -> Vec<(u64, u64, String)> {
    use pdb::FallibleIterator;
    let base = file.relative_address_base();
    // Where the program says its PDB is; failing that, beside the program under its own name.
    let said = file.pdb_info().ok().flatten().map(|info| String::from_utf8_lossy(info.path()).into_owned());
    let beside = std::path::Path::new(path).with_extension("pdb").to_string_lossy().into_owned();
    // A program often records only the PDB's file name: it is then looked for beside the program, and in the `deps`
    // folder beside it, where cargo leaves it.
    let folder = std::path::Path::new(path).parent().unwrap_or(std::path::Path::new("."));
    let mut candidates: Vec<String> = Vec::new();
    for name in said.into_iter() {
        let bare = std::path::Path::new(&name).file_name().map(|bare| bare.to_owned());
        candidates.push(name);
        if let Some(bare) = bare {
            candidates.push(folder.join(&bare).to_string_lossy().into_owned());
            candidates.push(folder.join("deps").join(&bare).to_string_lossy().into_owned());
        }
    }
    candidates.push(beside);
    // None is the usual case for Windows' own libraries, which name what they offer in their export tables.
    let Some(opened) = candidates.iter().find_map(|candidate| std::fs::File::open(candidate).ok()) else { return Vec::new() };
    let read = || -> pdb::Result<Vec<(u64, u64, String)>> {
        let mut pdb = pdb::PDB::open(opened)?;
        let places = pdb.address_map()?;
        let mut found = Vec::new();
        let table = pdb.global_symbols()?;
        let mut symbols = table.iter();
        while let Some(symbol) = symbols.next()? {
            if let Ok(pdb::SymbolData::Public(public)) = symbol.parse() {
                if let (true, Some(at)) = (public.function, public.offset.to_rva(&places)) {
                    found.push((base + u64::from(at.0), 0, format!("{:#}", rustc_demangle::demangle(&public.name.to_string()))));
                }
            }
        }
        // Functions that aren't visible outside their own part of the program are listed with that part.
        let parts = pdb.debug_information()?;
        let mut modules = parts.modules()?;
        while let Some(module) = modules.next()? {
            let Some(info) = pdb.module_info(&module)? else { continue };
            let mut symbols = info.symbols()?;
            while let Some(symbol) = symbols.next()? {
                if let Ok(pdb::SymbolData::Procedure(procedure)) = symbol.parse() {
                    if let Some(at) = procedure.offset.to_rva(&places) {
                        found.push((base + u64::from(at.0), u64::from(procedure.len), format!("{:#}", rustc_demangle::demangle(&procedure.name.to_string()))));
                    }
                }
            }
        }
        Ok(found)
    };
    match read() {
        Ok(found) => found,
        Err(error) => {
            eprintln!("observe-run: the PDB for {path} couldn't be read ({error}): its own functions go unnamed");
            Vec::new()
        }
    }
}

impl Images {
    /// The function at an offset from where the file was loaded, and how far into it.
    pub fn function_loaded(&mut self, path: &str, offset: u64) -> Option<(String, u64)> {
        let base = self.files.entry(path.to_string()).or_insert_with(|| read(path)).as_ref().map_or(0, |image| image.base);
        self.function(path, base + offset, false)
    }

    /// A frame for a place given as an offset from where the file was loaded, as Windows gives them.
    pub fn frame_loaded(&mut self, path: &str, offset: u64, shown_offset: u64) -> String {
        let base = self.files.entry(path.to_string()).or_insert_with(|| read(path)).as_ref().map_or(0, |image| image.base);
        self.frame(path, base + offset, false, shown_offset)
    }

    /// The function at an address of a file, and how far into it. `relative` says how the place is given: as an
    /// offset into the file (true) or into the file as loaded, from its base (false).
    pub fn function(&mut self, path: &str, place: u64, in_file: bool) -> Option<(String, u64)> {
        let image = self.files.entry(path.to_string()).or_insert_with(|| read(path)).as_ref()?;
        let address = if in_file {
            let (start, _, address) = image.segments.iter().find(|(start, length, _)| (*start..start + length).contains(&place))?;
            address + (place - start)
        } else {
            place
        };
        let at = image.symbols.partition_point(|symbol| symbol.0 <= address).checked_sub(1)?;
        let (start, length, name) = &image.symbols[at];
        // Where the file gives no length, a name is believed only close to where it starts.
        let inside = if *length > 0 { address < start + length } else { address - start < 0x10_000 };
        inside.then(|| (name.clone(), address - start))
    }

    /// A frame as events show it: `file!function+0x12`, or `file+0x1234` where no function is named.
    pub fn frame(&mut self, path: &str, place: u64, in_file: bool, shown_place: u64) -> String {
        match self.function(path, place, in_file) {
            Some((name, into)) => format!("{}!{name}+{into:#x}", short(path)),
            None => format!("{}+{shown_place:#x}", short(path)),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// This test program is itself a program with names in it.
    #[test]
    fn a_function_of_a_real_program_is_named() {
        let own = std::env::current_exe().unwrap();
        let path = own.to_str().unwrap();
        let mut images = Images::default();
        assert!(images.files.is_empty());
        // Whatever the format here (ELF, Mach-O, PE), the file is read and has functions with names.
        let read = read(path).expect("the test program can be read");
        assert!(read.symbols.len() > 100 && !read.segments.is_empty(), "{} symbols", read.symbols.len());
        let (address, _, name) = read.symbols.iter().find(|symbol| symbol.2.contains("a_function_of_a_real_program_is_named")).expect("this very function is in the table").clone();
        assert_eq!(images.function(path, address + 1, false), Some((name.clone(), 1)));
        // The same place, given as an offset from where the file would be loaded.
        let base = read.base;
        assert_eq!(images.function_loaded(path, address + 1 - base), Some((name.clone(), 1)));
        assert!(!name.contains("17h"), "names are given without the compiler's hash: {name}");
        assert_eq!(short("/usr/lib/libc.so.6"), "libc.so.6");
        assert_eq!(short("C:\\Windows\\System32\\ntdll.dll"), "ntdll.dll");
        assert!(images.frame("/no/such/file", 0x10, true, 0x10).ends_with("file+0x10"));
    }
}
