//! Drive C: and the file handles open on it.

use alloc::rc::Rc;
use alloc::string::String;
use alloc::vec::Vec;

pub const ERR_FILE_NOT_FOUND: u16 = 2;
pub const ERR_PATH_NOT_FOUND: u16 = 3;
pub const ERR_TOO_MANY_OPEN: u16 = 4;
pub const ERR_ACCESS_DENIED: u16 = 5;
pub const ERR_BAD_HANDLE: u16 = 6;
pub const ERR_NO_MORE_FILES: u16 = 18;

/// A DOS file time and date, as INT 21h/57h exchanges them.
pub(crate) type Stamp = (u16, u16);
/// What a file is stamped with until a program says otherwise: the machine
/// has no calendar, so 00:00:00 on 1980-01-01, the first date DOS can name.
const EPOCH: Stamp = (0, 0x0021);

/// A capsule's drive C:. One flat root directory held in memory; names are
/// stored upper-case, as DOS presents them.
pub struct Volume {
    files: Vec<Option<(String, Data, Stamp)>>,
    capacity: usize,
}

/// A file's contents. Stocked files are shared with whoever stocked them,
/// and copied only when a program first writes to one.
enum Data {
    Shared(Rc<[u8]>),
    Owned(Vec<u8>),
}

impl Data {
    fn bytes(&self) -> &[u8] {
        match self {
            Data::Shared(data) => data,
            Data::Owned(data) => data,
        }
    }
}

impl Volume {
    /// An empty volume that programs can fill with up to `capacity` bytes
    /// of file data.
    pub fn new(capacity: usize) -> Self {
        Self { files: Vec::new(), capacity }
    }

    /// Bytes the volume holds when full.
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    /// Bytes programs can still write before the disk is full.
    pub fn free(&self) -> usize {
        let used: usize = self.files.iter().flatten().map(|(_, data, _)| data.bytes().len()).sum();
        self.capacity.saturating_sub(used)
    }

    /// Add or replace a file, regardless of capacity: this is the operator
    /// stocking the disk, not a program writing to it. Returns its slot.
    pub fn put(&mut self, name: &str, data: Vec<u8>) -> usize {
        self.insert(name, Data::Owned(data))
    }

    /// Like [`put`](Self::put), sharing `data` instead of copying it.
    pub fn put_shared(&mut self, name: &str, data: Rc<[u8]>) -> usize {
        self.insert(name, Data::Shared(data))
    }

    /// Whether the file is still the very contents it was stocked with:
    /// no program has opened it for writing since.
    pub fn untouched(&self, name: &str) -> bool {
        self.slot(name).is_some_and(|slot| matches!(self.files[slot], Some((_, Data::Shared(_), _))))
    }

    fn insert(&mut self, name: &str, data: Data) -> usize {
        let entry = Some((name.to_ascii_uppercase(), data, EPOCH));
        match self.slot(name) {
            Some(slot) => {
                self.files[slot] = entry;
                slot
            }
            None => {
                self.files.push(entry);
                self.files.len() - 1
            }
        }
    }

    pub fn get(&self, name: &str) -> Option<&[u8]> {
        self.slot(name).map(|slot| self.data(slot))
    }

    pub fn names(&self) -> impl Iterator<Item = &str> {
        self.files.iter().flatten().map(|(name, ..)| name.as_str())
    }

    /// The first file at or after slot `from`: its slot, name and size.
    pub(crate) fn next_from(&self, from: usize) -> Option<(usize, &str, usize)> {
        (from..self.files.len()).find_map(|i| self.files[i].as_ref().map(|(n, d, _)| (i, n.as_str(), d.bytes().len())))
    }

    pub(crate) fn stamp(&self, slot: usize) -> Stamp {
        self.files[slot].as_ref().map_or(EPOCH, |f| f.2)
    }

    pub(crate) fn set_stamp(&mut self, slot: usize, stamp: Stamp) {
        if let Some(f) = self.files[slot].as_mut() {
            f.2 = stamp;
        }
    }

    pub(crate) fn slot(&self, name: &str) -> Option<usize> {
        self.files.iter().position(|f| matches!(f, Some((n, ..)) if n.eq_ignore_ascii_case(name)))
    }

    pub(crate) fn rename(&mut self, slot: usize, name: &str) {
        if let Some(f) = self.files[slot].as_mut() {
            f.0 = name.to_ascii_uppercase();
        }
    }

    pub(crate) fn remove(&mut self, slot: usize) {
        self.files[slot] = None;
    }

    pub(crate) fn data(&self, slot: usize) -> &[u8] {
        self.files[slot].as_ref().map_or(&[], |(_, data, _)| data.bytes())
    }

    /// The contents of an open file; `None` once it has been deleted.
    pub(crate) fn data_mut(&mut self, slot: usize) -> Option<&mut Vec<u8>> {
        let (_, data, _) = self.files[slot].as_mut()?;
        if let Data::Shared(shared) = data {
            *data = Data::Owned(shared.to_vec());
        }
        match data {
            Data::Owned(data) => Some(data),
            Data::Shared(_) => unreachable!("just made owned"),
        }
    }
}

/// What a guest path names.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Console,
    Null,
    File(String),
}

pub(crate) fn resolve(raw: &[u8]) -> Result<Target, u16> {
    parse(raw, false)
}

/// Like [`resolve`], but `*` and `?` are allowed: the path is a search pattern.
pub(crate) fn resolve_pattern(raw: &[u8]) -> Result<Target, u16> {
    parse(raw, true)
}

/// A file name is what the disk can hold: 8.3, upper case, no separators or
/// punctuation FAT reserves. Like DOS, an over-long part is cut, not refused.
fn parse(raw: &[u8], wild: bool) -> Result<Target, u16> {
    let mut path = raw;
    if path.len() >= 2 && path[1] == b':' {
        if !path[0].eq_ignore_ascii_case(&b'C') {
            return Err(ERR_PATH_NOT_FOUND);
        }
        path = &path[2..];
    }
    if let [b'\\' | b'/', rest @ ..] = path {
        path = rest;
    }
    if path.iter().any(|b| matches!(b, b'\\' | b'/')) {
        return Err(ERR_PATH_NOT_FOUND); // no subdirectories yet
    }
    let name = core::str::from_utf8(path).map_err(|_| ERR_FILE_NOT_FOUND)?.to_ascii_uppercase();
    if name.is_empty() {
        return Err(ERR_FILE_NOT_FOUND);
    }
    // Device names win over files, with or without an extension.
    match name.split('.').next().unwrap_or("").trim_end_matches(':') {
        "CON" => return Ok(Target::Console),
        "NUL" => return Ok(Target::Null),
        _ => {}
    }
    let (stem, ext) = name.split_once('.').unwrap_or((&name, ""));
    let bad = |b: char| !b.is_ascii_graphic() || "\"+,/:;<=>[\\]|.".contains(b) || (!wild && matches!(b, '*' | '?'));
    if stem.is_empty() || stem.chars().chain(ext.chars()).any(bad) {
        return Err(ERR_PATH_NOT_FOUND);
    }
    let cut = |s: &str, n: usize| s.chars().take(n).collect::<String>();
    let (stem, ext) = (cut(stem, 8), cut(ext, 3));
    Ok(Target::File(if ext.is_empty() { stem } else { alloc::format!("{stem}.{ext}") }))
}

#[derive(Clone, Copy)]
pub(crate) enum Handle {
    Console,
    Null,
    File { slot: usize, pos: u32, mode: u8 },
}

pub(crate) const MODE_READ: u8 = 0;
pub(crate) const MODE_WRITE: u8 = 1;

/// A name as the 11 bytes of an FCB: eight of name, three of extension, space
/// padded, upper case. `*` fills the rest of its field with `?`.
pub(crate) fn fcb_name(name: &str) -> [u8; 11] {
    let mut out = [b' '; 11];
    let (stem, ext) = name.split_once('.').unwrap_or((name, ""));
    for (field, text, width) in [(0, stem, 8), (8, ext, 3)] {
        let mut n = 0;
        for b in text.bytes().map(|b| b.to_ascii_uppercase()) {
            if b == b'*' {
                out[field + n..field + width].fill(b'?');
                n = width;
            } else if n < width {
                out[field + n] = b;
                n += 1;
            }
        }
    }
    out
}

/// Whether `name` matches an FCB-form wildcard `pattern`.
pub(crate) fn matches(pattern: &[u8; 11], name: &str) -> bool {
    let name = fcb_name(name);
    pattern.iter().zip(name).all(|(&p, n)| p == b'?' || p == n)
}
