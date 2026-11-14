//! The persistent disk: a FAT volume, labelled [`LABEL`], on a virtio block
//! device. Drive C: of every capsule is stocked from it at launch, and what
//! a program leaves behind is written back when it exits.
//!
//! Whatever the firmware reads at boot (the ESP's `\DOSH\C\`) is the base
//! drive; the disk's files lay over it. The disk is the supervisor's alone.

use alloc::rc::Rc;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use dosh_fat::{Error, Volume};

use crate::capsule::Changes;
use crate::log;
use crate::platform::pci;
use crate::platform::virtio::{self, VirtioBlk};

/// Files by name, their contents shared by everything stocked from them.
pub type Files = Vec<(String, Rc<[u8]>)>;

/// A disk labelled `DOSH` followed by a name is the persistent volume of
/// that name: `DOSHDATA` is `DATA`, the one capsules get unless told otherwise.
const PREFIX: &str = "DOSH";
pub const DEFAULT: &str = "DATA";

/// The disk's own bookkeeping: base-drive files a program deleted, one name
/// per line. Without it a deleted base file would reappear at the next
/// launch. Capsules never see it and cannot write it.
const WHITEOUTS: &str = "WHITEOUT.DSH";

pub struct Disk {
    name: String,
    volume: Volume<VirtioBlk>,
    whiteouts: Vec<String>,
    /// Whether `whiteouts` has changes not yet on the disk; retried until saved.
    dirty: bool,
}

impl Disk {
    /// Every persistent volume among the machine's block devices. Disks
    /// that are not ours, or whose name is taken, are left alone.
    pub fn find_all() -> Vec<Self> {
        let mut found: Vec<Self> = Vec::new();
        for function in pci::scan(8) {
            if function.ids() != (virtio::VENDOR, virtio::DEVICE_BLOCK) {
                continue;
            }
            let dev = match VirtioBlk::open(function) {
                Ok(dev) => dev,
                Err(why) => {
                    log!(
                        "storage event=skip device={:02x}:{:02x}.{} reason=\"{why}\"",
                        function.bus,
                        function.device,
                        function.function
                    );
                    continue;
                }
            };
            match Volume::open(dev) {
                Ok(volume) => {
                    let label = volume.label().to_string();
                    let Some(name) = label.strip_prefix(PREFIX).filter(|n| !n.is_empty() && *n != "NONE") else {
                        log!("storage event=skip label=\"{label}\" reason=\"not a {PREFIX} volume\"");
                        continue;
                    };
                    if !name.bytes().all(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || b == b'_') {
                        log!("storage event=skip label=\"{label}\" reason=\"volume names are A-Z, 0-9 and _\"");
                        continue;
                    }
                    if found.iter().any(|d| d.name == name) {
                        log!("storage event=skip label=\"{label}\" reason=\"volume {name} is already attached\"");
                        continue;
                    }
                    log!("storage event=attach label={label} volume={name}");
                    let mut disk = Self { name: name.to_string(), volume, whiteouts: Vec::new(), dirty: false };
                    disk.whiteouts = match disk.volume.read(WHITEOUTS) {
                        Ok(Some(list)) => String::from_utf8_lossy(&list).lines().map(String::from).collect(),
                        _ => Vec::new(),
                    };
                    found.push(disk);
                }
                Err(e) => log!("storage event=skip reason=\"{}\"", describe(&e)),
            }
        }
        found
    }

    pub fn name(&self) -> &str {
        &self.name
    }

    /// Every file on the disk, or none at all if it cannot be read.
    pub fn files(&mut self) -> Option<Files> {
        match self.volume.read_all() {
            Ok(files) => Some(
                files
                    .into_iter()
                    .filter(|(name, _)| name != WHITEOUTS)
                    .map(|(name, data)| (name, Rc::from(data)))
                    .collect(),
            ),
            Err(e) => {
                log!("storage event=error op=read reason=\"{}\"", describe(&e));
                None
            }
        }
    }

    /// Base-drive files a program deleted: they stay gone.
    pub fn is_whiteout(&self, name: &str) -> bool {
        self.whiteouts.iter().any(|w| w.eq_ignore_ascii_case(name))
    }

    /// Bytes still free on the disk.
    pub fn free(&mut self) -> u64 {
        self.volume.free_bytes().unwrap_or(0)
    }

    /// Store one file, by operator's hand rather than a program's.
    pub fn put(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        if name.eq_ignore_ascii_case(WHITEOUTS) {
            return Err("name is reserved".into());
        }
        self.volume.write(name, data).map_err(|e| describe(&e))?;
        self.set_whiteout(name, false);
        self.save_whiteouts().map_err(|e| describe(&e))
    }

    pub fn get(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        if name.eq_ignore_ascii_case(WHITEOUTS) {
            return Ok(None);
        }
        self.volume.read(name).map_err(|e| describe(&e))
    }

    /// Make the disk say what a finished program's drive C: says. A change
    /// is refused when the file is no longer what the program started from
    /// (another capsule got there first), when the disk cannot take it (a
    /// name it cannot hold, no room), or when it names the disk's own
    /// bookkeeping; the others still land.
    /// Returns what it refused, for the operator to collect.
    pub fn store(&mut self, capsule: usize, changes: &Changes, base: &Files) -> Changes {
        let mut refused = Changes::default();
        let error = |op: &str, name: &str, why: &str| {
            log!("storage event=error op={op} capsule={capsule} name={name} reason=\"{why}\"")
        };
        for (name, data, was) in &changes.written {
            if name == WHITEOUTS {
                error("write", name, "name is reserved");
                refused.written.push((name.clone(), data.clone(), was.clone()));
                continue;
            }
            match self.current(name, base) {
                Ok(now) if now.as_deref() == was.as_deref() => {}
                Ok(_) => {
                    error("write", name, "changed by someone else since launch");
                    refused.written.push((name.clone(), data.clone(), was.clone()));
                    continue;
                }
                Err(e) => {
                    error("write", name, &describe(&e));
                    refused.written.push((name.clone(), data.clone(), was.clone()));
                    continue;
                }
            }
            match self.volume.write(name, data) {
                Ok(()) => {
                    self.set_whiteout(name, false);
                    log!("storage event=write capsule={capsule} name={name} size={}", data.len())
                }
                Err(e) => {
                    error("write", name, &describe(&e));
                    refused.written.push((name.clone(), data.clone(), was.clone()));
                }
            }
        }
        for (name, was) in &changes.deleted {
            match self.current(name, base) {
                Ok(None) => continue, // already gone
                Ok(Some(now)) if now == *was => {}
                Ok(Some(_)) => {
                    error("delete", name, "changed by someone else since launch");
                    refused.deleted.push((name.clone(), was.clone()));
                    continue;
                }
                Err(e) => {
                    error("delete", name, &describe(&e));
                    refused.deleted.push((name.clone(), was.clone()));
                    continue;
                }
            }
            match self.volume.delete(name) {
                Ok(on_disk) => {
                    if base.iter().any(|(n, _)| n.eq_ignore_ascii_case(name)) {
                        self.set_whiteout(name, true);
                    }
                    if on_disk || self.is_whiteout(name) {
                        log!("storage event=delete capsule={capsule} name={name}");
                    }
                }
                Err(e) => {
                    error("delete", name, &describe(&e));
                    refused.deleted.push((name.clone(), was.clone()));
                }
            }
        }
        if let Err(e) = self.save_whiteouts() {
            error("write", WHITEOUTS, &describe(&e));
        }
        refused
    }

    /// What a capsule launched now would find under `name`: the disk's
    /// file, else the base drive's unless a program deleted it.
    fn current(&mut self, name: &str, base: &Files) -> Result<Option<Rc<[u8]>>, Error> {
        Ok(match self.volume.read(name)? {
            Some(data) => Some(Rc::from(data)),
            None if self.is_whiteout(name) => None,
            None => base.iter().find(|(n, _)| n.eq_ignore_ascii_case(name)).map(|(_, d)| d.clone()),
        })
    }

    fn set_whiteout(&mut self, name: &str, on: bool) {
        let before = self.whiteouts.len();
        self.whiteouts.retain(|w| !w.eq_ignore_ascii_case(name));
        self.dirty |= self.whiteouts.len() != before;
        if on {
            self.whiteouts.push(name.to_ascii_uppercase());
            self.dirty = true;
        }
    }

    /// Write the whiteout list, or remove its file once it is empty.
    fn save_whiteouts(&mut self) -> Result<(), Error> {
        if !self.dirty {
            return Ok(());
        }
        if self.whiteouts.is_empty() {
            self.volume.delete(WHITEOUTS)?;
        } else {
            self.volume.write(WHITEOUTS, self.whiteouts.join("\n").as_bytes())?;
        }
        self.dirty = false;
        Ok(())
    }
}

fn describe(e: &Error) -> String {
    match e {
        Error::Io => "device error".into(),
        Error::Format(why) => alloc::format!("not usable: {why}"),
        Error::Full => "no room".into(),
        Error::Name => "name does not fit 8.3".into(),
        Error::Corrupt => "damaged allocation table".into(),
    }
}
