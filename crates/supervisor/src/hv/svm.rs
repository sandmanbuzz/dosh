//! AMD SVM backend.
//!
//! One VMCB and one set of nested page tables per [`Vm`]. Guests are
//! contained three ways: nested paging confines them to their own memory;
//! intercepts turn every port access, MSR access, control-register write and
//! SVM instruction into an exit; and nothing else in the guest-visible
//! machine exists at all.

use core::arch::global_asm;
use core::sync::atomic::{AtomicU64, Ordering};

use super::{Cpu, Exit, Gprs, Segment, Unavailable};
use crate::platform::cpu::{cpuid, rdmsr, wrmsr};
use crate::platform::{timer, Pages, PAGE};

pub const BACKEND: &str = "svm";

const MSR_EFER: u32 = 0xC000_0080;
const MSR_VM_CR: u32 = 0xC001_0114;
const MSR_VM_HSAVE_PA: u32 = 0xC001_0117;
const EFER_SVME: u64 = 1 << 12;
const VM_CR_SVMDIS: u64 = 1 << 4;

/// VMCB layout: byte offsets into the control area and, from 0x400, the
/// state-save area. (AMD64 Architecture Programmer's Manual vol. 2, app. B.)
mod vmcb {
    pub const INTERCEPT_CR: usize = 0x000;
    pub const INTERCEPT_DR: usize = 0x004;
    pub const INTERCEPT_EXCEPTION: usize = 0x008;
    pub const INTERCEPT_MISC1: usize = 0x00C;
    pub const INTERCEPT_MISC2: usize = 0x010;
    pub const IOPM_BASE_PA: usize = 0x040;
    pub const MSRPM_BASE_PA: usize = 0x048;
    pub const GUEST_ASID: usize = 0x058;
    pub const TLB_CONTROL: usize = 0x05C;
    pub const VIRTUAL_INTERRUPT: usize = 0x060;
    pub const EXIT_CODE: usize = 0x070;
    pub const EXIT_INFO_1: usize = 0x078;
    pub const EXIT_INFO_2: usize = 0x080;
    pub const NESTED_CONTROL: usize = 0x090;
    pub const NESTED_CR3: usize = 0x0B0;

    pub const ES: usize = 0x400;
    pub const CS: usize = 0x410;
    pub const SS: usize = 0x420;
    pub const DS: usize = 0x430;
    pub const FS: usize = 0x440;
    pub const GS: usize = 0x450;
    pub const GDTR: usize = 0x460;
    pub const LDTR: usize = 0x470;
    pub const IDTR: usize = 0x480;
    pub const TR: usize = 0x490;
    pub const EFER: usize = 0x4D0;
    pub const CR0: usize = 0x558;
    pub const DR7: usize = 0x560;
    pub const DR6: usize = 0x568;
    pub const RFLAGS: usize = 0x570;
    pub const RIP: usize = 0x578;
    pub const RSP: usize = 0x5D8;
    pub const RAX: usize = 0x5F8;
    pub const G_PAT: usize = 0x668;
}

// Intercepts. A guest gets native execution of ordinary instructions and
// nothing else.
/// Writes to CR0, CR3, CR4 and CR8.
const CR_WRITES: u32 = (1 << 0 | 1 << 3 | 1 << 4 | 1 << 8) << 16;
/// All debug-register access: SVM does not switch DR0-DR3 for us.
const DR_ALL: u32 = 0xFFFF_FFFF;
/// Machine check.
const EXCEPTIONS: u32 = 1 << 18;
/// INTR, NMI, SMI, INIT, HLT, port I/O, MSRs, task switch, shutdown.
const MISC1: u32 = 0xF | 1 << 24 | 1 << 27 | 1 << 28 | 1 << 29 | 1 << 31;
/// VMRUN, VMMCALL, VMLOAD, VMSAVE, STGI, CLGI, SKINIT, WBINVD, MONITOR,
/// MWAIT, XSETBV.
const MISC2: u32 = 0x7F | 1 << 9 | 1 << 10 | 1 << 11 | 1 << 13;

/// VMCB virtual-interrupt word: a request pending (V_IRQ), TPR ignored so the
/// request is taken at priority zero, and host ticks not masked by the guest.
const V_IRQ: u64 = 1 << 8;
const V_IGN_TPR: u64 = 1 << 20;
const V_INTR_MASKING: u64 = 1 << 24;

const EXIT_INTR: u64 = 0x60;
const EXIT_NMI: u64 = 0x61;
const EXIT_HLT: u64 = 0x78;
const EXIT_IOIO: u64 = 0x7B;
const EXIT_SHUTDOWN: u64 = 0x7F;
const EXIT_NPF: u64 = 0x400;
const EXIT_INVALID: u64 = u64::MAX;

// World switch. Called with the System V convention:
//   rdi = guest VMCB, rsi = guest Gprs, rdx = host VMCB,
//   rcx = guest FXSAVE area, r8 = host FXSAVE area
// VMRUN itself switches RAX, RSP, RIP, RFLAGS, the control registers and the
// main segment registers. The rest is done here: the remaining general
// registers, the x87/SSE state, and (VMLOAD/VMSAVE) the hidden segment
// state.
//
// The supervisor runs with interrupts disabled, but a guest must not: the
// clock tick is what takes the processor back from it. So, with the global
// interrupt flag clear, interrupts are enabled just before VMRUN (to be
// recorded as the host's wish) and disabled again straight after; a tick
// during guest execution then forces an exit, and stays pending until the
// supervisor chooses to take it.
global_asm!(
    r#"
    .section .text
    .global dosh_svm_enter
dosh_svm_enter:
    push rbx
    push rbp
    push r12
    push r13
    push r14
    push r15
    push r8                     // [rsp+32] host FXSAVE area
    push rcx                    // [rsp+24] guest FXSAVE area
    push rsi                    // [rsp+16] guest Gprs
    push rdx                    // [rsp+8]  host VMCB
    push rdi                    // [rsp]    guest VMCB
    .byte 0x0F, 0x01, 0xDD      // clgi
    fxsave64 [r8]
    fxrstor64 [rcx]
    mov rax, rdx
    .byte 0x0F, 0x01, 0xDB      // vmsave: host hidden state
    mov rax, rsi
    mov rcx, [rax + 0x08]
    mov rdx, [rax + 0x10]
    mov rbx, [rax + 0x18]
    mov rbp, [rax + 0x28]
    mov rsi, [rax + 0x30]
    mov rdi, [rax + 0x38]
    mov r8,  [rax + 0x40]
    mov r9,  [rax + 0x48]
    mov r10, [rax + 0x50]
    mov r11, [rax + 0x58]
    mov r12, [rax + 0x60]
    mov r13, [rax + 0x68]
    mov r14, [rax + 0x70]
    mov r15, [rax + 0x78]
    mov rax, [rsp]
    sti
    .byte 0x0F, 0x01, 0xDA      // vmload: guest hidden state
    .byte 0x0F, 0x01, 0xD8      // vmrun
    cli
    .byte 0x0F, 0x01, 0xDB      // vmsave: guest hidden state
    mov rax, [rsp + 16]
    mov [rax + 0x08], rcx
    mov [rax + 0x10], rdx
    mov [rax + 0x18], rbx
    mov [rax + 0x28], rbp
    mov [rax + 0x30], rsi
    mov [rax + 0x38], rdi
    mov [rax + 0x40], r8
    mov [rax + 0x48], r9
    mov [rax + 0x50], r10
    mov [rax + 0x58], r11
    mov [rax + 0x60], r12
    mov [rax + 0x68], r13
    mov [rax + 0x70], r14
    mov [rax + 0x78], r15
    mov rax, [rsp + 8]
    .byte 0x0F, 0x01, 0xDA      // vmload: host hidden state
    mov rax, [rsp + 24]
    fxsave64 [rax]
    mov rax, [rsp + 32]
    fxrstor64 [rax]
    .byte 0x0F, 0x01, 0xDC      // stgi
    add rsp, 40
    pop r15
    pop r14
    pop r13
    pop r12
    pop rbp
    pop rbx
    ret
"#
);

extern "sysv64" {
    fn dosh_svm_enter(vmcb: u64, gprs: *mut Gprs, host_vmcb: u64, guest_fx: *mut u8, host_fx: *mut u8);
}

/// The machine's virtualization extension, switched on.
pub struct Hv {
    host_vmcb: u64,
    host_fx: u64,
    /// Permission maps shared by every guest: all ports, all MSRs intercepted.
    iopm: u64,
    msrpm: u64,
}

impl Hv {
    pub fn enable() -> Result<Self, Unavailable> {
        let extended = cpuid(0x8000_0000).0;
        if extended < 0x8000_000A || cpuid(0x8000_0001).2 & (1 << 2) == 0 {
            return Err(Unavailable::NoExtension);
        }
        if cpuid(0x8000_000A).3 & 1 == 0 {
            return Err(Unavailable::NoNestedPaging);
        }
        // SAFETY: these MSRs exist on every processor that reports SVM.
        unsafe {
            if rdmsr(MSR_VM_CR) & VM_CR_SVMDIS != 0 {
                return Err(Unavailable::DisabledByFirmware);
            }
            wrmsr(MSR_EFER, rdmsr(MSR_EFER) | EFER_SVME);
            wrmsr(MSR_VM_HSAVE_PA, Pages::zeroed(PAGE).leak().as_ptr() as u64);
        }
        let all_ones = |bytes| {
            let map = Pages::zeroed(bytes).leak();
            map.fill(0xFF);
            map.as_ptr() as u64
        };
        Ok(Self {
            host_vmcb: Pages::zeroed(PAGE).leak().as_ptr() as u64,
            host_fx: Pages::zeroed(PAGE).leak().as_ptr() as u64,
            iopm: all_ones(3 * PAGE),
            msrpm: all_ones(2 * PAGE),
        })
    }

    /// A new real-mode guest with `ram_bytes` of memory at guest-physical
    /// zero. The megabyte above it wraps back to the start of memory, as it
    /// does on a PC with the A20 line off.
    pub fn create_vm(&self, ram_bytes: usize) -> Vm {
        static NEXT_ID: AtomicU64 = AtomicU64::new(1);
        assert!(ram_bytes.is_multiple_of(PAGE) && ram_bytes <= 256 * PAGE);
        let ram = Pages::zeroed(ram_bytes);
        let mut tables = Pages::zeroed(4 * PAGE);

        // Nested page tables: one 4 KiB-granular table covers the guest.
        // Nested walks are user-mode walks, hence the U/S bit throughout.
        const TABLE: u64 = 0x07; // present, writable, user
        let (base, ram_base) = (tables.addr(), ram.addr());
        let words = tables.words();
        for level in 0..3 {
            words[level * 512] = (base + (level as u64 + 1) * PAGE as u64) | TABLE;
        }
        let pages = ram_bytes / PAGE;
        for page in 0..pages + 16 {
            words[3 * 512 + page] = (ram_base + ((page % pages) * PAGE) as u64) | TABLE;
        }

        let mut vm = Vm {
            id: NEXT_ID.fetch_add(1, Ordering::Relaxed),
            cpu: Cpu { rflags: 0x2, ..Cpu::default() },
            vmcb: Pages::zeroed(PAGE),
            fx: Pages::zeroed(PAGE),
            ram,
            _tables: tables,
            host_vmcb: self.host_vmcb,
            host_fx: self.host_fx,
        };
        // Power-on x87/SSE state: FCW 037Fh, MXCSR 1F80h, all registers empty.
        vm.fx.bytes()[0..2].copy_from_slice(&0x037Fu16.to_le_bytes());
        vm.fx.bytes()[24..28].copy_from_slice(&0x1F80u32.to_le_bytes());

        vm.set(vmcb::INTERCEPT_CR, CR_WRITES);
        vm.set(vmcb::INTERCEPT_DR, DR_ALL);
        vm.set(vmcb::INTERCEPT_EXCEPTION, EXCEPTIONS);
        vm.set(vmcb::INTERCEPT_MISC1, MISC1);
        vm.set(vmcb::INTERCEPT_MISC2, MISC2);
        vm.set(vmcb::IOPM_BASE_PA, self.iopm);
        vm.set(vmcb::MSRPM_BASE_PA, self.msrpm);
        vm.set(vmcb::GUEST_ASID, 1u32);
        // V_INTR_MASKING: the guest's interrupt flag governs only its own
        // (virtual) interrupts. It cannot hold off the supervisor's.
        vm.set(vmcb::VIRTUAL_INTERRUPT, V_INTR_MASKING);
        vm.set(vmcb::NESTED_CONTROL, 1u64);
        vm.set(vmcb::NESTED_CR3, base);

        // Real mode, as after reset. The guest's EFER.SVME is a VMRUN
        // requirement, not something the guest can use: EFER is intercepted.
        vm.set(vmcb::EFER, EFER_SVME);
        vm.set(vmcb::CR0, 0x10u64);
        vm.set(vmcb::DR6, 0xFFFF_0FF0u64);
        vm.set(vmcb::DR7, 0x400u64);
        vm.set(vmcb::G_PAT, 0x0007_0406_0007_0406u64);
        vm.set_segment(vmcb::GDTR, &Segment { limit: 0xFFFF, ..Segment::default() });
        vm.set_segment(vmcb::IDTR, &Segment { limit: 0x3FF, ..Segment::default() });
        vm.set_segment(vmcb::LDTR, &Segment { limit: 0xFFFF, attr: 0x82, ..Segment::default() });
        vm.set_segment(vmcb::TR, &Segment { limit: 0xFFFF, attr: 0x8B, ..Segment::default() });
        vm
    }
}

/// The last guest to run. Every guest shares one address-space ID, so the
/// TLB is flushed whenever a different guest is entered.
static LAST_RUN: AtomicU64 = AtomicU64::new(0);

pub struct Vm {
    id: u64,
    pub cpu: Cpu,
    vmcb: Pages,
    fx: Pages,
    ram: Pages,
    _tables: Pages,
    host_vmcb: u64,
    host_fx: u64,
}

impl Vm {
    fn set<T: Copy>(&mut self, offset: usize, value: T) {
        let bytes = &mut self.vmcb.bytes()[offset..offset + size_of::<T>()];
        // SAFETY: the slice is exactly `T`-sized; unaligned writes are fine.
        unsafe { bytes.as_mut_ptr().cast::<T>().write_unaligned(value) }
    }

    fn get<T: Copy>(&mut self, offset: usize) -> T {
        let bytes = &self.vmcb.bytes()[offset..offset + size_of::<T>()];
        // SAFETY: as above; only used with plain integer types.
        unsafe { bytes.as_ptr().cast::<T>().read_unaligned() }
    }

    fn set_segment(&mut self, offset: usize, seg: &Segment) {
        self.set(offset, seg.selector);
        self.set(offset + 2, seg.attr);
        self.set(offset + 4, seg.limit);
        self.set(offset + 8, seg.base);
    }

    fn get_segment(&mut self, offset: usize) -> Segment {
        Segment {
            selector: self.get(offset),
            attr: self.get(offset + 2),
            limit: self.get(offset + 4),
            base: self.get(offset + 8),
        }
    }

    pub fn ram(&mut self) -> &mut [u8] {
        self.ram.bytes()
    }

    /// Ask the processor to deliver interrupt `vector` to the guest as soon
    /// as the guest's interrupt flag allows. The processor clears the request
    /// when it takes the interrupt.
    pub fn post_interrupt(&mut self, vector: u8) {
        self.set(vmcb::VIRTUAL_INTERRUPT, V_INTR_MASKING | V_IGN_TPR | V_IRQ | (vector as u64) << 32);
    }

    /// Whether a posted interrupt has not been taken yet.
    pub fn interrupt_pending(&mut self) -> bool {
        self.get::<u64>(vmcb::VIRTUAL_INTERRUPT) & V_IRQ != 0
    }

    /// Execute guest instructions on the processor until the guest needs the
    /// supervisor.
    pub fn run(&mut self) -> Exit {
        const SEGMENTS: [usize; 6] = [vmcb::CS, vmcb::DS, vmcb::ES, vmcb::SS, vmcb::FS, vmcb::GS];
        let cpu = self.cpu;
        self.set(vmcb::RAX, cpu.gprs.rax);
        self.set(vmcb::RSP, cpu.gprs.rsp);
        self.set(vmcb::RIP, cpu.rip);
        self.set(vmcb::RFLAGS, cpu.rflags);
        for (offset, seg) in SEGMENTS.into_iter().zip([cpu.cs, cpu.ds, cpu.es, cpu.ss, cpu.fs, cpu.gs]) {
            self.set_segment(offset, &seg);
        }
        let switched = LAST_RUN.swap(self.id, Ordering::Relaxed) != self.id;
        self.set(vmcb::TLB_CONTROL, switched as u32);

        // SAFETY: the VMCB, register block and FXSAVE areas are valid,
        // identity-mapped and exclusively ours; SVM is enabled.
        unsafe {
            dosh_svm_enter(
                self.vmcb.addr(),
                &mut self.cpu.gprs,
                self.host_vmcb,
                self.fx.bytes().as_mut_ptr(),
                self.host_fx as *mut u8,
            )
        }

        self.cpu.gprs.rax = self.get(vmcb::RAX);
        self.cpu.gprs.rsp = self.get(vmcb::RSP);
        self.cpu.rip = self.get(vmcb::RIP);
        self.cpu.rflags = self.get(vmcb::RFLAGS);
        let [cs, ds, es, ss, fs, gs] = SEGMENTS.map(|offset| self.get_segment(offset));
        (self.cpu.cs, self.cpu.ds, self.cpu.es, self.cpu.ss, self.cpu.fs, self.cpu.gs) = (cs, ds, es, ss, fs, gs);

        let code: u64 = self.get(vmcb::EXIT_CODE);
        let info1: u64 = self.get(vmcb::EXIT_INFO_1);
        let info2: u64 = self.get(vmcb::EXIT_INFO_2);
        match code {
            EXIT_HLT => Exit::Halt,
            EXIT_IOIO => Exit::Io {
                port: (info1 >> 16) as u16,
                input: info1 & 1 != 0,
                bytes: (info1 >> 4 & 7) as u8,
                string: info1 & (1 << 2) != 0,
                next_rip: info2,
            },
            EXIT_NPF => Exit::MemoryFault { address: info2 },
            EXIT_SHUTDOWN => Exit::Shutdown,
            EXIT_INTR | EXIT_NMI => {
                timer::accept();
                Exit::Preempted
            }
            // The processor refused the guest state we built: our bug.
            EXIT_INVALID => panic!("VMRUN rejected the guest state of vm {}", self.id),
            code => Exit::Forbidden { code, info: info1 },
        }
    }
}
