//! The capsule's stand-in for PC firmware: the interrupt vector table, the
//! BIOS data area, and the service gates.
//!
//! A real-mode program reaches DOS and the BIOS with `INT n`, and is free to
//! read, replace and chain the vectors. So the vectors are real: each one
//! initially points at a two-byte gate in the ROM segment,
//!
//! ```text
//! F000:(n*2)   HLT     ; leaves the guest; the capsule performs service n
//!              IRET    ; resumes the caller, natively
//! ```
//!
//! `HLT` is a one-byte instruction that every hardware-virtualization
//! extension can intercept, which keeps the gate independent of the processor
//! vendor. The capsule recognises a gate by address, never by instruction.

use crate::mem::Mem;

pub const GATE_SEG: u16 = 0xF000;
/// First segment past conventional memory (640 KiB).
pub const CONVENTIONAL_TOP: u16 = 0xA000;

const HLT: u8 = 0xF4;
const IRET: u8 = 0xCF;
const BDA: u16 = 0x0040;

pub fn install(mem: &mut Mem) {
    for vector in 0..256u16 {
        mem.w16(0, vector * 4, vector * 2);
        mem.w16(0, vector * 4 + 2, GATE_SEG);
        mem.w8(GATE_SEG, vector * 2, HLT);
        mem.w8(GATE_SEG, vector * 2 + 1, IRET);
    }
    mem.w16(BDA, 0x10, EQUIPMENT);
    mem.w16(BDA, 0x13, CONVENTIONAL_KB);
    mem.w8(BDA, 0x49, 3); // video mode: 80x25 colour text
    mem.w16(BDA, 0x4A, 80); // columns
    mem.w8(BDA, 0x84, 24); // rows - 1
}

/// 80x25 colour display, no other hardware.
pub const EQUIPMENT: u16 = 0x0020;
pub const CONVENTIONAL_KB: u16 = (CONVENTIONAL_TOP as u32 * 16 / 1024) as u16;

/// If `cs:ip` is the `HLT` of a service gate, the vector it serves.
pub fn gate_vector(cs: u16, ip: u16) -> Option<u8> {
    (cs == GATE_SEG && ip < 512 && ip.is_multiple_of(2)).then_some((ip / 2) as u8)
}
