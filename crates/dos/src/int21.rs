//! INT 21h: the DOS function dispatcher.

use alloc::vec::Vec;

use crate::files::{self, Handle, Target, MODE_READ, MODE_WRITE};
use crate::mem::Mem;
use crate::{arena, CharDevice, Dos, Fault, Outcome, Regs, DOS_VERSION, FLAG_CF, FLAG_ZF};

const ERR_BAD_FUNCTION: u16 = 1;
/// Characters a console handle read accepts per line, as in DOS.
const CON_LINE_MAX: usize = 126;

type DosResult = Result<(), u16>;

/// The ASCIIZ path at `seg:off`.
fn path_at(mem: &Mem, seg: u16, off: u16) -> Vec<u8> {
    (0..128).map(|i| mem.r8(seg, off.wrapping_add(i))).take_while(|&b| b != 0).collect()
}

impl Dos {
    fn next_key(&mut self, con: &mut dyn CharDevice) -> Option<u8> {
        self.key.take().or_else(|| con.read())
    }

    fn key_ready(&mut self, con: &mut dyn CharDevice) -> bool {
        if self.key.is_none() {
            self.key = con.read();
        }
        self.key.is_some()
    }

    /// Advance the console line editor with whatever input has arrived.
    /// Returns the line, without its terminator, once Enter is pressed.
    fn edit_line(&mut self, con: &mut dyn CharDevice, max: usize) -> Option<Vec<u8>> {
        while let Some(c) = self.next_key(con) {
            match c {
                b'\r' => {
                    con.write(b'\r');
                    return Some(core::mem::take(&mut self.line));
                }
                // A byte-stream endpoint may end lines with CR LF.
                b'\n' => {}
                0x08 | 0x7F => {
                    if self.line.pop().is_some() {
                        b"\x08 \x08".iter().for_each(|&b| con.write(b));
                    }
                }
                _ if self.line.len() < max => {
                    self.line.push(c);
                    con.write(c);
                }
                _ => con.write(0x07),
            }
        }
        None
    }

    fn handle(&mut self, bx: u16) -> Result<&mut Handle, u16> {
        self.handles.get_mut(bx as usize).and_then(Option::as_mut).ok_or(files::ERR_BAD_HANDLE)
    }

    fn open(&mut self, r: &mut Regs, mem: &Mem, create: bool) -> DosResult {
        let handle = match files::resolve(&path_at(mem, r.ds, r.dx))? {
            Target::Console => Handle::Console,
            Target::Null => Handle::Null,
            Target::File(name) => {
                let slot = match (self.volume.slot(&name), create) {
                    (_, true) => self.volume.put(&name, Vec::new()),
                    (Some(slot), false) => slot,
                    (None, false) => return Err(files::ERR_FILE_NOT_FOUND),
                };
                // Created files are opened read/write; AL carries no mode.
                let mode = if create { 2 } else { r.al() & 3 };
                Handle::File { slot, pos: 0, mode }
            }
        };
        let free = self.handles.iter().position(Option::is_none).ok_or(files::ERR_TOO_MANY_OPEN)?;
        self.handles[free] = Some(handle);
        r.ax = free as u16;
        Ok(())
    }

    /// Returns `None` when the read must wait for console input.
    fn read(&mut self, r: &mut Regs, mem: &mut Mem, con: &mut dyn CharDevice) -> Option<DosResult> {
        let count = r.cx as usize;
        let mut data = Vec::new();
        let handle = match self.handle(r.bx) {
            Ok(handle) => *handle,
            Err(e) => return Some(Err(e)),
        };
        match handle {
            Handle::Null => {}
            Handle::Console => {
                if self.cooked.is_empty() && count > 0 {
                    let line = self.edit_line(con, CON_LINE_MAX)?;
                    con.write(b'\n');
                    // A line starting with Ctrl-Z is end of file.
                    if line.first() != Some(&0x1A) {
                        self.cooked.extend(line);
                        self.cooked.extend(b"\r\n");
                    }
                }
                let n = count.min(self.cooked.len());
                data.extend(self.cooked.drain(..n));
            }
            Handle::File { mode: MODE_WRITE, .. } => return Some(Err(files::ERR_ACCESS_DENIED)),
            Handle::File { slot, pos, .. } => {
                let file = self.volume.data(slot);
                let start = (pos as usize).min(file.len());
                data.extend(&file[start..file.len().min(start + count)]);
                if let Ok(Handle::File { pos, .. }) = self.handle(r.bx) {
                    *pos += data.len() as u32;
                }
            }
        }
        mem.write(r.ds, r.dx, &data);
        r.ax = data.len() as u16;
        Some(Ok(()))
    }

    fn write(&mut self, r: &mut Regs, mem: &Mem, con: &mut dyn CharDevice) -> DosResult {
        let mut data = alloc::vec![0; r.cx as usize];
        mem.read(r.ds, r.dx, &mut data);
        match *self.handle(r.bx)? {
            Handle::Null => {}
            Handle::Console => data.iter().for_each(|&b| con.write(b)),
            Handle::File { mode: MODE_READ, .. } => return Err(files::ERR_ACCESS_DENIED),
            Handle::File { slot, pos, .. } => {
                let (start, end) = (pos as usize, pos as usize + data.len());
                let free = self.volume.free();
                if let Some(file) = self.volume.data_mut(slot) {
                    if end.saturating_sub(file.len()) > free {
                        // Disk full. DOS reports it as a short write.
                        r.ax = 0;
                        return Ok(());
                    }
                    if data.is_empty() {
                        file.resize(start, 0); // a zero-length write sets the file size
                    } else {
                        if file.len() < end {
                            file.resize(end, 0);
                        }
                        file[start..end].copy_from_slice(&data);
                    }
                }
                if let Ok(Handle::File { pos, .. }) = self.handle(r.bx) {
                    *pos += data.len() as u32;
                }
            }
        }
        r.ax = data.len() as u16;
        Ok(())
    }

    fn seek(&mut self, r: &mut Regs) -> DosResult {
        // Absolute positions are unsigned; relative moves are signed.
        let offset = (r.cx as u32) << 16 | r.dx as u32;
        let offset = if r.al() == 0 { offset as i64 } else { offset as i32 as i64 };
        let origin = r.al();
        let size = match *self.handle(r.bx)? {
            Handle::File { slot, .. } => self.volume.data(slot).len() as i64,
            _ => 0,
        };
        let new = match self.handle(r.bx)? {
            Handle::File { pos, .. } => {
                let base = match origin {
                    0 => 0,
                    1 => *pos as i64,
                    2 => size,
                    _ => return Err(ERR_BAD_FUNCTION),
                };
                *pos = (base + offset).clamp(0, u32::MAX as i64) as u32;
                *pos
            }
            _ => 0,
        };
        r.ax = new as u16;
        r.dx = (new >> 16) as u16;
        Ok(())
    }

    pub(crate) fn int21(&mut self, r: &mut Regs, mem: &mut Mem, con: &mut dyn CharDevice) -> Outcome {
        let result: DosResult = match r.ah() {
            0x00 => return Outcome::Exit(0),
            0x01 | 0x07 | 0x08 => {
                let Some(c) = self.next_key(con) else { return Outcome::WaitInput };
                if r.ah() == 0x01 {
                    con.write(c);
                }
                r.set_al(c);
                return Outcome::Resume;
            }
            0x02 => {
                con.write(r.dl());
                return Outcome::Resume;
            }
            0x06 if r.dl() == 0xFF => {
                let key = self.next_key(con);
                r.set_al(key.unwrap_or(0));
                r.set_flag(FLAG_ZF, key.is_none());
                return Outcome::Resume;
            }
            0x06 => {
                con.write(r.dl());
                return Outcome::Resume;
            }
            0x09 => {
                // A string with no terminator stops after one trip round the segment.
                for i in 0..=u16::MAX {
                    match mem.r8(r.ds, r.dx.wrapping_add(i)) {
                        b'$' => break,
                        b => con.write(b),
                    }
                }
                r.set_al(b'$');
                return Outcome::Resume;
            }
            0x0A => {
                let max = (mem.r8(r.ds, r.dx) as usize).saturating_sub(1);
                let Some(line) = self.edit_line(con, max) else { return Outcome::WaitInput };
                mem.w8(r.ds, r.dx.wrapping_add(1), line.len() as u8);
                mem.write(r.ds, r.dx.wrapping_add(2), &line);
                mem.w8(r.ds, r.dx.wrapping_add(2 + line.len() as u16), b'\r');
                return Outcome::Resume;
            }
            0x0B => {
                let ready = self.key_ready(con);
                r.set_al(if ready { 0xFF } else { 0 });
                return Outcome::Resume;
            }
            0x19 => {
                r.set_al(2); // C:
                return Outcome::Resume;
            }
            0x1A => {
                self.dta = (r.ds, r.dx);
                return Outcome::Resume;
            }
            0x25 => {
                let slot = r.al() as u16 * 4;
                mem.w16(0, slot, r.dx);
                mem.w16(0, slot + 2, r.ds);
                return Outcome::Resume;
            }
            0x2F => {
                (r.es, r.bx) = self.dta;
                return Outcome::Resume;
            }
            0x30 => {
                r.ax = u16::from_le_bytes([DOS_VERSION.0, DOS_VERSION.1]);
                r.bx = 0xFF00; // OEM: MS-DOS
                r.cx = 0;
                return Outcome::Resume;
            }
            0x35 => {
                let slot = r.al() as u16 * 4;
                r.bx = mem.r16(0, slot);
                r.es = mem.r16(0, slot + 2);
                return Outcome::Resume;
            }
            0x3C => self.open(r, mem, true),
            0x3D => self.open(r, mem, false),
            0x3E => match self.handles.get_mut(r.bx as usize) {
                Some(handle @ Some(_)) => {
                    *handle = None;
                    Ok(())
                }
                _ => Err(files::ERR_BAD_HANDLE),
            },
            0x3F => match self.read(r, mem, con) {
                Some(result) => result,
                None => return Outcome::WaitInput,
            },
            0x40 => self.write(r, mem, con),
            0x41 => match files::resolve(&path_at(mem, r.ds, r.dx)) {
                Ok(Target::File(name)) => match self.volume.slot(&name) {
                    Some(slot) => {
                        self.volume.remove(slot);
                        Ok(())
                    }
                    None => Err(files::ERR_FILE_NOT_FOUND),
                },
                Ok(_) => Err(files::ERR_ACCESS_DENIED),
                Err(e) => Err(e),
            },
            0x42 => self.seek(r),
            0x44 if r.al() == 0 => self
                .handle(r.bx)
                .map(|h| match h {
                    Handle::Console => 0x80D3, // character device: console in and out
                    Handle::Null => 0x8084,
                    Handle::File { .. } => 0x0002, // file on drive C:
                })
                .map(|info| r.dx = info),
            0x48 => match arena::alloc(mem, r.bx, self.psp) {
                Ok(seg) => {
                    r.ax = seg;
                    Ok(())
                }
                Err((e, largest)) => {
                    r.bx = largest;
                    Err(e)
                }
            },
            0x49 => arena::free(mem, r.es),
            0x4A => arena::resize(mem, r.es, r.bx).map_err(|(e, max)| {
                r.bx = max;
                e
            }),
            0x4C => return Outcome::Exit(r.al()),
            0x51 | 0x62 => {
                r.bx = self.psp;
                return Outcome::Resume;
            }
            0x59 => {
                // Extended error: the code is exact; class, suggested action
                // and locus are the usual ones for that kind of failure.
                let (class, action, locus) = match self.last_error {
                    0 => (0, 0, 0),
                    2 | 3 => (8, 3, 2),     // not found; ask again; disk
                    4 | 7 | 8 => (1, 4, 5), // out of resource; abort; memory
                    5 => (3, 3, 2),         // not authorised; ask again; disk
                    _ => (7, 4, 1),         // program error; abort; unknown
                };
                r.ax = self.last_error;
                r.bx = u16::from_be_bytes([class, action]);
                r.cx = (r.cx & 0x00FF) | (locus as u16) << 8;
                return Outcome::Resume;
            }
            ah => return Outcome::Fault(Fault::Unimplemented { vector: 0x21, ah, al: r.al() }),
        };
        if let Err(code) = result {
            r.ax = code;
            self.last_error = code;
        }
        r.set_flag(FLAG_CF, result.is_err());
        Outcome::Resume
    }
}
