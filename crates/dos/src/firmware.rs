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
/// Segment of the BIOS data area.
pub const BDA: u16 = 0x0040;

pub fn install(mem: &mut Mem) {
    for vector in 0..256u16 {
        mem.w16(0, vector * 4, vector * 2);
        mem.w16(0, vector * 4 + 2, GATE_SEG);
        mem.w8(GATE_SEG, vector * 2, HLT);
        mem.w8(GATE_SEG, vector * 2 + 1, IRET);
    }
    mem.w16(BDA, 0x6C, 0); // tick count, set by the supervisor's clock
    mem.w16(BDA, 0x6E, 0);
    mem.w8(BDA, 0x70, 0); // midnight flag
    mem.w16(BDA, 0x10, EQUIPMENT);
    mem.w16(BDA, 0x13, CONVENTIONAL_KB);
    mem.w8(BDA, 0x49, 3); // video mode: 80x25 colour text
    mem.w16(BDA, 0x4A, 80); // columns
    mem.w8(BDA, 0x84, 24); // rows - 1
}

/// 80x25 colour display, no other hardware.
pub const EQUIPMENT: u16 = 0x0020;
pub const CONVENTIONAL_KB: u16 = (CONVENTIONAL_TOP as u32 * 16 / 1024) as u16;

/// BIOS timer ticks in one day: the count wraps to zero after this, and the
/// midnight flag is set.
const TICKS_PER_DAY: u64 = 0x18_00B0;

/// Bring the BIOS clock (0040:006Ch, a dword, and the midnight flag at
/// 0040:0070h) to `hz100`, the capsule's clock in hundredths of a second.
/// The BIOS counts the 1.193182 MHz PIT divided by 65536, about 18.2 a
/// second.
pub fn ticks_at(hz100: u64) -> u32 {
    let raw = hz100 * 1_193_182 / (65_536 * 100);
    (raw % TICKS_PER_DAY) as u32
}

/// Store the tick count where programs read it.
pub fn write_ticks(mem: &mut Mem, count: u32) {
    mem.w16(BDA, 0x6C, count as u16);
    mem.w16(BDA, 0x6E, (count >> 16) as u16);
}

/// Set the midnight flag, which `INT 1Ah AH=00h` reports and clears.
pub fn set_midnight(mem: &mut Mem) {
    mem.w8(BDA, 0x70, 1);
}

/// If `cs:ip` is the `HLT` of a service gate, the vector it serves.
pub fn gate_vector(cs: u16, ip: u16) -> Option<u8> {
    (cs == GATE_SEG && ip < 512 && ip.is_multiple_of(2)).then_some((ip / 2) as u8)
}
