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

use alloc::format;
use alloc::rc::Rc;
use alloc::string::{String, ToString};
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
        stocks: Vec::new(),
        disks: storage::Disk::find_all(),
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
    /// What a capsule launched now is stocked with, by volume (none: the base
    /// drive alone); a volume's entry is dropped when its disk changes.
    stocks: Vec<(Option<String>, storage::Files)>,
    disks: Vec<storage::Disk>,
    wall_clock: Option<dosh_dos::DateTime>,
    shared: bool,
    capsules: Vec<Capsule>,
}

/// Machines kept at once: each holds a megabyte of guest memory and a drive
/// out of the supervisor's fixed pool.
const MAX_LIVE: usize = 24;

/// What finished capsules may hold for the operator, together. Past it a
/// finishing capsule's changes are dropped and logged.
const HELD_LIMIT: usize = 16 << 20;

impl Fleet {
    /// The persistent volume a capsule asking for `requested` is kept on.
    /// Unnamed is the default volume, if there is one; `NONE` is no volume.
    fn volume(&self, requested: Option<&str>) -> Result<Option<String>, String> {
        match requested {
            Some("NONE") => Ok(None),
            Some(name) if self.disks.iter().any(|d| d.name() == name) => Ok(Some(name.to_string())),
            Some(name) => Err(format!("no volume {name}")),
            None => Ok(self.disks.iter().any(|d| d.name() == storage::DEFAULT).then(|| storage::DEFAULT.to_string())),
        }
    }

    /// The disk an operator command names; unnamed is the default volume.
    fn disk(&mut self, requested: Option<&str>) -> Result<&mut storage::Disk, String> {
        let name = match requested {
            Some("NONE") => return Err("NONE is not a volume".to_string()),
            Some(name) => name,
            None => storage::DEFAULT,
        };
        self.stocks.retain(|(v, _)| v.as_deref() != Some(name)); // what follows may change it
        match self.disks.iter_mut().find(|d| d.name() == name) {
            Some(disk) => Ok(disk),
            None if requested.is_none() => Err("no persistent disk".to_string()),
            None => Err(format!("no volume {name}")),
        }
    }

    /// The files a capsule launched now finds on C:: the base drive, with
    /// `volume` laid over it.
    /// None if the volume's disk cannot be read: a capsule must not start
    /// on a drive that is silently missing its files.
    fn stock(&mut self, volume: Option<&str>) -> Option<storage::Files> {
        if let Some((_, files)) = self.stocks.iter().find(|(v, _)| v.as_deref() == volume) {
            return Some(files.clone());
        }
        let mut files = self.base.clone();
        if let Some(disk) = self.disks.iter_mut().find(|d| Some(d.name()) == volume) {
            let on_disk = disk.files()?;
            // One entry per name: the disk's file replaces the base drive's.
            files.retain(|(name, _)| {
                !disk.is_whiteout(name) && !on_disk.iter().any(|(n, _)| n.eq_ignore_ascii_case(name))
            });
            files.extend(on_disk);
        }
        self.stocks.push((volume.map(str::to_string), files.clone()));
        Some(files)
    }

    fn launch(&mut self, mut spec: boot::CapsuleSpec) -> Option<usize> {
        let id = self.capsules.len();
        let volume = match self.volume(spec.volume.as_deref()) {
            Ok(volume) => volume,
            Err(why) => {
                log!("capsule id={id} event=refused program={} reason=\"{why}\"", spec.program);
                return None;
            }
        };
        spec.volume = volume.clone(); // from here on, None means no volume
        let Some(files) = self.stock(volume.as_deref()) else {
            log!("capsule id={id} event=refused program={} reason=\"volume cannot be read\"", spec.program);
            return None;
        };
        // A program may not write more than the disk could keep.
        let room = self
            .disks
            .iter_mut()
            .find(|d| Some(d.name()) == volume.as_deref())
            .map_or(capsule::DISK_ROOM, |d| d.free().min(capsule::DISK_ROOM as u64) as usize);
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
        let mut held_total: usize = self.capsules.iter().map(|c| c.held().bytes()).sum();
        for capsule in &mut self.capsules {
            ran |= capsule.run_slice();
            if !capsule.is_live() && !capsule.is_retired() {
                if capsule.exit_code().is_some() {
                    let changes = capsule.changes();
                    let name = capsule.volume().map(str::to_string);
                    let kept = match self.disks.iter_mut().find(|d| Some(d.name()) == name.as_deref()) {
                        Some(disk) => {
                            self.stocks.retain(|(v, _)| *v != name);
                            disk.store(capsule.id(), &changes, &self.base)
                        }
                        None => changes, // no volume: the operator may collect it
                    };
                    if held_total + kept.bytes() > HELD_LIMIT {
                        log!(
                            "capsule id={} event=error reason=\"too much held for the operator; changes dropped\"",
                            capsule.id()
                        );
                    } else {
                        held_total += kept.bytes();
                        capsule.hold(kept);
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
            Launch(spec) => {
                let live = self.capsules.iter().filter(|c| !c.is_retired()).count();
                let volume = self.volume(spec.volume.as_deref());
                if self.hv.is_none() {
                    log!("mgmt cmd=launch status=error reason=\"no hardware virtualization\"");
                } else if live >= MAX_LIVE {
                    log!("mgmt cmd=launch status=error reason=\"too many capsules ({MAX_LIVE})\"");
                } else if let Err(why) = &volume {
                    log!("mgmt cmd=launch status=error reason=\"{why}\"");
                } else if let Some(files) = self.stock(volume.ok().flatten().as_deref()) {
                    if !files.iter().any(|(n, _)| n.eq_ignore_ascii_case(&spec.program)) {
                        log!("mgmt cmd=launch status=error reason=\"no such program: {}\"", spec.program);
                    } else if let Some(id) = self.launch(spec) {
                        log!("mgmt cmd=launch status=ok id={id}");
                    }
                } else {
                    log!("mgmt cmd=launch status=error reason=\"volume cannot be read\"");
                }
            }
            Suspend(id) => self.act("suspend", id, Capsule::suspend, "not running"),
            Resume(id) => self.act("resume", id, Capsule::resume, "not suspended"),
            Kill(id) => self.act("kill", id, Capsule::kill, "not running"),
            Files { volume } => match self.disk(volume.as_deref()).map(|disk| disk.files()) {
                Err(why) => log!("mgmt cmd=files status=error reason=\"{why}\""),
                Ok(None) => log!("mgmt cmd=files status=error reason=\"disk unreadable\""),
                Ok(Some(files)) => {
                    for (name, data) in &files {
                        log!("mgmt item=file name={name} size={}", data.len());
                    }
                    log!("mgmt cmd=files status=ok count={}", files.len());
                }
            },
            Put { volume, name, data } => match self.disk(volume.as_deref()).map(|disk| disk.put(&name, &data)) {
                Ok(Ok(())) => log!("mgmt cmd=put status=ok name={name} size={}", data.len()),
                Ok(Err(why)) | Err(why) => log!("mgmt cmd=put status=error reason=\"{why}\""),
            },
            Get { volume, name } => match self.disk(volume.as_deref()).map(|disk| disk.get(&name)) {
                Ok(Ok(Some(data))) => {
                    log!("mgmt cmd=get status=ok name={name} size={} hex={}", data.len(), mgmt::to_hex(&data))
                }
                Ok(Ok(None)) => log!("mgmt cmd=get status=error reason=\"no such file\""),
                Ok(Err(why)) | Err(why) => log!("mgmt cmd=get status=error reason=\"{why}\""),
            },
            Changes(id) => match self.capsules.get(id) {
                None => log!("mgmt cmd=changes status=error reason=\"no capsule {id}\""),
                Some(c) => {
                    let held = c.held();
                    for (name, data, _) in &held.written {
                        log!("mgmt item=change id={id} name={name} size={}", data.len());
                    }
                    for (name, _) in &held.deleted {
                        log!("mgmt item=change id={id} name={name} deleted=yes");
                    }
                    log!("mgmt cmd=changes status=ok count={}", held.written.len() + held.deleted.len());
                }
            },
            Export { id, name } => match self.capsules.get(id) {
                None => log!("mgmt cmd=export status=error reason=\"no capsule {id}\""),
                Some(c) => match c.held().written.iter().find(|(n, ..)| n.eq_ignore_ascii_case(&name)) {
                    Some((name, data, _)) => {
                        log!(
                            "mgmt cmd=export status=ok id={id} name={name} size={} hex={}",
                            data.len(),
                            mgmt::to_hex(data)
                        )
                    }
                    None => log!("mgmt cmd=export status=error reason=\"capsule {id} holds no such file\""),
                },
            },
            Discard(id) => match self.capsules.get_mut(id) {
                None => log!("mgmt cmd=discard status=error reason=\"no capsule {id}\""),
                Some(c) => {
                    c.discard();
                    log!("mgmt cmd=discard status=ok id={id}");
                }
            },
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
