//! Processor odds and ends: MSRs, CPUID, port I/O.

use core::arch::asm;
use core::arch::x86_64::__cpuid;

pub fn cpuid(leaf: u32) -> (u32, u32, u32, u32) {
    // SAFETY: CPUID exists on every x86-64 processor.
    let r = unsafe { __cpuid(leaf) };
    (r.eax, r.ebx, r.ecx, r.edx)
}

/// # Safety
/// The MSR must exist.
pub unsafe fn rdmsr(msr: u32) -> u64 {
    let (lo, hi): (u32, u32);
    asm!("rdmsr", in("ecx") msr, out("eax") lo, out("edx") hi, options(nomem, nostack, preserves_flags));
    (hi as u64) << 32 | lo as u64
}

/// # Safety
/// The MSR must exist and the value must be one the supervisor can live with.
pub unsafe fn wrmsr(msr: u32, value: u64) {
    asm!("wrmsr", in("ecx") msr, in("eax") value as u32, in("edx") (value >> 32) as u32,
         options(nomem, nostack, preserves_flags));
}

pub fn outb(port: u16, value: u8) {
    // SAFETY: the supervisor owns every port; callers name ones they drive.
    unsafe { asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)) }
}

pub fn inb(port: u16) -> u8 {
    let value: u8;
    // SAFETY: as above.
    unsafe { asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)) }
    value
}

pub fn outl(port: u16, value: u32) {
    // SAFETY: as for `outb`.
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags)) }
}

pub fn inl(port: u16) -> u32 {
    let value: u32;
    // SAFETY: as for `outb`.
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags)) }
    value
}

pub fn outw(port: u16, value: u16) {
    // SAFETY: as for `outb`.
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)) }
}

pub fn inw(port: u16) -> u16 {
    let value: u16;
    // SAFETY: as for `outb`.
    unsafe { asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags)) }
    value
}
