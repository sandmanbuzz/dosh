//! The supervisor's clock: a periodic local-APIC timer interrupt.
//!
//! This is the only interrupt the supervisor takes, and it exists so that no
//! guest can keep the processor. While a guest runs, the tick forces it out
//! (see `hv`); the supervisor itself keeps interrupts off and takes pending
//! ticks only at the two points below, [`accept`] and [`idle`].

use core::arch::asm;
use core::sync::atomic::{AtomicU64, Ordering};

use super::cpu::{inb, outb, rdmsr, wrmsr};

pub const TICK_VECTOR: u64 = 0x20;
pub const SPURIOUS_VECTOR: u64 = 0xFF;
pub const TICK_HZ: u32 = 100;

static TICKS: AtomicU64 = AtomicU64::new(0);

// Local APIC registers, as xAPIC MMIO offsets.
const EOI: u32 = 0x0B0;
const SPURIOUS: u32 = 0x0F0;
const LVT_TIMER: u32 = 0x320;
const INITIAL_COUNT: u32 = 0x380;
const CURRENT_COUNT: u32 = 0x390;
const DIVIDE: u32 = 0x3E0;

const MSR_APIC_BASE: u32 = 0x1B;

/// The local APIC is reached through MSRs in x2APIC mode, MMIO otherwise.
fn apic_write(reg: u32, value: u32) {
    // SAFETY: the local APIC belongs to the supervisor; its MMIO page lies
    // inside the identity map.
    unsafe {
        let base = rdmsr(MSR_APIC_BASE);
        if base & (1 << 10) != 0 {
            wrmsr(0x800 + (reg >> 4), value as u64);
        } else {
            (((base & !0xFFF) + reg as u64) as *mut u32).write_volatile(value);
        }
    }
}

fn apic_read(reg: u32) -> u32 {
    // SAFETY: as above.
    unsafe {
        let base = rdmsr(MSR_APIC_BASE);
        if base & (1 << 10) != 0 {
            rdmsr(0x800 + (reg >> 4)) as u32
        } else {
            (((base & !0xFFF) + reg as u64) as *const u32).read_volatile()
        }
    }
}

/// Local-APIC timer counts in one tick, measured against the PIT, whose
/// frequency is the one fixed point of PC timekeeping.
fn calibrate() -> u32 {
    const PIT_HZ: u32 = 1_193_182;
    const FALLBACK: u32 = 1_000_000;
    let count = (PIT_HZ / TICK_HZ) as u16;
    // Channel 2, gated on, speaker off: counts down once and raises OUT2.
    outb(0x61, (inb(0x61) & !0x02) | 0x01);
    outb(0x43, 0xB0);
    outb(0x42, count as u8);
    outb(0x42, (count >> 8) as u8);
    apic_write(INITIAL_COUNT, u32::MAX);
    for _ in 0..50_000_000u32 {
        if inb(0x61) & 0x20 != 0 {
            return (u32::MAX - apic_read(CURRENT_COUNT)).max(1000);
        }
    }
    FALLBACK // no PIT answered; any period still preempts
}

/// Silence every interrupt source the firmware left behind, then start the
/// tick. Interrupts stay disabled in the supervisor; see [`accept`].
pub fn start() {
    // SAFETY: interrupts are ours to manage from here on.
    unsafe { asm!("cli", options(nomem, nostack)) };
    // Both legacy interrupt controllers: mask every line.
    outb(0x21, 0xFF);
    outb(0xA1, 0xFF);
    // SAFETY: the APIC base MSR exists; setting bit 11 enables the APIC.
    unsafe { wrmsr(MSR_APIC_BASE, rdmsr(MSR_APIC_BASE) | 1 << 11) };
    apic_write(SPURIOUS, 0x100 | SPURIOUS_VECTOR as u32);
    apic_write(DIVIDE, 0x3); // divide by 16
    apic_write(LVT_TIMER, 1 << 16); // masked while calibrating
    let period = calibrate();
    apic_write(LVT_TIMER, TICK_VECTOR as u32 | 1 << 17); // periodic
    apic_write(INITIAL_COUNT, period);
}

pub fn ticks() -> u64 {
    TICKS.load(Ordering::Relaxed)
}

/// Called by the trap handler for [`TICK_VECTOR`].
pub fn on_tick() {
    TICKS.fetch_add(1, Ordering::Relaxed);
    apic_write(EOI, 0);
}

/// Take any pending tick now. The handler is ordinary compiled code, so the
/// window is declared as a call: nothing may be kept in registers across it.
pub fn accept() {
    // SAFETY: the trap path preserves everything a call would.
    unsafe { asm!("sti", "nop", "cli", clobber_abi("sysv64")) }
}

/// Sleep until the next tick.
pub fn idle() {
    // SAFETY: as for `accept`. STI takes effect after the following HLT
    // begins, so a tick cannot slip in between and be slept through.
    unsafe { asm!("sti", "hlt", "cli", clobber_abi("sysv64")) }
}
