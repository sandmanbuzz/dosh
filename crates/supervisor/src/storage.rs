//! The persistent disk: a FAT volume, labelled [`LABEL`], on a virtio block
//! device. Drive C: of every capsule is stocked from it at launch, and what
//! a program leaves behind is written back when it exits.
//!
//! Whatever the firmware reads at boot (the ESP's `\DOSH\C\`) is the base
//! drive; the disk's files lay over it. The disk is the supervisor's alone.

use alloc::string::String;
use alloc::vec::Vec;

use dosh_fat::{Error, Volume};

use crate::capsule::Changes;
use crate::log;
use crate::platform::pci;
use crate::platform::virtio::{self, VirtioBlk};

pub const LABEL: &str = "DOSHDATA";

pub struct Disk {
    volume: Volume<VirtioBlk>,
}

impl Disk {
    /// Find the persistent disk among the machine's block devices.
    pub fn find() -> Option<Self> {
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
                Ok(volume) if volume.label() == LABEL => {
                    log!("storage event=attach label={LABEL}");
                    return Some(Self { volume });
                }
                Ok(volume) => log!("storage event=skip label=\"{}\" reason=\"not {LABEL}\"", volume.label()),
                Err(e) => log!("storage event=skip reason=\"{}\"", describe(&e)),
            }
        }
        None
    }

    /// Every file on the disk.
    pub fn files(&mut self) -> Vec<(String, Vec<u8>)> {
        match self.volume.read_all() {
            Ok(files) => files,
            Err(e) => {
                log!("storage event=error op=read reason=\"{}\"", describe(&e));
                Vec::new()
            }
        }
    }

    /// Store one file, by operator's hand rather than a program's.
    pub fn put(&mut self, name: &str, data: &[u8]) -> Result<(), String> {
        self.volume.write(name, data).map_err(|e| describe(&e))
    }

    pub fn get(&mut self, name: &str) -> Result<Option<Vec<u8>>, String> {
        self.volume.read(name).map_err(|e| describe(&e))
    }

    /// Make the disk say what a finished program's drive C: says. A file the
    /// disk cannot take (a name it cannot hold, no room) is reported and
    /// skipped; the others still land.
    pub fn store(&mut self, capsule: usize, changes: &Changes) {
        for (name, data) in &changes.written {
            match self.volume.write(name, data) {
                Ok(()) => log!("storage event=write capsule={capsule} name={name} size={}", data.len()),
                Err(e) => {
                    log!("storage event=error op=write capsule={capsule} name={name} reason=\"{}\"", describe(&e))
                }
            }
        }
        for name in &changes.deleted {
            match self.volume.delete(name) {
                Ok(true) => log!("storage event=delete capsule={capsule} name={name}"),
                Ok(false) => {} // it was never on the disk: part of the base drive
                Err(e) => {
                    log!("storage event=error op=delete capsule={capsule} name={name} reason=\"{}\"", describe(&e))
                }
            }
        }
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
