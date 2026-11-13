//! INT 21h: the DOS function dispatcher.

use alloc::vec::Vec;

use crate::files::{self, Handle, Target, MODE_READ, MODE_WRITE};
use crate::mem::Mem;
use crate::{arena, exec, CharDevice, Dos, Fault, Outcome, Regs, DOS_VERSION, FLAG_CF, FLAG_ZF};

const ERR_BAD_FUNCTION: u16 = 1;
const ERR_NO_MEMORY: u16 = 8;
const ERR_BAD_ENVIRONMENT: u16 = 10;
const ERR_BAD_FORMAT: u16 = 11;
/// Characters a console handle read accepts per line, as in DOS.
const CON_LINE_MAX: usize = 126;

type DosResult = Result<(), u16>;

/// The ASCIIZ path at `seg:off`.
fn path_at(mem: &Mem, seg: u16, off: u16) -> Vec<u8> {
    (0..128).map(|i| mem.r8(seg, off.wrapping_add(i))).take_while(|&b| b != 0).collect()
}

impl Dos {
    /// The next character for a DOS input call. With a screen, input is
    /// keystrokes, and an extended key reads as 0 then its scan code.
    fn next_key(&mut self, con: &mut dyn CharDevice) -> Option<u8> {
        if let Some(key) = self.key.take() {
            return Some(key);
        }
        if !self.screen {
            return con.read();
        }
        if let Some(scan) = self.key_scan.take() {
            return Some(scan);
        }
        let key = self.kbd.pop(con)?;
        if key as u8 == 0 {
            self.key_scan = Some((key >> 8) as u8);
        }
        Some(key as u8)
    }

    fn key_ready(&mut self, con: &mut dyn CharDevice) -> bool {
        if self.key.is_none() {
            self.key = self.next_key(con);
        }
        self.key.is_some()
    }

    /// `INT 16h`, the keyboard BIOS.
    pub(crate) fn int16(&mut self, r: &mut Regs, con: &mut dyn CharDevice) -> Outcome {
        match r.ah() {
            0x00 | 0x10 => match self.kbd.pop(con) {
                Some(key) => r.ax = key,
                None => return Outcome::WaitInput,
            },
            0x01 | 0x11 => {
                let key = self.kbd.peek(con);
                r.set_flag(FLAG_ZF, key.is_none());
                if let Some(key) = key {
                    r.ax = key;
                }
            }
            // Shift state: a terminal reports none.
            0x02 | 0x12 => r.ax = 0,
            ah => return Outcome::Fault(Fault::Unimplemented { vector: 0x16, ah, al: r.al() }),
        }
        Outcome::Resume
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
        let entry = self.handles.get(bx as usize).copied().flatten().ok_or(files::ERR_BAD_HANDLE)?;
        self.open[entry].as_mut().map(|(handle, _)| handle).ok_or(files::ERR_BAD_HANDLE)
    }

    /// Point handle `at` (or the lowest free one) at system file table entry `entry`.
    fn attach(&mut self, entry: usize, at: Option<usize>) -> Result<usize, u16> {
        let at = match at {
            Some(at) => at,
            None => self.handles.iter().position(Option::is_none).ok_or(files::ERR_TOO_MANY_OPEN)?,
        };
        self.handles[at] = Some(entry);
        if let Some((_, refs)) = self.open[entry].as_mut() {
            *refs += 1;
        }
        Ok(at)
    }

    fn detach(&mut self, bx: u16) -> DosResult {
        let entry = self.handles.get_mut(bx as usize).and_then(Option::take).ok_or(files::ERR_BAD_HANDLE)?;
        if let Some((_, refs)) = self.open[entry].as_mut() {
            *refs -= 1;
            if *refs == 0 {
                self.open[entry] = None;
            }
        }
        Ok(())
    }

    /// EXEC (AH=4Bh AL=0): load a program as a child of the caller and start
    /// it. The caller resumes, with AX=0 and carry clear, when the child ends.
    fn spawn(&mut self, r: &mut Regs, mem: &mut Mem) -> DosResult {
        let Target::File(name) = files::resolve(&path_at(mem, r.ds, r.dx))? else {
            return Err(files::ERR_ACCESS_DENIED);
        };
        let file = self.volume.get(&name).ok_or(files::ERR_FILE_NOT_FOUND)?;
        // The parameter block: environment, command tail, two FCBs.
        let (pb_seg, pb) = (r.es, r.bx);
        let far = |mem: &Mem, at: u16| (mem.r16(pb_seg, pb.wrapping_add(at + 2)), mem.r16(pb_seg, pb.wrapping_add(at)));
        let env_seg = match mem.r16(pb_seg, pb) {
            0 => mem.r16(self.psp, 0x2C),
            seg => seg,
        };
        let vars = exec::env_vars(mem, env_seg);
        let (tail_seg, tail_off) = far(mem, 2);
        let fcbs = [far(mem, 6), far(mem, 10)];
        let mut tail = alloc::vec![0; mem.r8(tail_seg, tail_off) as usize];
        mem.read(tail_seg, tail_off.wrapping_add(1), &mut tail);
        let (psp, start) = exec::load(mem, file, &name, &tail, Some(self.psp), &vars).map_err(|e| match e {
            exec::LoadError::NotFound => files::ERR_FILE_NOT_FOUND,
            exec::LoadError::BadImage => ERR_BAD_FORMAT,
            exec::LoadError::NoMemory => ERR_NO_MEMORY,
            exec::LoadError::TailTooLong => ERR_BAD_ENVIRONMENT,
        })?;
        for (slot, (seg, off)) in [0x5C, 0x6C].into_iter().zip(fcbs) {
            let mut fcb = [0; 16];
            mem.read(seg, off, &mut fcb);
            mem.write(psp, slot, &fcb);
        }
        self.parents.push(crate::Parent { psp: self.psp, dta: self.dta, handles: self.handles, regs: *r });
        // The child shares the parent's open files.
        for entry in self.handles.iter().flatten() {
            if let Some((_, refs)) = self.open[*entry].as_mut() {
                *refs += 1;
            }
        }
        self.psp = psp;
        self.dta = (psp, 0x80);
        // Arrange for the gate's IRET to land on the child's entry point.
        let sp = start.sp.wrapping_sub(6);
        mem.w16(start.ss, sp, start.ip);
        mem.w16(start.ss, sp.wrapping_add(2), start.cs);
        mem.w16(start.ss, sp.wrapping_add(4), start.flags);
        *r = Regs { ss: start.ss, sp, ds: psp, es: psp, flags: start.flags, cs: r.cs, ip: r.ip, ..Regs::default() };
        Ok(())
    }

    /// The running process ended: give back what it held and resume its parent.
    pub(crate) fn return_to_parent(&mut self, code: u8, r: &mut Regs, mem: &mut Mem) {
        let Some(parent) = self.parents.pop() else { return };
        arena::free_owned(mem, self.psp);
        for handle in 0..self.handles.len() {
            let _ = self.detach(handle as u16);
        }
        self.handles = parent.handles;
        self.psp = parent.psp;
        self.dta = parent.dta;
        self.child_code = code as u16;
        *r = parent.regs;
        r.ax = 0;
        r.set_flag(FLAG_CF, false);
    }

    /// Duplicate handle `bx` (AH=45h) or make `cx` another name for it (AH=46h).
    fn duplicate(&mut self, r: &mut Regs, onto: bool) -> DosResult {
        let entry = self.handles.get(r.bx as usize).copied().flatten().ok_or(files::ERR_BAD_HANDLE)?;
        if !onto {
            r.ax = self.attach(entry, None)? as u16;
            return Ok(());
        }
        if r.cx as usize >= self.handles.len() {
            return Err(files::ERR_BAD_HANDLE);
        }
        if self.handles[r.cx as usize] != Some(entry) {
            let _ = self.detach(r.cx); // a handle already open there is closed first
            self.attach(entry, Some(r.cx as usize))?;
        }
        Ok(())
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
        let entry = match self.open.iter().position(Option::is_none) {
            Some(free) => free,
            None => {
                self.open.push(None);
                self.open.len() - 1
            }
        };
        self.open[entry] = Some((handle, 0));
        match self.attach(entry, None) {
            Ok(at) => r.ax = at as u16,
            Err(e) => {
                self.open[entry] = None;
                return Err(e);
            }
        }
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

    /// Directory search state lives in the caller's DTA, as in DOS: the
    /// pattern in bytes 1-11, the next slot to look at in bytes 12-13.
    fn find_first(&mut self, r: &mut Regs, mem: &mut Mem) -> DosResult {
        let path = path_at(mem, r.ds, r.dx);
        let name = match files::resolve(&path)? {
            Target::File(name) => name,
            _ => return Err(files::ERR_FILE_NOT_FOUND),
        };
        let (seg, off) = self.dta;
        mem.w8(seg, off, 3);
        mem.write(seg, off.wrapping_add(1), &files::fcb_name(&name));
        mem.w16(seg, off.wrapping_add(12), 0);
        self.find(mem).map_err(|e| if e == files::ERR_NO_MORE_FILES { files::ERR_FILE_NOT_FOUND } else { e })
    }

    /// IOCTL status (AL=6 input, 7 output): AL=FFh ready, 0 not. Output never
    /// blocks. A file is ready for input until its position reaches its end.
    fn io_status(&mut self, r: &mut Regs, con: &mut dyn CharDevice) -> DosResult {
        let output = r.al() == 7;
        let handle = *self.handle(r.bx)?;
        let ready = match handle {
            Handle::Console if !output => self.key_ready(con),
            Handle::File { slot, pos, .. } if !output => (pos as usize) < self.volume.data(slot).len(),
            _ => true,
        };
        r.set_al(if ready { 0xFF } else { 0 });
        Ok(())
    }

    fn rename(&mut self, r: &Regs, mem: &Mem) -> DosResult {
        let (Target::File(from), Target::File(to)) =
            (files::resolve(&path_at(mem, r.ds, r.dx))?, files::resolve(&path_at(mem, r.es, r.di))?)
        else {
            return Err(files::ERR_ACCESS_DENIED); // devices cannot be renamed, or be a name
        };
        if from.contains(['*', '?']) || to.contains(['*', '?']) {
            return Err(files::ERR_ACCESS_DENIED);
        }
        let slot = self.volume.slot(&from).ok_or(files::ERR_FILE_NOT_FOUND)?;
        match self.volume.slot(&to) {
            Some(existing) if existing == slot => Ok(()),
            Some(_) => Err(files::ERR_ACCESS_DENIED),
            None => {
                self.volume.rename(slot, &to);
                Ok(())
            }
        }
    }

    /// Get (AL=0) or set (AL=1) the time and date of an open file.
    fn file_stamp(&mut self, r: &mut Regs) -> DosResult {
        let Handle::File { slot, .. } = *self.handle(r.bx)? else {
            return Err(ERR_BAD_FUNCTION); // devices have no timestamp
        };
        match r.al() {
            0 => (r.cx, r.dx) = self.volume.stamp(slot),
            1 => self.volume.set_stamp(slot, (r.cx, r.dx)),
            _ => return Err(ERR_BAD_FUNCTION),
        }
        Ok(())
    }

    fn find_next(&mut self, _r: &mut Regs, mem: &mut Mem) -> DosResult {
        self.find(mem)
    }

    fn find(&mut self, mem: &mut Mem) -> DosResult {
        let (seg, off) = self.dta;
        let mut pattern = [0; 11];
        mem.read(seg, off.wrapping_add(1), &mut pattern);
        let mut from = mem.r16(seg, off.wrapping_add(12)) as usize;
        while let Some((slot, name, size)) = self.volume.next_from(from) {
            from = slot + 1;
            if !files::matches(&pattern, name) {
                continue;
            }
            mem.w16(seg, off.wrapping_add(12), from as u16);
            mem.w8(seg, off.wrapping_add(21), 0x20); // archive: every file is an ordinary one
            let (time, date) = self.volume.stamp(slot);
            mem.w16(seg, off.wrapping_add(22), time);
            mem.w16(seg, off.wrapping_add(24), date);
            mem.w16(seg, off.wrapping_add(26), size as u16);
            mem.w16(seg, off.wrapping_add(28), (size >> 16) as u16);
            let mut entry = [0u8; 13];
            entry[..name.len()].copy_from_slice(name.as_bytes());
            mem.write(seg, off.wrapping_add(30), &entry);
            return Ok(());
        }
        mem.w16(seg, off.wrapping_add(12), from as u16);
        Err(files::ERR_NO_MORE_FILES)
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
            0x0D => return Outcome::Resume, // disk reset: nothing is buffered, writes reach the volume at once
            0x0E => {
                // Select disk. C: is the only drive and always current, so
                // this changes nothing; the count is of drive letters in use.
                r.set_al(3);
                return Outcome::Resume;
            }
            0x0F | 0x16 => {
                crate::fcb::open(r, mem, &mut self.volume, r.ah() == 0x16);
                return Outcome::Resume;
            }
            0x10 => {
                crate::fcb::close(r, mem, &self.volume);
                return Outcome::Resume;
            }
            0x13 => {
                crate::fcb::delete(r, mem, &mut self.volume);
                return Outcome::Resume;
            }
            0x17 => {
                crate::fcb::rename(r, mem, &mut self.volume);
                return Outcome::Resume;
            }
            0x14 | 0x15 | 0x21 | 0x22 | 0x27 | 0x28 => {
                use crate::fcb::Access;
                let access = match r.ah() {
                    0x14 | 0x15 => Access::Sequential,
                    0x21 | 0x22 => Access::Random,
                    _ => Access::RandomBlock,
                };
                let write = matches!(r.ah(), 0x15 | 0x22 | 0x28);
                crate::fcb::transfer(r, mem, &mut self.volume, self.dta, access, write);
                return Outcome::Resume;
            }
            0x11 | 0x12 => {
                crate::fcb::find(r, mem, &self.volume, self.dta, r.ah() == 0x11);
                return Outcome::Resume;
            }
            0x24 => {
                crate::fcb::set_random_record(r, mem);
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
            0x29 => {
                crate::fcb::parse_filename(r, mem);
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
            0x3E => self.detach(r.bx),
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
            0x44 if matches!(r.al(), 6 | 7) => self.io_status(r, con),
            0x45 => self.duplicate(r, false),
            0x46 => self.duplicate(r, true),
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
            0x56 => self.rename(r, mem),
            0x57 => self.file_stamp(r),
            0x4B if r.al() == 0 => self.spawn(r, mem),
            0x4D => {
                r.ax = core::mem::take(&mut self.child_code);
                Ok(())
            }
            0x4E => self.find_first(r, mem),
            0x4F => self.find_next(r, mem),
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
