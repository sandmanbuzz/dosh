//! File control blocks: INT 21h function 29h, parse filename.

use crate::mem::Mem;
use crate::Regs;

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
