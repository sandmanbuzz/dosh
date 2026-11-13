//! Drive C: and the file handles open on it.

use alloc::string::String;
use alloc::vec::Vec;

pub const ERR_FILE_NOT_FOUND: u16 = 2;
pub const ERR_PATH_NOT_FOUND: u16 = 3;
pub const ERR_TOO_MANY_OPEN: u16 = 4;
pub const ERR_ACCESS_DENIED: u16 = 5;
pub const ERR_BAD_HANDLE: u16 = 6;

/// A capsule's drive C:. One flat root directory held in memory; names are
/// stored upper-case, as DOS presents them.
pub struct Volume {
    files: Vec<Option<(String, Vec<u8>)>>,
    capacity: usize,
}

impl Volume {
    /// An empty volume that programs can fill with up to `capacity` bytes
    /// of file data.
    pub fn new(capacity: usize) -> Self {
        Self { files: Vec::new(), capacity }
    }

    /// Bytes programs can still write before the disk is full.
    pub fn free(&self) -> usize {
        let used: usize = self.files.iter().flatten().map(|(_, data)| data.len()).sum();
        self.capacity.saturating_sub(used)
    }

    /// Add or replace a file, regardless of capacity: this is the operator
    /// stocking the disk, not a program writing to it. Returns its slot.
    pub fn put(&mut self, name: &str, data: Vec<u8>) -> usize {
        let entry = Some((name.to_ascii_uppercase(), data));
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
        self.files.iter().flatten().map(|(name, _)| name.as_str())
    }

    pub(crate) fn slot(&self, name: &str) -> Option<usize> {
        self.files.iter().position(|f| matches!(f, Some((n, _)) if n.eq_ignore_ascii_case(name)))
    }

    pub(crate) fn remove(&mut self, slot: usize) {
        self.files[slot] = None;
    }

    pub(crate) fn data(&self, slot: usize) -> &[u8] {
        self.files[slot].as_ref().map_or(&[], |(_, data)| data)
    }

    /// The contents of an open file; `None` once it has been deleted.
    pub(crate) fn data_mut(&mut self, slot: usize) -> Option<&mut Vec<u8>> {
        self.files[slot].as_mut().map(|(_, data)| data)
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
    Ok(match name.split('.').next().unwrap_or("").trim_end_matches(':') {
        "CON" => Target::Console,
        "NUL" => Target::Null,
        _ => Target::File(name),
    })
}

#[derive(Clone, Copy)]
pub(crate) enum Handle {
    Console,
    Null,
    File { slot: usize, pos: u32, mode: u8 },
}

pub(crate) const MODE_READ: u8 = 0;
pub(crate) const MODE_WRITE: u8 = 1;
