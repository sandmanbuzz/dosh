//! The DOSH supervisor.
//!
//! An EFI application that takes the machine away from the firmware and uses
//! it to host DOS programs, each in its own hardware-virtualized capsule.
//!
//! ```text
//! firmware phase   boot      read the boot manifest and drive C: from the ESP
//!                            ExitBootServices
//! supervisor phase platform  own stack, page tables, descriptor tables
//!                  hv        enable hardware virtualization
//!                  capsule   build, run and retire guests
//!                  present   carry capsule consoles to transports
//!                            shut the machine down when no work remains
//! ```

#![no_std]
#![no_main]

extern crate alloc;

mod boot;
mod capsule;
mod device;
mod hv;
mod platform;
mod present;

use alloc::vec::Vec;

use capsule::Capsule;
use uefi::prelude::*;

/// Write one event line to the supervisor log (COM1).
///
/// The log is the supervisor's own voice and the lab's source of truth:
/// `[dosh] <subject> key=value ...`. Capsule consoles never appear here.
#[macro_export]
macro_rules! log {
    ($($arg:tt)*) => { $crate::log_event(format_args!($($arg)*)) };
}

pub fn log_event(event: core::fmt::Arguments) {
    use core::fmt::Write;
    let mut port = platform::uart::Uart::COM1;
    let _ = writeln!(port, "[dosh] {event}");
}

#[entry]
fn efi_main() -> Status {
    platform::uart::Uart::COM1.init();
    log!("boot stage=firmware version={}", env!("CARGO_PKG_VERSION"));
    let provision = boot::provision();
    // SAFETY: nothing from boot services is used past this point; the
    // supervisor's memory was allocated up front by `boot::provision`.
    let memory_map = unsafe { uefi::boot::exit_boot_services(None) };
    let top = boot::top_of_memory(&memory_map);
    // The map lives in firmware pool memory that can no longer be freed.
    core::mem::forget(memory_map);
    platform::take_over(top, provision, supervise)
}

/// The supervisor proper. Runs on its own stack with the machine in hand.
fn supervise(provision: boot::Provision) -> ! {
    log!("boot stage=supervisor capsules={}", provision.capsules.len());
    if let Some(error) = &provision.error {
        log!("boot error=\"{error}\"");
    }

    let hv = match hv::Hv::enable() {
        Ok(hv) => {
            log!("hv backend={} status=enabled", hv::BACKEND);
            Some(hv)
        }
        Err(e) => {
            log!("hv status=unavailable reason=\"{e}\"");
            None
        }
    };

    let mut capsules: Vec<Capsule> = Vec::new();
    if let Some(hv) = &hv {
        for (id, spec) in provision.capsules.into_iter().enumerate() {
            let endpoint = present::Endpoint::for_capsule(id);
            capsules.push(Capsule::launch(id, hv, spec, endpoint));
        }
    }

    // Round-robin, preemptive: each live capsule in turn gets the processor
    // until it finishes, blocks, or the clock ticks.
    loop {
        let mut live = false;
        let mut ran = false;
        for capsule in &mut capsules {
            ran |= capsule.run_slice();
            live |= capsule.is_live();
        }
        if !live {
            break;
        }
        if !ran {
            platform::timer::idle(); // every live capsule is waiting for input
        }
    }

    let exited = capsules.iter().filter(|c| c.exit_code().is_some()).count();
    if platform::nmis() > 0 {
        log!("nmi taken={}", platform::nmis());
    }
    log!("shutdown capsules={} exited={} faulted={}", capsules.len(), exited, capsules.len() - exited);
    platform::power_off()
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    match info.location() {
        Some(at) => log!("panic at={}:{} message=\"{}\"", at.file(), at.line(), info.message()),
        None => log!("panic message=\"{}\"", info.message()),
    }
    platform::power_off()
}
