//! The supervisor's descriptor tables, and what happens when the processor
//! raises something through them.
//!
//! The supervisor expects its clock tick, and no exceptions. A non-maskable
//! interrupt is the one other thing it survives: it is counted and the
//! interrupted code resumes. Anything else is reported and the machine
//! powered off.

use alloc::boxed::Box;
use core::arch::{asm, global_asm};
use core::sync::atomic::{AtomicU64, Ordering};

use super::{timer, Pages};

const CODE: u16 = 0x08;
const DATA: u16 = 0x10;
const TSS: u16 = 0x18;
const DOUBLE_FAULT: usize = 8;
const NMI: u64 = 2;

static NMIS: AtomicU64 = AtomicU64::new(0);

/// How many non-maskable interrupts the supervisor has taken.
pub fn nmis() -> u64 {
    NMIS.load(Ordering::Relaxed)
}

#[repr(C, packed)]
struct TablePointer {
    limit: u16,
    base: u64,
}

// 256 entry stubs, 16 bytes apart. Each normalises the stack (a zero where
// the processor pushed no error code), pushes its vector, and joins the
// common path.
global_asm!(
    r#"
    .section .text
    .balign 16
    .global dosh_trap_stubs
dosh_trap_stubs:
    .set vec, 0
    .rept 256
    .balign 16
    .if (vec == 8) + (vec == 10) + (vec == 11) + (vec == 12) + (vec == 13) + (vec == 14) + (vec == 17) + (vec == 21) + (vec == 29) + (vec == 30)
    .else
    .byte 0x6A, 0x00
    .endif
    .byte 0x68
    .long vec
    jmp dosh_trap_common
    .set vec, vec + 1
    .endr

dosh_trap_common:
    push rax
    push rcx
    push rdx
    push rbx
    push rbp
    push rsi
    push rdi
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    mov rbx, rsp
    and rsp, -16
    call dosh_trap
    mov rsp, rbx
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rdi
    pop rsi
    pop rbp
    pop rbx
    pop rdx
    pop rcx
    pop rax
    add rsp, 16
    iretq
"#
);

extern "C" {
    static dosh_trap_stubs: u8;
}

#[repr(C)]
struct TrapFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rdi: u64,
    rsi: u64,
    rbp: u64,
    rbx: u64,
    rdx: u64,
    rcx: u64,
    rax: u64,
    vector: u64,
    error: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
    rsp: u64,
    ss: u64,
}

#[no_mangle]
extern "sysv64" fn dosh_trap(frame: &TrapFrame) {
    match frame.vector {
        timer::TICK_VECTOR => return timer::on_tick(),
        timer::SPURIOUS_VECTOR => return,
        // Counted, not logged: this may have interrupted the logger.
        NMI => {
            NMIS.fetch_add(1, Ordering::Relaxed);
            return;
        }
        _ => {}
    }
    let cr2: u64;
    // SAFETY: reading CR2 has no side effects.
    unsafe { asm!("mov {}, cr2", out(reg) cr2, options(nomem, nostack, preserves_flags)) };
    crate::log!(
        "trap vector={} error={:#x} rip={:#x} rsp={:#x} cr2={:#x}",
        frame.vector,
        frame.error,
        frame.rip,
        frame.rsp,
        cr2
    );
    super::power_off()
}

pub fn install() {
    // A task state segment, needed only so a double fault gets a known-good
    // stack instead of escalating to a reset.
    let tss: &'static mut [u8; 104] = Box::leak(Box::new([0; 104]));
    let ist1 = Pages::zeroed(16 << 10).leak().as_mut_ptr_range().end as u64;
    tss[36..44].copy_from_slice(&ist1.to_le_bytes());
    tss[102..104].copy_from_slice(&104u16.to_le_bytes()); // no I/O bitmap
    let tss_base = tss.as_ptr() as u64;

    let gdt: &'static mut [u64; 5] = Box::leak(Box::new([
        0,
        0x00AF_9A00_0000_FFFF, // 64-bit code
        0x00CF_9200_0000_FFFF, // data
        103 | (tss_base & 0xFF_FFFF) << 16 | 0x89 << 40 | (tss_base >> 24 & 0xFF) << 56,
        tss_base >> 32,
    ]));

    let stubs = &raw const dosh_trap_stubs as u64;
    let idt: &'static mut [[u64; 2]; 256] = Box::leak(Box::new([[0; 2]; 256]));
    for (vector, gate) in idt.iter_mut().enumerate() {
        let target = stubs + vector as u64 * 16;
        let ist = (vector == DOUBLE_FAULT) as u64;
        // Interrupt gate, present, DPL 0.
        gate[0] = (target & 0xFFFF) | (CODE as u64) << 16 | ist << 32 | 0x8E << 40 | (target >> 16 & 0xFFFF) << 48;
        gate[1] = target >> 32;
    }

    let gdtr = TablePointer { limit: 5 * 8 - 1, base: gdt.as_ptr() as u64 };
    let idtr = TablePointer { limit: 256 * 16 - 1, base: idt.as_ptr() as u64 };
    // SAFETY: the tables are valid and live forever; the far return reloads
    // CS from the new GDT.
    unsafe {
        asm!(
            "lgdt [{gdtr}]",
            "push {code}",
            "lea {tmp}, [rip + 2f]",
            "push {tmp}",
            "retfq",
            "2:",
            "mov ds, {data:x}",
            "mov es, {data:x}",
            "mov ss, {data:x}",
            "mov fs, {data:x}",
            "mov gs, {data:x}",
            "ltr {tss:x}",
            "lidt [{idtr}]",
            gdtr = in(reg) &gdtr,
            idtr = in(reg) &idtr,
            code = const CODE as u64,
            data = in(reg) DATA as u64,
            tss = in(reg) TSS as u64,
            tmp = out(reg) _,
        )
    }
}
