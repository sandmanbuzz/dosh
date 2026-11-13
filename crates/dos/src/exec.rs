//! Program loading: the environment block, the PSP, and COM and MZ images.

use crate::arena;
use crate::firmware::CONVENTIONAL_TOP;
use crate::mem::Mem;
use crate::Regs;
use alloc::vec::Vec;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LoadError {
    NotFound,
    /// The image is truncated or its header is inconsistent.
    BadImage,
    NoMemory,
    /// The command tail does not fit in the PSP.
    TailTooLong,
}

pub const PSP_PARAS: u16 = 0x10;
const TAIL_MAX: usize = 126;
/// A COM image shares one segment with its PSP and stack.
const COM_MAX: usize = 0xFF00 - 0x100;

struct MzHeader {
    image_len: usize,
    header_len: usize,
    relocs: usize,
    reloc_table: usize,
    min_extra: u16,
    max_extra: u16,
    ss: u16,
    sp: u16,
    ip: u16,
    cs: u16,
}

fn mz_header(file: &[u8]) -> Option<Result<MzHeader, LoadError>> {
    if file.len() < 2 || !matches!(&file[..2], b"MZ" | b"ZM") {
        return None;
    }
    Some((|| {
        if file.len() < 0x1C {
            return Err(LoadError::BadImage);
        }
        let w = |at: usize| u16::from_le_bytes([file[at], file[at + 1]]);
        let (last_page, pages) = (w(2) as usize, w(4) as usize);
        let header_len = w(8) as usize * 16;
        let mut file_len = pages * 512;
        if last_page != 0 {
            file_len = file_len.checked_sub(512 - last_page).ok_or(LoadError::BadImage)?;
        }
        // Anything past the load module (overlays, debug info) stays on disk.
        let file_len = file_len.min(file.len());
        let image_len = file_len.checked_sub(header_len).ok_or(LoadError::BadImage)?;
        let hdr = MzHeader {
            image_len,
            header_len,
            relocs: w(6) as usize,
            reloc_table: w(0x18) as usize,
            min_extra: w(0x0A),
            max_extra: w(0x0C),
            ss: w(0x0E),
            sp: w(0x10),
            ip: w(0x14),
            cs: w(0x16),
        };
        if hdr.reloc_table + hdr.relocs * 4 > file.len() {
            return Err(LoadError::BadImage);
        }
        Ok(hdr)
    })())
}

/// The variables every root process starts with.
pub const DEFAULT_ENV: &[u8] = b"PATH=C:\\\0COMSPEC=C:\\COMMAND.COM\0\0";

/// The variables of the environment block at `seg`, up to and including the
/// empty string that ends them.
pub fn env_vars(mem: &Mem, seg: u16) -> Vec<u8> {
    let mut vars = Vec::new();
    for off in 0..=u16::MAX {
        let b = mem.r8(seg, off);
        vars.push(b);
        if b == 0 && (vars.len() == 1 || vars[vars.len() - 2] == 0) {
            break;
        }
    }
    vars
}

fn env_block(mem: &mut Mem, program: &str, vars: &[u8]) -> Result<u16, LoadError> {
    // Variables, then a count of trailing strings (1), then the full path of
    // the program, as DOS 3+ provides.
    let len = vars.len() + 2 + 3 + program.len() + 1;
    let seg = arena::alloc(mem, len.div_ceil(16) as u16, 0xFFFF).map_err(|_| LoadError::NoMemory)?;
    let mut off = 0;
    for part in [vars, &[1, 0], b"C:\\", program.as_bytes(), &[0]] {
        mem.write(seg, off, part);
        off += part.len() as u16;
    }
    Ok(seg)
}

fn build_psp(mem: &mut Mem, psp: u16, parent: u16, env: u16, tail: &[u8]) {
    mem.write(psp, 0, &[0; 256]);
    mem.write(psp, 0x00, &[0xCD, 0x20]); // INT 20h: `ret` from a COM program ends it
    mem.w16(psp, 0x02, psp + arena::size(mem, psp)); // first segment past the program
    mem.write(psp, 0x05, &[0x9A, 0xF0, 0xFE, 0x1D, 0xF0]); // CP/M-style far call to DOS
    for (slot, vector) in [(0x0A, 0x22u16), (0x0E, 0x23), (0x12, 0x24)] {
        mem.w16(psp, slot, mem.r16(0, vector * 4));
        mem.w16(psp, slot + 2, mem.r16(0, vector * 4 + 2));
    }
    mem.w16(psp, 0x16, parent);
    mem.write(psp, 0x18, &[0xFF; 20]); // job file table
    mem.write(psp, 0x18, &[1, 1, 1, 0, 2]);
    mem.w16(psp, 0x2C, env);
    mem.w16(psp, 0x32, 20);
    mem.w16(psp, 0x34, 0x18);
    mem.w16(psp, 0x36, psp);
    mem.write(psp, 0x38, &[0xFF; 4]);
    mem.write(psp, 0x40, &[crate::DOS_VERSION.0, crate::DOS_VERSION.1]);
    mem.write(psp, 0x50, &[0xCD, 0x21, 0xCB]); // INT 21h / RETF
    for fcb in [0x5C, 0x6C] {
        mem.write(psp, fcb + 1, b"           ");
    }
    mem.w8(psp, 0x80, tail.len() as u8);
    mem.write(psp, 0x81, tail);
    mem.w8(psp, 0x81 + tail.len() as u16, b'\r');
}

/// Load `file` as a new process and return its initial register state.
/// `program` is its name on drive C:; `tail` is the raw command tail,
/// including its customary leading space. `parent` is the PSP of the process
/// starting it (the root process is its own parent) and `vars` its
/// environment variables, in the form [`env_vars`] returns.
pub fn load(
    mem: &mut Mem,
    file: &[u8],
    program: &str,
    tail: &[u8],
    parent: Option<u16>,
    vars: &[u8],
) -> Result<(u16, Regs), LoadError> {
    if tail.len() > TAIL_MAX {
        return Err(LoadError::TailTooLong);
    }
    let mz = mz_header(file).transpose()?;
    if mz.is_none() && file.len() > COM_MAX {
        return Err(LoadError::BadImage);
    }
    let env = env_block(mem, program, vars)?;

    // A program is given the largest free block; an MZ header may cap it.
    let largest = match arena::alloc(mem, 0xFFFF, 0) {
        Err((arena::ERR_NO_MEMORY, largest)) => largest,
        _ => return Err(LoadError::NoMemory),
    };
    let (needed, wanted) = match &mz {
        Some(h) => {
            let image = PSP_PARAS as usize + h.image_len.div_ceil(16);
            (image + h.min_extra as usize, image + h.max_extra as usize)
        }
        // Image, plus room for the initial stack.
        None => (PSP_PARAS as usize + file.len().div_ceil(16) + 0x10, usize::MAX),
    };
    if needed > largest as usize {
        return Err(LoadError::NoMemory);
    }
    let paras = wanted.max(needed).min(largest as usize) as u16;
    let psp = arena::alloc(mem, paras, 0).map_err(|_| LoadError::NoMemory)?;
    arena::set_owner(mem, psp, psp);
    arena::set_owner(mem, env, psp);
    debug_assert!(psp + paras <= CONVENTIONAL_TOP);
    build_psp(mem, psp, parent.unwrap_or(psp), env, tail);

    let mut regs = Regs { ds: psp, es: psp, flags: 0x0202, ..Regs::default() };
    match mz {
        None => {
            mem.write(psp, 0x100, file);
            // Stack at the top of the segment, or of the block if smaller,
            // holding a zero so that a near `ret` lands on the PSP's INT 20h.
            let sp = if paras >= 0x1000 { 0xFFFE } else { (paras << 4) - 2 };
            mem.w16(psp, sp, 0);
            regs.cs = psp;
            regs.ss = psp;
            regs.ip = 0x100;
            regs.sp = sp;
        }
        Some(h) => {
            let base = psp + PSP_PARAS;
            let image = &file[h.header_len..h.header_len + h.image_len];
            mem.span_mut(base, image.len()).ok_or(LoadError::NoMemory)?.copy_from_slice(image);
            for i in 0..h.relocs {
                let at = h.reloc_table + i * 4;
                let off = u16::from_le_bytes([file[at], file[at + 1]]);
                let seg = u16::from_le_bytes([file[at + 2], file[at + 3]]);
                if ((seg as usize) << 4) + off as usize + 2 > h.image_len {
                    return Err(LoadError::BadImage);
                }
                let seg = base.wrapping_add(seg);
                mem.w16(seg, off, mem.r16(seg, off).wrapping_add(base));
            }
            regs.cs = base.wrapping_add(h.cs);
            regs.ss = base.wrapping_add(h.ss);
            regs.ip = h.ip;
            regs.sp = h.sp;
        }
    }
    Ok((psp, regs))
}
