//! Hardware virtualization: running guest code natively on the processor,
//! inside a boundary the processor itself enforces.
//!
//! This module is the only place that knows which virtualization extension
//! the machine has. The rest of the supervisor sees a [`Vm`]: guest memory,
//! guest register state ([`Cpu`]), and `run`, which executes guest
//! instructions on the physical processor until something needs the
//! supervisor and reports it as an [`Exit`].
//!
//! AMD SVM is the one backend so far. An Intel VT-x backend would present
//! the same `Hv`/`Vm` surface from a sibling module.

mod svm;

pub use svm::{Hv, Vm, BACKEND};

/// A segment register, including the hidden descriptor state.
#[derive(Debug, Default, Clone, Copy)]
pub struct Segment {
    pub selector: u16,
    pub base: u64,
    pub limit: u32,
    /// Descriptor attributes: type, S, DPL, P in the low byte; AVL, L, D/B, G
    /// above it.
    pub attr: u16,
}

impl Segment {
    /// A real-mode segment: base is sixteen times the selector.
    pub fn real(selector: u16, code: bool) -> Self {
        Self { selector, base: (selector as u64) << 4, limit: 0xFFFF, attr: if code { 0x9B } else { 0x93 } }
    }
}

/// Guest general-purpose registers, in instruction-encoding order.
#[derive(Debug, Default, Clone, Copy)]
#[repr(C)]
pub struct Gprs {
    pub rax: u64,
    pub rcx: u64,
    pub rdx: u64,
    pub rbx: u64,
    pub rsp: u64,
    pub rbp: u64,
    pub rsi: u64,
    pub rdi: u64,
    pub r8_r15: [u64; 8],
}

/// The guest processor state the supervisor reads and edits between runs.
#[derive(Debug, Default, Clone, Copy)]
pub struct Cpu {
    pub gprs: Gprs,
    pub rip: u64,
    pub rflags: u64,
    pub cs: Segment,
    pub ds: Segment,
    pub es: Segment,
    pub ss: Segment,
    pub fs: Segment,
    pub gs: Segment,
}

/// Why the guest stopped running.
#[derive(Debug, Clone, Copy)]
pub enum Exit {
    /// The guest executed `HLT`. `rip` still points at it.
    Halt,
    /// The guest executed a port I/O instruction, which has not happened
    /// yet. Resuming at `next_rip` skips it.
    Io { port: u16, input: bool, bytes: u8, string: bool, next_rip: u64 },
    /// The guest touched physical memory it does not have.
    MemoryFault { address: u64 },
    /// The guest faulted its way into a shutdown (a triple fault).
    Shutdown,
    /// An external interrupt, the supervisor's clock, took the processor
    /// back. The guest did nothing and can simply be run again. The guest
    /// itself sees no interrupt: its own timer is driven by its BIOS clock.
    Preempted,
    /// A non-maskable interrupt reached the supervisor. It is the host's
    /// business: the guest is not interrupted, and can be run again.
    Nmi,
    /// The guest attempted something no capsule may do, identified by the
    /// backend's raw exit code.
    Forbidden { code: u64, info: u64 },
}

#[derive(Debug, Clone, Copy)]
pub enum Unavailable {
    NoExtension,
    DisabledByFirmware,
    NoNestedPaging,
}

impl core::fmt::Display for Unavailable {
    fn fmt(&self, f: &mut core::fmt::Formatter) -> core::fmt::Result {
        f.write_str(match self {
            Self::NoExtension => "processor has no supported virtualization extension (AMD SVM)",
            Self::DisabledByFirmware => "SVM is disabled and locked by firmware",
            Self::NoNestedPaging => "processor lacks nested paging",
        })
    }
}
