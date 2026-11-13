//! A small FAT16/FAT32 driver for the files in a volume's root directory.
//!
//! This is what makes DOSH's drive C: outlive a power cycle: the supervisor
//! reads a disk's root directory into a capsule's volume at launch and
//! writes back what the program changed. Anything beyond that is out of
//! scope. Subdirectories and long file names are left alone (an entry the
//! driver does not understand is never modified), FAT12 is refused, and the
//! sector size must be 512.
//!
//! The driver is ignorant of hardware. It is handed a [`Block`] device.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

pub const SECTOR: usize = 512;
const ENTRY: usize = 32;
const ATTR_LONG_NAME: u8 = 0x0F;
const ATTR_VOLUME: u8 = 0x08;
const ATTR_DIRECTORY: u8 = 0x10;
const DELETED: u8 = 0xE5;
/// 1980-01-01 00:00:00, the first date DOS can name.
const EPOCH_DATE: u16 = 0x0021;

#[derive(Debug, PartialEq, Eq)]
pub enum Error {
    /// The device failed.
    Io,
    /// The disk is not a FAT volume this driver handles.
    Format(&'static str),
    /// No room for the data, or no free directory entry.
    Full,
    /// The name does not fit 8.3.
    Name,
    /// Structure on disk that cannot be right (a loop in a chain, say).
    Corrupt,
}

/// A device of 512-byte sectors.
pub trait Block {
    /// Fill `buf`, a whole number of sectors, from `sector` on.
    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), Error>;
    fn write(&mut self, sector: u64, buf: &[u8]) -> Result<(), Error>;
    fn sectors(&self) -> u64;
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Kind {
    Fat16,
    Fat32,
}

pub struct Volume<B: Block> {
    dev: B,
    kind: Kind,
    sectors_per_cluster: u32,
    fat_start: u64,
    fat_sectors: u32,
    fats: u8,
    /// FAT16: the fixed root directory region. FAT32: unused.
    root_start: u64,
    root_entries: u32,
    /// FAT32: first cluster of the root directory.
    root_cluster: u32,
    data_start: u64,
    clusters: u32,
    label: String,
    /// The FAT sector last read, to make chain walks and scans cheap.
    cache: Option<(u64, [u8; SECTOR])>,
    next_free: u32,
}

fn le16(b: &[u8], at: usize) -> u32 {
    u16::from_le_bytes([b[at], b[at + 1]]) as u32
}
fn le32(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

/// An 11-byte directory name as `NAME.EXT`.
fn display_name(raw: &[u8]) -> String {
    let part = |b: &[u8]| String::from_utf8_lossy(b).trim_end().to_string();
    let (stem, ext) = (part(&raw[..8]), part(&raw[8..11]));
    if ext.is_empty() {
        stem
    } else {
        alloc::format!("{stem}.{ext}")
    }
}

/// `NAME.EXT` as the 11 bytes of a directory entry.
fn short_name(name: &str) -> Result<[u8; 11], Error> {
    let (stem, ext) = name.split_once('.').unwrap_or((name, ""));
    let ok = |s: &str, max: usize| {
        s.len() <= max && s.bytes().all(|b| b.is_ascii_graphic() && !b"\"*+,/:;<=>?[\\]|.".contains(&b))
    };
    if stem.is_empty() || !ok(stem, 8) || !ok(ext, 3) {
        return Err(Error::Name);
    }
    let mut out = [b' '; 11];
    for (i, b) in stem.bytes().enumerate() {
        out[i] = b.to_ascii_uppercase();
    }
    for (i, b) in ext.bytes().enumerate() {
        out[8 + i] = b.to_ascii_uppercase();
    }
    Ok(out)
}

fn first_cluster(kind: Kind, entry: &[u8; ENTRY]) -> u32 {
    let high = if kind == Kind::Fat32 { le16(entry, 20) << 16 } else { 0 };
    high | le16(entry, 26)
}

struct Slot {
    sector: u64,
    offset: usize,
}

impl<B: Block> Volume<B> {
    pub fn open(mut dev: B) -> Result<Self, Error> {
        let mut boot = [0u8; SECTOR];
        dev.read(0, &mut boot)?;
        if boot[510] != 0x55 || boot[511] != 0xAA {
            return Err(Error::Format("no boot sector signature"));
        }
        if le16(&boot, 11) != SECTOR as u32 {
            return Err(Error::Format("sector size is not 512"));
        }
        let sectors_per_cluster = boot[13] as u32;
        let reserved = le16(&boot, 14);
        let fats = boot[16];
        let root_entries = le16(&boot, 17);
        let total = match le16(&boot, 19) {
            0 => le32(&boot, 32),
            n => n,
        };
        let fat_sectors = match le16(&boot, 22) {
            0 => le32(&boot, 36),
            n => n,
        };
        if sectors_per_cluster == 0 || !sectors_per_cluster.is_power_of_two() || fats == 0 || fat_sectors == 0 {
            return Err(Error::Format("implausible BPB"));
        }
        let root_sectors = (root_entries * ENTRY as u32).div_ceil(SECTOR as u32);
        let data_start = (reserved + fats as u32 * fat_sectors + root_sectors) as u64;
        let clusters = (total as u64).saturating_sub(data_start) as u32 / sectors_per_cluster;
        let kind = match clusters {
            0..=4084 => return Err(Error::Format("FAT12 is not supported")),
            4085..=65524 => Kind::Fat16,
            _ => Kind::Fat32,
        };
        let label_at = if kind == Kind::Fat32 { 71 } else { 43 };
        let label = String::from_utf8_lossy(&boot[label_at..label_at + 11]).trim_end().into();
        Ok(Self {
            dev,
            kind,
            sectors_per_cluster,
            fat_start: reserved as u64,
            fat_sectors,
            fats,
            root_start: (reserved + fats as u32 * fat_sectors) as u64,
            root_entries,
            root_cluster: le32(&boot, 44),
            data_start,
            clusters,
            label,
            cache: None,
            next_free: 2,
        })
    }

    /// The volume label from the boot sector.
    pub fn label(&self) -> &str {
        &self.label
    }

    pub fn into_inner(self) -> B {
        self.dev
    }

    // --- the allocation table -------------------------------------------

    fn fat_entry_pos(&self, cluster: u32) -> (u64, usize) {
        let byte = cluster as u64 * if self.kind == Kind::Fat32 { 4 } else { 2 };
        (self.fat_start + byte / SECTOR as u64, (byte % SECTOR as u64) as usize)
    }

    fn fat_sector(&mut self, sector: u64) -> Result<&mut [u8; SECTOR], Error> {
        if self.cache.as_ref().map(|c| c.0) != Some(sector) {
            let mut buf = [0u8; SECTOR];
            self.dev.read(sector, &mut buf)?;
            self.cache = Some((sector, buf));
        }
        Ok(&mut self.cache.as_mut().unwrap().1)
    }

    fn get(&mut self, cluster: u32) -> Result<u32, Error> {
        let (sector, at) = self.fat_entry_pos(cluster);
        let kind = self.kind;
        let buf = self.fat_sector(sector)?;
        Ok(match kind {
            Kind::Fat16 => le16(buf, at),
            Kind::Fat32 => le32(buf, at) & 0x0FFF_FFFF,
        })
    }

    fn set(&mut self, cluster: u32, value: u32) -> Result<(), Error> {
        let (sector, at) = self.fat_entry_pos(cluster);
        let kind = self.kind;
        let buf = self.fat_sector(sector)?;
        match kind {
            Kind::Fat16 => buf[at..at + 2].copy_from_slice(&(value as u16).to_le_bytes()),
            Kind::Fat32 => {
                let keep = le32(buf, at) & 0xF000_0000; // the top four bits are reserved
                buf[at..at + 4].copy_from_slice(&(keep | value & 0x0FFF_FFFF).to_le_bytes());
            }
        }
        let copy = *buf;
        for n in 0..self.fats as u64 {
            self.dev.write(sector + n * self.fat_sectors as u64, &copy)?;
        }
        Ok(())
    }

    fn is_end(&self, entry: u32) -> bool {
        match self.kind {
            Kind::Fat16 => entry >= 0xFFF8,
            Kind::Fat32 => entry >= 0x0FFF_FFF8,
        }
    }

    fn end_mark(&self) -> u32 {
        if self.kind == Kind::Fat16 {
            0xFFFF
        } else {
            0x0FFF_FFFF
        }
    }

    /// Every cluster of a chain, in order. A chain that wanders outside the
    /// volume or revisits a cluster is corrupt.
    fn chain(&mut self, first: u32) -> Result<Vec<u32>, Error> {
        let mut out = Vec::new();
        let mut at = first;
        while at >= 2 {
            if at >= self.clusters + 2 || out.len() as u32 > self.clusters {
                return Err(Error::Corrupt);
            }
            out.push(at);
            at = self.get(at)?;
            if self.is_end(at) {
                break;
            }
        }
        Ok(out)
    }

    fn release(&mut self, first: u32) -> Result<(), Error> {
        for cluster in self.chain(first)? {
            self.set(cluster, 0)?;
            self.next_free = self.next_free.min(cluster);
        }
        Ok(())
    }

    fn allocate(&mut self, count: u32) -> Result<Vec<u32>, Error> {
        let mut found = Vec::with_capacity(count as usize);
        let top = self.clusters + 2;
        let mut at = self.next_free.max(2);
        let mut scanned = 0;
        while (found.len() as u32) < count {
            if scanned >= top {
                break;
            }
            if at >= top {
                at = 2;
            }
            if self.get(at)? == 0 {
                found.push(at);
            }
            at += 1;
            scanned += 1;
        }
        if (found.len() as u32) < count {
            return Err(Error::Full);
        }
        for pair in found.windows(2) {
            self.set(pair[0], pair[1])?;
        }
        let end = self.end_mark();
        if let Some(&last) = found.last() {
            self.set(last, end)?;
            self.next_free = last + 1;
        }
        Ok(found)
    }

    // --- the root directory ---------------------------------------------

    /// The sectors that hold the root directory.
    fn root_sectors(&mut self) -> Result<Vec<u64>, Error> {
        match self.kind {
            Kind::Fat16 => {
                let n = (self.root_entries * ENTRY as u32).div_ceil(SECTOR as u32) as u64;
                Ok((self.root_start..self.root_start + n).collect())
            }
            Kind::Fat32 => {
                let clusters = self.chain(self.root_cluster)?;
                let mut out = Vec::new();
                for c in clusters {
                    let base = self.cluster_sector(c);
                    out.extend(base..base + self.sectors_per_cluster as u64);
                }
                Ok(out)
            }
        }
    }

    fn cluster_sector(&self, cluster: u32) -> u64 {
        self.data_start + (cluster as u64 - 2) * self.sectors_per_cluster as u64
    }

    /// Visit each 32-byte root entry: `(slot, raw bytes)`; stops when `visit`
    /// returns `Some`, or at the directory's end marker.
    fn scan<T>(&mut self, mut visit: impl FnMut(&Slot, &[u8; ENTRY]) -> Option<T>) -> Result<Option<T>, Error> {
        let mut buf = [0u8; SECTOR];
        for sector in self.root_sectors()? {
            self.dev.read(sector, &mut buf)?;
            for offset in (0..SECTOR).step_by(ENTRY) {
                let entry: &[u8; ENTRY] = buf[offset..offset + ENTRY].try_into().unwrap();
                if entry[0] == 0 {
                    return Ok(None);
                }
                if let Some(found) = visit(&Slot { sector, offset }, entry) {
                    return Ok(Some(found));
                }
            }
        }
        Ok(None)
    }

    fn is_file(entry: &[u8; ENTRY]) -> bool {
        entry[0] != DELETED && entry[11] & (ATTR_LONG_NAME | ATTR_VOLUME | ATTR_DIRECTORY) == 0
    }

    fn first_cluster(&self, entry: &[u8; ENTRY]) -> u32 {
        first_cluster(self.kind, entry)
    }

    /// The names of the files in the root directory.
    pub fn names(&mut self) -> Result<Vec<String>, Error> {
        let mut names = Vec::new();
        self.scan::<()>(|_, e| {
            if Self::is_file(e) {
                names.push(display_name(&e[..11]));
            }
            None
        })?;
        Ok(names)
    }

    fn find(&mut self, raw: &[u8; 11]) -> Result<Option<(Slot, [u8; ENTRY])>, Error> {
        self.scan(|slot, e| (Self::is_file(e) && &e[..11] == raw).then_some((Slot { ..*slot }, *e)))
    }

    /// Read every file in the root directory.
    pub fn read_all(&mut self) -> Result<Vec<(String, Vec<u8>)>, Error> {
        let mut entries = Vec::new();
        let kind = self.kind;
        self.scan::<()>(|_, e| {
            if Self::is_file(e) {
                entries.push((display_name(&e[..11]), first_cluster(kind, e), le32(e, 28)));
            }
            None
        })?;
        let mut files = Vec::new();
        for (name, first, size) in entries {
            files.push((name, self.read_chain(first, size as usize)?));
        }
        Ok(files)
    }

    pub fn read(&mut self, name: &str) -> Result<Option<Vec<u8>>, Error> {
        let Some((_, e)) = self.find(&short_name(name)?)? else { return Ok(None) };
        let first = self.first_cluster(&e);
        self.read_chain(first, le32(&e, 28) as usize).map(Some)
    }

    fn read_chain(&mut self, first: u32, size: usize) -> Result<Vec<u8>, Error> {
        let mut data = vec![0u8; size];
        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR;
        let chain = self.chain(first)?;
        if chain.len() * cluster_bytes < size {
            return Err(Error::Corrupt);
        }
        for (cluster, part) in chain.iter().zip(data.chunks_mut(cluster_bytes)) {
            let mut buf = vec![0u8; cluster_bytes];
            self.dev.read(self.cluster_sector(*cluster), &mut buf)?;
            part.copy_from_slice(&buf[..part.len()]);
        }
        Ok(data)
    }

    /// Create `name` with `data`, or replace it. The new contents are written
    /// before the directory entry points at them, so a failure part way
    /// leaves the old file in place (at worst with some free space leaked).
    pub fn write(&mut self, name: &str, data: &[u8]) -> Result<(), Error> {
        let raw = short_name(name)?;
        let existing = self.find(&raw)?;
        let cluster_bytes = self.sectors_per_cluster as usize * SECTOR;
        let count = data.len().div_ceil(cluster_bytes) as u32;
        let chain = if count == 0 { Vec::new() } else { self.allocate(count)? };
        for (cluster, part) in chain.iter().zip(data.chunks(cluster_bytes)) {
            let mut buf = vec![0u8; cluster_bytes];
            buf[..part.len()].copy_from_slice(part);
            self.dev.write(self.cluster_sector(*cluster), &buf)?;
        }
        let first = chain.first().copied().unwrap_or(0);

        let (slot, old_first) = match existing {
            Some((slot, e)) => (slot, self.first_cluster(&e)),
            None => match self.free_slot() {
                Ok(slot) => (slot, 0),
                Err(e) => {
                    if first != 0 {
                        self.release(first)?;
                    }
                    return Err(e);
                }
            },
        };
        let mut sector = [0u8; SECTOR];
        self.dev.read(slot.sector, &mut sector)?;
        let entry = &mut sector[slot.offset..slot.offset + ENTRY];
        entry.fill(0);
        entry[..11].copy_from_slice(&raw);
        entry[11] = 0x20; // archive
        entry[16..18].copy_from_slice(&EPOCH_DATE.to_le_bytes()); // created
        entry[18..20].copy_from_slice(&EPOCH_DATE.to_le_bytes()); // accessed
        entry[24..26].copy_from_slice(&EPOCH_DATE.to_le_bytes()); // written
        entry[20..22].copy_from_slice(&((first >> 16) as u16).to_le_bytes());
        entry[26..28].copy_from_slice(&(first as u16).to_le_bytes());
        entry[28..32].copy_from_slice(&(data.len() as u32).to_le_bytes());
        self.dev.write(slot.sector, &sector)?;
        if old_first != 0 {
            self.release(old_first)?;
        }
        Ok(())
    }

    /// A deleted entry, or the first never-used one. A never-used entry at
    /// the very end of a FAT16 root has no successor to take over the
    /// end-of-directory marker, which is fine: the region simply ends.
    fn free_slot(&mut self) -> Result<Slot, Error> {
        let mut buf = [0u8; SECTOR];
        for sector in self.root_sectors()? {
            self.dev.read(sector, &mut buf)?;
            for offset in (0..SECTOR).step_by(ENTRY) {
                if buf[offset] == 0 || buf[offset] == DELETED {
                    return Ok(Slot { sector, offset });
                }
            }
        }
        Err(Error::Full)
    }

    pub fn delete(&mut self, name: &str) -> Result<bool, Error> {
        let Some((slot, e)) = self.find(&short_name(name)?)? else { return Ok(false) };
        let mut sector = [0u8; SECTOR];
        self.dev.read(slot.sector, &mut sector)?;
        sector[slot.offset] = DELETED;
        self.dev.write(slot.sector, &sector)?;
        let first = self.first_cluster(&e);
        if first != 0 {
            self.release(first)?;
        }
        Ok(true)
    }

    /// Bytes of data space not in use.
    pub fn free_bytes(&mut self) -> Result<u64, Error> {
        let mut free = 0u64;
        for c in 2..self.clusters + 2 {
            if self.get(c)? == 0 {
                free += 1;
            }
        }
        Ok(free * self.sectors_per_cluster as u64 * SECTOR as u64)
    }
}

/// Lay an empty FAT16 file system over `dev`, with a 512-entry root
/// directory and the smallest clusters that keep the cluster count in FAT16's
/// range. For anything fancier, format with a host tool.
pub fn format<B: Block>(dev: &mut B, label: &str) -> Result<(), Error> {
    let total = dev.sectors() as u32;
    let reserved = 1u32;
    let root_entries = 512u32;
    let root_sectors = root_entries * ENTRY as u32 / SECTOR as u32;
    let mut spc = 1u32;
    while total / spc > 65_000 {
        spc *= 2;
    }
    if spc > 64 {
        return Err(Error::Format("disk too large for FAT16"));
    }
    let fat_sectors = (total / spc + 2).div_ceil(256) + 1;
    let data_start = reserved + 2 * fat_sectors + root_sectors;
    if total < data_start || (total - data_start) / spc < 4_085 {
        return Err(Error::Format("disk too small for FAT16 (needs about 2.2 MB)"));
    }
    let mut boot = [0u8; SECTOR];
    boot[0..3].copy_from_slice(&[0xEB, 0x3C, 0x90]);
    boot[3..11].copy_from_slice(b"DOSH    ");
    boot[11..13].copy_from_slice(&(SECTOR as u16).to_le_bytes());
    boot[13] = spc as u8;
    boot[14..16].copy_from_slice(&(reserved as u16).to_le_bytes());
    boot[16] = 2;
    boot[17..19].copy_from_slice(&(root_entries as u16).to_le_bytes());
    boot[21] = 0xF8;
    boot[22..24].copy_from_slice(&(fat_sectors as u16).to_le_bytes());
    boot[32..36].copy_from_slice(&total.to_le_bytes());
    boot[38] = 0x29;
    let mut padded = [b' '; 11];
    for (d, s) in padded.iter_mut().zip(label.bytes()) {
        *d = s.to_ascii_uppercase();
    }
    boot[43..54].copy_from_slice(&padded);
    boot[54..62].copy_from_slice(b"FAT16   ");
    boot[510] = 0x55;
    boot[511] = 0xAA;
    dev.write(0, &boot)?;

    let zero = [0u8; SECTOR];
    for n in 0..2 * fat_sectors + root_sectors {
        dev.write((reserved + n) as u64, &zero)?;
    }
    for n in 0..2 {
        let mut first = [0u8; SECTOR];
        first[0..2].copy_from_slice(&0xFFF8u16.to_le_bytes());
        first[2..4].copy_from_slice(&0xFFFFu16.to_le_bytes());
        dev.write((reserved + n * fat_sectors) as u64, &first)?;
    }
    Ok(())
}

#[cfg(test)]
mod tests;
