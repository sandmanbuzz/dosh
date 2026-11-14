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
mod mgmt;
mod platform;
mod present;
mod storage;

use alloc::rc::Rc;
use alloc::string::String;
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

    let mut fleet = Fleet {
        hv,
        base: provision.files.into_iter().map(|(name, data)| (name, Rc::from(data))).collect(),
        stock: None,
        disk: storage::Disk::find(),
        wall_clock: provision.wall_clock,
        shared: provision.shared_console,
        capsules: Vec::new(),
    };
    for spec in provision.capsules {
        fleet.launch(spec);
    }

    // Drain the line before announcing: the operator may answer at once.
    let mut channel = provision.stay.then(mgmt::Channel::open);
    if channel.is_some() {
        log!("mgmt event=ready");
    }

    // Round-robin, preemptive: each live capsule in turn gets the processor
    // until it finishes, blocks, or the clock ticks.
    loop {
        let ran = fleet.run_round();
        if let Some(channel) = channel.as_mut() {
            while let Some(input) = channel.poll() {
                match input {
                    mgmt::Input::Command(mgmt::Command::Shutdown) => {
                        log!("mgmt cmd=shutdown status=ok");
                        fleet.shutdown()
                    }
                    mgmt::Input::Command(command) => fleet.command(command),
                    mgmt::Input::Invalid(why) => log!("mgmt status=error reason=\"{why}\""),
                }
            }
        } else if !fleet.any_live() {
            break;
        }
        if !ran {
            platform::timer::idle(); // every live capsule is waiting for input
        }
    }
    fleet.shutdown()
}

/// Everything the supervisor runs: the capsules, and what they are stocked from.
struct Fleet {
    hv: Option<hv::Hv>,
    base: storage::Files,
    /// What a capsule launched now is stocked with; dropped when the disk changes.
    stock: Option<storage::Files>,
    disk: Option<storage::Disk>,
    wall_clock: Option<dosh_dos::DateTime>,
    shared: bool,
    capsules: Vec<Capsule>,
}

/// Machines kept at once: each holds a megabyte of guest memory and a drive
/// out of the supervisor's fixed pool.
const MAX_LIVE: usize = 24;

impl Fleet {
    /// The files a capsule launched now finds on C:: the base drive, with the
    /// persistent disk laid over it.
    fn stock(&mut self) -> storage::Files {
        if let Some(files) = &self.stock {
            return files.clone();
        }
        let mut files = self.base.clone();
        let mut complete = true;
        if let Some(disk) = self.disk.as_mut() {
            let on_disk = disk.files();
            complete = on_disk.is_some();
            let on_disk = on_disk.unwrap_or_default();
            // One entry per name: the disk's file replaces the base drive's.
            files.retain(|(name, _)| {
                !disk.is_whiteout(name) && !on_disk.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
            });
            files.extend(on_disk);
        }
        if complete {
            self.stock = Some(files.clone()); // an unreadable disk is not remembered
        }
        files
    }

    fn launch(&mut self, spec: boot::CapsuleSpec) -> Option<usize> {
        let id = self.capsules.len();
        let files = self.stock();
        // A program may not write more than the disk could keep.
        let room = self.disk.as_mut().map_or(capsule::DISK_ROOM, |d| d.free().min(capsule::DISK_ROOM as u64) as usize);
        let hv = self.hv.as_ref()?;
        let endpoint = present::Endpoint::for_capsule(id, self.shared);
        self.capsules.push(Capsule::launch(id, hv, spec, files, room, endpoint, self.wall_clock));
        Some(id)
    }

    fn any_live(&self) -> bool {
        self.capsules.iter().any(Capsule::is_live)
    }

    /// One turn for every capsule, settling those that finished. Returns
    /// whether any of them did anything.
    fn run_round(&mut self) -> bool {
        let mut ran = false;
        for capsule in &mut self.capsules {
            ran |= capsule.run_slice();
            if !capsule.is_live() && !capsule.is_retired() {
                if capsule.exit_code().is_some() {
                    let changes = capsule.changes();
                    if let Some(disk) = self.disk.as_mut() {
                        disk.store(capsule.id(), &changes, &self.base);
                        self.stock = None;
                    }
                }
                capsule.retire();
            }
        }
        ran
    }

    fn command(&mut self, command: mgmt::Command) {
        use mgmt::Command::*;
        match command {
            List => {
                for c in &self.capsules {
                    log!("mgmt item=capsule id={} program={} status={}", c.id(), c.program(), c.status());
                }
                log!("mgmt cmd=list status=ok count={}", self.capsules.len());
            }
            Launch { limit, program, tail } => {
                let known = |files: &[(String, Rc<[u8]>)]| files.iter().any(|(n, _)| n.eq_ignore_ascii_case(&program));
                let live = self.capsules.iter().filter(|c| !c.is_retired()).count();
                if self.hv.is_none() {
                    log!("mgmt cmd=launch status=error reason=\"no hardware virtualization\"");
                } else if live >= MAX_LIVE {
                    log!("mgmt cmd=launch status=error reason=\"too many capsules ({MAX_LIVE})\"");
                } else if !known(&self.stock()) {
                    log!("mgmt cmd=launch status=error reason=\"no such program: {program}\"");
                } else if let Some(id) = self.launch(boot::CapsuleSpec { cpu_limit: limit, program, tail }) {
                    log!("mgmt cmd=launch status=ok id={id}");
                }
            }
            Suspend(id) => self.act("suspend", id, Capsule::suspend, "not running"),
            Resume(id) => self.act("resume", id, Capsule::resume, "not suspended"),
            Kill(id) => self.act("kill", id, Capsule::kill, "not running"),
            Files => {
                let Some(disk) = self.disk.as_mut() else {
                    return log!("mgmt cmd=files status=error reason=\"no persistent disk\"");
                };
                let Some(files) = disk.files() else {
                    return log!("mgmt cmd=files status=error reason=\"disk unreadable\"");
                };
                for (name, data) in &files {
                    log!("mgmt item=file name={name} size={}", data.len());
                }
                log!("mgmt cmd=files status=ok count={}", files.len());
            }
            Put { name, data } => {
                let Some(disk) = self.disk.as_mut() else {
                    return log!("mgmt cmd=put status=error reason=\"no persistent disk\"");
                };
                self.stock = None;
                match disk.put(&name, &data) {
                    Ok(()) => log!("mgmt cmd=put status=ok name={name} size={}", data.len()),
                    Err(why) => log!("mgmt cmd=put status=error reason=\"{why}\""),
                }
            }
            Get(name) => {
                let Some(disk) = self.disk.as_mut() else {
                    return log!("mgmt cmd=get status=error reason=\"no persistent disk\"");
                };
                match disk.get(&name) {
                    Ok(Some(data)) => {
                        log!("mgmt cmd=get status=ok name={name} size={} hex={}", data.len(), mgmt::to_hex(&data))
                    }
                    Ok(None) => log!("mgmt cmd=get status=error reason=\"no such file\""),
                    Err(why) => log!("mgmt cmd=get status=error reason=\"{why}\""),
                }
            }
            Shutdown => unreachable!("handled by the loop"),
        }
    }

    fn act(&mut self, cmd: &str, id: usize, what: fn(&mut Capsule) -> bool, refusal: &str) {
        let Some(capsule) = self.capsules.get_mut(id) else {
            return log!("mgmt cmd={cmd} status=error reason=\"no capsule {id}\"");
        };
        if what(capsule) {
            log!("mgmt cmd={cmd} status=ok id={id}");
        } else {
            log!("mgmt cmd={cmd} status=error reason=\"capsule {id} is {refusal}\"");
        }
    }

    fn shutdown(&self) -> ! {
        let exited = self.capsules.iter().filter(|c| c.exit_code().is_some()).count();
        if platform::nmis() > 0 {
            log!("nmi taken={}", platform::nmis());
        }
        log!("shutdown capsules={} exited={} faulted={}", self.capsules.len(), exited, self.capsules.len() - exited);
        platform::power_off()
    }
}

#[panic_handler]
fn panic(info: &core::panic::PanicInfo) -> ! {
    match info.location() {
        Some(at) => log!("panic at={}:{} message=\"{}\"", at.file(), at.line(), info.message()),
        None => log!("panic message=\"{}\"", info.message()),
    }
    platform::power_off()
}
