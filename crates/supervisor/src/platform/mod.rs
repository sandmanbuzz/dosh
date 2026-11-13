//! The physical machine: taking it over from the firmware, and the few
//! pieces of it the supervisor touches directly.

pub mod cpu;
mod tables;
pub mod timer;
pub mod uart;

use alloc::alloc::{alloc_zeroed, dealloc, handle_alloc_error, Layout};
use core::arch::asm;
use core::ptr::NonNull;

use crate::boot::Provision;

pub const PAGE: usize = 4096;
const STACK_BYTES: usize = 256 << 10;

/// Page-aligned, zeroed memory from the supervisor pool. The supervisor runs
/// identity-mapped, so its address is also its physical address.
pub struct Pages {
    ptr: NonNull<u8>,
    layout: Layout,
}

impl Pages {
    pub fn zeroed(bytes: usize) -> Self {
        let layout = Layout::from_size_align(bytes.next_multiple_of(PAGE), PAGE).unwrap();
        // SAFETY: the layout has non-zero size.
        let ptr = NonNull::new(unsafe { alloc_zeroed(layout) }).unwrap_or_else(|| handle_alloc_error(layout));
        Self { ptr, layout }
    }

    pub fn addr(&self) -> u64 {
        self.ptr.as_ptr() as u64
    }

    pub fn bytes(&mut self) -> &mut [u8] {
        // SAFETY: the allocation is `layout.size()` bytes and exclusively ours.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr(), self.layout.size()) }
    }

    /// The same memory as 64-bit words: page-table entries, control fields.
    pub fn words(&mut self) -> &mut [u64] {
        // SAFETY: as above; page alignment satisfies u64.
        unsafe { core::slice::from_raw_parts_mut(self.ptr.as_ptr().cast(), self.layout.size() / 8) }
    }

    pub fn leak(self) -> &'static mut [u8] {
        let mut this = core::mem::ManuallyDrop::new(self);
        // SAFETY: never freed, so the borrow may live forever.
        unsafe { &mut *(this.bytes() as *mut [u8]) }
    }
}

impl Drop for Pages {
    fn drop(&mut self) {
        // SAFETY: allocated in `zeroed` with this layout.
        unsafe { dealloc(self.ptr.as_ptr(), self.layout) }
    }
}

type Entry = fn(Provision) -> !;

/// Leave the firmware's world for the supervisor's: our own page tables,
/// descriptor tables, clock and stack. Then call `entry`.
pub fn take_over(top_of_memory: u64, provision: Provision, entry: Entry) -> ! {
    // SAFETY: nothing of ours can take an interrupt until `timer::start`.
    unsafe { asm!("cli", options(nomem, nostack)) };
    install_paging(top_of_memory);
    tables::install();
    timer::start();

    let stack = Pages::zeroed(STACK_BYTES).leak();
    let top = stack.as_mut_ptr_range().end as usize;
    let mut payload = Some((provision, entry));
    // SAFETY: the new stack is ours and big enough; the trampoline never
    // returns, so nothing of the old stack is needed except `payload`, which
    // stays intact because nothing reuses firmware memory.
    unsafe {
        asm!(
            "mov rsp, {sp}",
            "call {trampoline}",
            sp = in(reg) top - 32, // the callee's shadow space
            trampoline = sym trampoline,
            in("rcx") &mut payload,
            options(noreturn),
        )
    }
}

extern "efiapi" fn trampoline(payload: *mut Option<(Provision, Entry)>) -> ! {
    // SAFETY: `take_over` passes a live, initialised payload.
    let (provision, entry) = unsafe { (*payload).take() }.unwrap();
    entry(provision)
}

/// Identity-map all of memory, and at least the low 4 GiB where the
/// processor's own devices live, with 2 MiB pages.
fn install_paging(top_of_memory: u64) {
    const PRESENT_WRITABLE: u64 = 0x03;
    const LARGE: u64 = 0x80;
    let gib = top_of_memory.div_ceil(1 << 30).max(4) as usize;
    assert!(gib <= 512, "more memory than one PML4 entry maps");

    let pml4 = Pages::zeroed(PAGE).leak();
    let mut pdpt = Pages::zeroed(PAGE);
    for g in 0..gib {
        let mut pd = Pages::zeroed(PAGE);
        for (i, entry) in pd.words().iter_mut().enumerate() {
            *entry = ((g as u64) << 30) | ((i as u64) << 21) | PRESENT_WRITABLE | LARGE;
        }
        pdpt.words()[g] = pd.addr() | PRESENT_WRITABLE;
        pd.leak();
    }
    let root = pml4.as_ptr() as u64;
    pml4[..8].copy_from_slice(&(pdpt.addr() | PRESENT_WRITABLE).to_le_bytes());
    pdpt.leak();
    // SAFETY: the new tables map everything the old ones did, at the same
    // addresses. Clearing CR4.PGE first drops any global mappings.
    unsafe {
        asm!(
            "mov {tmp}, cr4",
            "and {tmp}, {no_pge}",
            "mov cr4, {tmp}",
            "mov cr3, {root}",
            tmp = out(reg) _,
            no_pge = in(reg) !(1u64 << 7),
            root = in(reg) root,
            options(nostack),
        )
    }
}

/// Turn the machine off through the firmware's runtime services, which
/// outlive boot services.
pub fn power_off() -> ! {
    uefi::runtime::reset(uefi::runtime::ResetType::SHUTDOWN, uefi::Status::SUCCESS, None)
}
