//! File control blocks: the CP/M-style file interface, and INT 21h function
//! 29h, parse filename.
//!
//! An FCB carries all of an open file's state in the program's own memory:
//! block and record cursors, record size, file size. It names its file, and
//! each call finds the file by that name, so there is nothing for DOS to keep
//! (and nothing to leak) between calls.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::mem::Mem;
use crate::{files, Regs, Volume};

/// Offsets into an FCB.
const NAME: u16 = 1;
const BLOCK: u16 = 0x0C;
const RECORD_SIZE: u16 = 0x0E;
const FILE_SIZE: u16 = 0x10;
const DATE: u16 = 0x14;
const TIME: u16 = 0x16;

/// The file an FCB names, if it is on drive C: and plain (no wildcards).
fn name_of(mem: &Mem, seg: u16, off: u16) -> Option<String> {
    let drive = mem.r8(seg, off);
    if drive != 0 && drive != 3 {
        return None;
    }
    let mut raw = [0u8; 11];
    mem.read(seg, off + NAME, &mut raw);
    if raw.contains(&b'?') {
        return None;
    }
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim_end().to_string();
    let (stem, ext) = (text(&raw[..8]), text(&raw[8..]));
    Some(if ext.is_empty() { stem } else { format!("{stem}.{ext}") })
}

/// Open (0Fh) or create (16h) the file named by the FCB at `DS:DX`.
/// Returns `AL`: 0 on success, FFh if the file cannot be opened.
pub(crate) fn open(r: &mut Regs, mem: &mut Mem, volume: &mut Volume, create: bool) {
    let (seg, off) = (r.ds, r.dx);
    let Some(name) = name_of(mem, seg, off) else { return r.set_al(0xFF) };
    let slot = match (volume.slot(&name), create) {
        (_, true) => volume.put(&name, Vec::new()),
        (Some(slot), false) => slot,
        (None, false) => return r.set_al(0xFF),
    };
    let size = volume.data(slot).len() as u32;
    let (time, date) = volume.stamp(slot);
    mem.w16(seg, off + BLOCK, 0);
    mem.w16(seg, off + RECORD_SIZE, 0x80);
    mem.w16(seg, off + FILE_SIZE, size as u16);
    mem.w16(seg, off + FILE_SIZE + 2, (size >> 16) as u16);
    mem.w16(seg, off + DATE, date);
    mem.w16(seg, off + TIME, time);
    r.set_al(0);
}

const SEPARATORS: &[u8] = b":.;,=+ \t/\"[]<>|";

fn is_terminator(b: u8) -> bool {
    b < b' ' || SEPARATORS.contains(&b)
}

/// Parse the name at `DS:SI` into the FCB at `ES:DI`. AL bit 0 skips leading
/// separators; bits 1-3 leave the drive, name or extension untouched when
/// the string does not give one. Returns `AL` (0 plain, 1 wildcards, FFh bad
/// drive) and advances `SI` past what was parsed.
pub(crate) fn parse_filename(r: &mut Regs, mem: &mut Mem) {
    let flags = r.al();
    let (ds, es, di) = (r.ds, r.es, r.di);
    let mut si = r.si;
    let peek = |mem: &Mem, si: u16| mem.r8(ds, si);

    if flags & 1 != 0 {
        while matches!(peek(mem, si), b' ' | b'\t' | b';' | b',' | b'=' | b'+') {
            si = si.wrapping_add(1);
        }
    }
    let mut wild = false;
    let mut bad_drive = false;

    if peek(mem, si.wrapping_add(1)) == b':' {
        let letter = peek(mem, si).to_ascii_uppercase();
        if letter.is_ascii_uppercase() && letter - b'A' < 26 {
            // Only C: exists.
            bad_drive = letter != b'C';
            mem.w8(es, di, letter - b'A' + 1);
        } else {
            bad_drive = true;
        }
        si = si.wrapping_add(2);
    } else if flags & 2 == 0 {
        mem.w8(es, di, 0);
    }

    // `width` bytes of name or extension, padded with spaces; `*` fills the rest with `?`.
    let mut field = |mem: &mut Mem, si: &mut u16, at: u16, width: u16, given: &mut bool| {
        let mut buf = [b' '; 8];
        let mut n = 0;
        loop {
            let b = peek(mem, *si);
            if is_terminator(b) {
                break;
            }
            *si = si.wrapping_add(1);
            *given = true;
            if b == b'*' {
                buf[n..width as usize].fill(b'?');
                n = width as usize;
                wild = true;
            } else if n < width as usize {
                wild |= b == b'?';
                buf[n] = b.to_ascii_uppercase();
                n += 1;
            }
        }
        // Optional fields keep their old contents when absent.
        if *given || flags & (if width == 8 { 4 } else { 8 }) == 0 {
            mem.write(es, di.wrapping_add(at), &buf[..width as usize]);
        }
    };
    let mut name_given = false;
    field(mem, &mut si, 1, 8, &mut name_given);
    let mut ext_given = false;
    if peek(mem, si) == b'.' {
        si = si.wrapping_add(1);
        field(mem, &mut si, 9, 3, &mut ext_given);
    } else if flags & 8 == 0 {
        mem.write(es, di.wrapping_add(9), b"   ");
    }

    r.si = si;
    r.set_al(if bad_drive { 0xFF } else { wild as u8 });
}

const CUR_RECORD: u16 = 0x20;
const RANDOM: u16 = 0x21;

pub(crate) enum Access {
    /// 14h, 15h: at the current block and record, which advance.
    Sequential,
    /// 21h, 22h: at the random record, which the current block and record then follow.
    Random,
    /// 27h, 28h: `CX` records from the random record, which advances.
    RandomBlock,
}

/// The random record field. With records of 64 bytes or more it is three
/// bytes, not four: the fourth belongs to whatever follows a 33-byte FCB.
fn random_record(mem: &Mem, seg: u16, off: u16, size: u32) -> u32 {
    let low = mem.r16(seg, off + RANDOM) as u32 | (mem.r8(seg, off + RANDOM + 2) as u32) << 16;
    if size >= 64 {
        low
    } else {
        low | (mem.r8(seg, off + RANDOM + 3) as u32) << 24
    }
}

fn set_random_field(mem: &mut Mem, seg: u16, off: u16, size: u32, record: u32) {
    mem.w16(seg, off + RANDOM, record as u16);
    mem.w8(seg, off + RANDOM + 2, (record >> 16) as u8);
    if size < 64 {
        mem.w8(seg, off + RANDOM + 3, (record >> 24) as u8);
    }
}

fn set_cursor(mem: &mut Mem, seg: u16, off: u16, record: u32) {
    mem.w16(seg, off + BLOCK, (record / 128) as u16);
    mem.w8(seg, off + CUR_RECORD, (record % 128) as u8);
}

/// Read or write records between the file an FCB names and the DTA.
/// `AL` reports: 0 done; 1 end of file (reads) or full disk (writes); 2 the
/// transfer would not fit in the DTA's segment; 3 a read stopped inside a
/// final partial record, which is padded with zeros.
pub(crate) fn transfer(r: &mut Regs, mem: &mut Mem, volume: &mut Volume, dta: (u16, u16), access: Access, write: bool) {
    let (seg, off) = (r.ds, r.dx);
    let size = mem.r16(seg, off + RECORD_SIZE) as u32;
    let first = match access {
        Access::Sequential => mem.r16(seg, off + BLOCK) as u32 * 128 + mem.r8(seg, off + CUR_RECORD) as u32,
        _ => random_record(mem, seg, off, size),
    };
    let count = if matches!(access, Access::RandomBlock) { r.cx as u32 } else { 1 };
    let Some(slot) = name_of(mem, seg, off).and_then(|name| volume.slot(&name)) else { return r.set_al(1) };
    if size == 0 || size * count > 0x1_0000 || dta.1 as u32 + size * count > 0x1_0000 {
        return r.set_al(2);
    }
    let start = (first as u64 * size as u64) as usize;
    let mut done = 0;
    let mut status = 0;
    if write {
        let end = start + (size * count) as usize;
        let have = volume.data(slot).len();
        if end > have && end - have > volume.free() {
            r.cx = 0;
            return r.set_al(1);
        }
        let file = volume.data_mut(slot).expect("slot just looked up");
        if file.len() < end {
            file.resize(end, 0);
        }
        mem.read(dta.0, dta.1, &mut file[start..end]);
        done = count;
        let new = file.len() as u32;
        mem.w16(seg, off + FILE_SIZE, new as u16);
        mem.w16(seg, off + FILE_SIZE + 2, (new >> 16) as u16);
    } else {
        let file = volume.data(slot);
        for i in 0..count as usize {
            let at = start + i * size as usize;
            if at >= file.len() {
                status = if done == 0 { 1 } else { 0 };
                break;
            }
            let have = (file.len() - at).min(size as usize);
            let to = dta.1.wrapping_add((i as u32 * size) as u16);
            mem.write(dta.0, to, &file[at..at + have]);
            mem.write(dta.0, to.wrapping_add(have as u16), &alloc::vec![0; size as usize - have]);
            done += 1;
            if have < size as usize {
                status = 3;
                break;
            }
        }
    }
    let next = first + done;
    match access {
        Access::Sequential => set_cursor(mem, seg, off, next),
        Access::Random => set_cursor(mem, seg, off, first),
        Access::RandomBlock => {
            set_cursor(mem, seg, off, first);
            set_random_field(mem, seg, off, size, next);
            r.cx = done as u16;
        }
    }
    r.set_al(status);
}

/// Set the random record field to match the current block and record (24h).
pub(crate) fn set_random_record(r: &mut Regs, mem: &mut Mem) {
    let (seg, off) = (r.ds, r.dx);
    let record = mem.r16(seg, off + BLOCK) as u32 * 128 + mem.r8(seg, off + CUR_RECORD) as u32;
    set_random_field(mem, seg, off, mem.r16(seg, off + RECORD_SIZE) as u32, record);
}

/// Find first (11h, `first`) or next (12h) matching file for the FCB at
/// `DS:DX`, which may be an extended FCB (first byte FFh). The match goes to
/// the DTA as a directory entry preceded by a drive number (and, for an
/// extended FCB, its header). The next slot to examine lives in the FCB's
/// reserved bytes.
pub(crate) fn find(r: &mut Regs, mem: &mut Mem, volume: &Volume, dta: (u16, u16), first: bool) {
    let extended = mem.r8(r.ds, r.dx) == 0xFF;
    let (seg, off) = (r.ds, r.dx + if extended { 7 } else { 0 });
    let drive = mem.r8(seg, off);
    if drive != 0 && drive != 3 {
        return r.set_al(0xFF);
    }
    let mut pattern = [0; 11];
    mem.read(seg, off + NAME, &mut pattern);
    if first {
        mem.w16(seg, off + 0x18, 0);
    }
    let mut from = mem.r16(seg, off + 0x18) as usize;
    while let Some((slot, name, size)) = volume.next_from(from) {
        from = slot + 1;
        if !files::matches(&pattern, name) {
            continue;
        }
        mem.w16(seg, off + 0x18, from as u16);
        let (time, date) = volume.stamp(slot);
        let mut at = dta.1;
        if extended {
            mem.write(dta.0, at, &[0xFF, 0, 0, 0, 0, 0, 0x20]);
            at += 7;
        }
        mem.w8(dta.0, at, 3); // drive C:
        mem.write(dta.0, at + 1, &files::fcb_name(name));
        mem.w8(dta.0, at + 12, 0x20); // archive: an ordinary file
        mem.write(dta.0, at + 13, &[0; 10]);
        mem.w16(dta.0, at + 23, time);
        mem.w16(dta.0, at + 25, date);
        mem.w16(dta.0, at + 27, 0);
        mem.w16(dta.0, at + 29, size as u16);
        mem.w16(dta.0, at + 31, (size >> 16) as u16);
        return r.set_al(0);
    }
    mem.w16(seg, off + 0x18, from as u16);
    r.set_al(0xFF);
}

/// "NAME.EXT" from the 11 padded bytes of an FCB name.
fn dotted(raw: &[u8; 11]) -> String {
    let text = |b: &[u8]| String::from_utf8_lossy(b).trim_end().to_string();
    let (stem, ext) = (text(&raw[..8]), text(&raw[8..]));
    if ext.is_empty() {
        stem
    } else {
        format!("{stem}.{ext}")
    }
}

/// Delete (13h) every file matching the FCB's name, wildcards included.
pub(crate) fn delete(r: &mut Regs, mem: &Mem, volume: &mut Volume) {
    let mut pattern = [0; 11];
    mem.read(r.ds, r.dx + NAME, &mut pattern);
    let doomed: Vec<usize> = (0..)
        .scan(0, |from, _| {
            let (slot, name, _) = volume.next_from(*from)?;
            *from = slot + 1;
            Some((slot, files::matches(&pattern, name)))
        })
        .filter_map(|(slot, hit)| hit.then_some(slot))
        .collect();
    doomed.iter().for_each(|&slot| volume.remove(slot));
    r.set_al(if doomed.is_empty() { 0xFF } else { 0 });
}

/// Rename (17h): the old name is at offset 1 of the FCB and the new one at
/// 11h, either with wildcards; a `?` in the new name keeps the old character.
/// A name that already exists is never replaced: nothing is renamed then.
pub(crate) fn rename(r: &mut Regs, mem: &Mem, volume: &mut Volume) {
    let (mut old, mut new) = ([0; 11], [0; 11]);
    mem.read(r.ds, r.dx + NAME, &mut old);
    mem.read(r.ds, r.dx + 0x11, &mut new);
    let mut plan = Vec::new();
    let mut from = 0;
    while let Some((slot, name, _)) = volume.next_from(from) {
        from = slot + 1;
        if files::matches(&old, name) {
            let have = files::fcb_name(name);
            let mut target = new;
            for (t, h) in target.iter_mut().zip(have) {
                if *t == b'?' {
                    *t = h;
                }
            }
            plan.push((slot, dotted(&target)));
        }
    }
    let clash = plan.iter().any(|(slot, to)| volume.slot(to).is_some_and(|other| other != *slot));
    if plan.is_empty() || clash {
        return r.set_al(0xFF);
    }
    plan.iter().for_each(|(slot, to)| volume.rename(*slot, to));
    r.set_al(0);
}

/// Close (10h). Writes reach the volume at once, so there is nothing to
/// flush; closing only confirms the file the FCB names is still there.
pub(crate) fn close(r: &mut Regs, mem: &Mem, volume: &Volume) {
    let found = name_of(mem, r.ds, r.dx).is_some_and(|name| volume.slot(&name).is_some());
    r.set_al(if found { 0 } else { 0xFF });
}
