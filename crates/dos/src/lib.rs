//! DOS compatibility services for a DOSH execution capsule.
//!
//! This crate is the DOS personality: it lays out a capsule's memory the way
//! a DOS program expects, loads programs into it, and answers the software
//! interrupts they raise. It knows nothing about processors, hypervisors or
//! transports. It is handed three things by the capsule that owns it:
//!
//! * [`Mem`], the capsule's first megabyte;
//! * [`Regs`], the 16-bit register state at a service gate;
//! * a [`CharDevice`], the capsule's console device.
//!
//! Everything else it keeps in the guest's own memory, in the documented DOS
//! layouts, or in [`Dos`].
//!
//! The contract with programs is deliberately explicit: a service this crate
//! does not implement is never faked. It stops the capsule with
//! [`Fault::Unimplemented`], naming the call, so compatibility grows from what
//! real programs demonstrably need.

#![cfg_attr(not(test), no_std)]

extern crate alloc;

mod arena;
mod exec;
mod files;
pub mod firmware;
mod int21;
mod mem;
#[cfg(test)]
mod tests;

use alloc::collections::VecDeque;
use alloc::vec::Vec;

pub use exec::LoadError;
pub use files::Volume;
pub use mem::{Mem, RAM_SIZE};

/// Reported by INT 21h/30h and recorded in the PSP: (major, minor).
pub const DOS_VERSION: (u8, u8) = (5, 0);

/// The 16-bit register state of a guest at a service gate.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct Regs {
    pub ax: u16,
    pub bx: u16,
    pub cx: u16,
    pub dx: u16,
    pub si: u16,
    pub di: u16,
    pub bp: u16,
    pub sp: u16,
    pub ip: u16,
    pub flags: u16,
    pub cs: u16,
    pub ds: u16,
    pub es: u16,
    pub ss: u16,
}

pub const FLAG_CF: u16 = 0x0001;
pub const FLAG_ZF: u16 = 0x0040;

impl Regs {
    pub fn al(&self) -> u8 {
        self.ax as u8
    }
    pub fn ah(&self) -> u8 {
        (self.ax >> 8) as u8
    }
    pub fn dl(&self) -> u8 {
        self.dx as u8
    }
    pub fn set_al(&mut self, v: u8) {
        self.ax = (self.ax & 0xFF00) | v as u16;
    }
    fn set_flag(&mut self, flag: u16, on: bool) {
        self.flags = if on { self.flags | flag } else { self.flags & !flag };
    }
}

/// A capsule's console as DOS sees it: a character device. Where the bytes
/// come from and go to is the capsule's business.
pub trait CharDevice {
    /// The next input byte, if one has arrived.
    fn read(&mut self) -> Option<u8>;
    fn write(&mut self, byte: u8);
}

/// What the capsule should do after a service call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// Return to the guest.
    Resume,
    /// The program terminated with this return code.
    Exit(u8),
    /// The call needs console input that has not arrived. Nothing has
    /// changed; make the same call again when the device has input.
    WaitInput,
    Fault(Fault),
}

/// Why a program cannot continue.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Fault {
    /// A service outside the current compatibility contract.
    Unimplemented { vector: u8, ah: u8, al: u8 },
    /// The processor raised an exception the program did not handle.
    Exception { vector: u8 },
}

/// The DOS state of one capsule.
pub struct Dos {
    volume: Volume,
    psp: u16,
    dta: (u16, u16),
    handles: [Option<files::Handle>; 20],
    /// A key read ahead by an input-status call.
    key: Option<u8>,
    /// The console line being edited.
    line: Vec<u8>,
    /// A finished console line not yet consumed by handle reads.
    cooked: VecDeque<u8>,
    /// The error code of the last failed call, for "get extended error".
    last_error: u16,
    /// The BIOS tick count the clock last set, kept outside guest memory so
    /// a guest that overwrites 0040:006Ch cannot fake a midnight.
    bios_ticks: u32,
}

impl Dos {
    /// Prepare `mem` as a freshly booted DOS machine with `volume` as C:.
    pub fn new(mem: &mut Mem, volume: Volume) -> Self {
        firmware::install(mem);
        arena::init(mem);
        let mut handles = [None; 20];
        // stdin, stdout, stderr, and the aux and printer devices nobody has.
        handles[..3].fill(Some(files::Handle::Console));
        handles[3..5].fill(Some(files::Handle::Null));
        Self {
            volume,
            psp: 0,
            dta: (0, 0x80),
            handles,
            key: None,
            line: Vec::new(),
            cooked: VecDeque::new(),
            last_error: 0,
            bios_ticks: 0,
        }
    }

    pub fn volume(&self) -> &Volume {
        &self.volume
    }

    /// Advance the BIOS clock to `hz100`, the capsule's monotonic clock in
    /// hundredths of a second. The supervisor calls this before each run of
    /// the guest, so the guest reads the time from memory and from `INT 1Ah`.
    pub fn set_clock(&mut self, mem: &mut Mem, hz100: u64) {
        let count = firmware::ticks_at(hz100);
        if count < self.bios_ticks {
            firmware::set_midnight(mem);
        }
        self.bios_ticks = count;
        firmware::write_ticks(mem, count);
    }

    /// Load `program` from C: as the capsule's root process. Returns the
    /// register state it starts with.
    pub fn exec(&mut self, mem: &mut Mem, program: &str, tail: &[u8]) -> Result<Regs, LoadError> {
        let file = self.volume.get(program).ok_or(LoadError::NotFound)?;
        let (psp, regs) = exec::load(mem, file, &program.to_ascii_uppercase(), tail)?;
        self.psp = psp;
        self.dta = (psp, 0x80);
        Ok(regs)
    }

    /// Perform the service for `vector`. `regs` is the guest state at the
    /// gate: the caller's return address and flags are on its stack, and the
    /// gate's `IRET` will pop them when the capsule resumes the guest.
    pub fn interrupt(&mut self, vector: u8, regs: &mut Regs, mem: &mut Mem, con: &mut dyn CharDevice) -> Outcome {
        // Services report through the caller's flags, which are in the
        // interrupt frame rather than the live register.
        let flags_at = regs.sp.wrapping_add(4);
        let live_flags = core::mem::replace(&mut regs.flags, mem.r16(regs.ss, flags_at));
        let outcome = match vector {
            0x10 if regs.ah() == 0x0E => {
                con.write(regs.al());
                Outcome::Resume
            }
            0x11 => {
                regs.ax = firmware::EQUIPMENT;
                Outcome::Resume
            }
            0x12 => {
                regs.ax = firmware::CONVENTIONAL_KB;
                Outcome::Resume
            }
            0x1A if regs.ah() == 0x00 => {
                // Read the tick count. The midnight flag is reported once.
                regs.cx = mem.r16(firmware::BDA, 0x6E);
                regs.dx = mem.r16(firmware::BDA, 0x6C);
                regs.set_al(mem.r8(firmware::BDA, 0x70));
                mem.w8(firmware::BDA, 0x70, 0);
                Outcome::Resume
            }
            // The clock tick and the user tick hook. The supervisor keeps the
            // BIOS clock itself, so the tick's own work is already done.
            0x08 | 0x1C => Outcome::Resume,
            0x20 => Outcome::Exit(0),
            0x21 => self.int21(regs, mem, con),
            0x29 => {
                con.write(regs.al());
                Outcome::Resume
            }
            // Nothing in a capsule raises hardware interrupts yet, so
            // anything arriving on these vectors is the processor.
            0x00..=0x07 => Outcome::Fault(Fault::Exception { vector }),
            _ => Outcome::Fault(Fault::Unimplemented { vector, ah: regs.ah(), al: regs.al() }),
        };
        mem.w16(regs.ss, flags_at, regs.flags);
        regs.flags = live_flags;
        outcome
    }
}
