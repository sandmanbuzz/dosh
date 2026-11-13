//! A polled virtio block driver, using the legacy (I/O port) PCI interface.
//!
//! Legacy rather than modern because its registers live in an I/O BAR, which
//! firmware places in the first 64 KiB of port space; a modern device's
//! registers live in a memory BAR that may sit anywhere in a 64-bit address
//! space, beyond what the supervisor maps. QEMU provides it with
//! `disable-modern=on`. The supervisor owns the machine after boot services
//! end, so one request at a time, completed by polling, is all it needs.

use core::sync::atomic::{fence, Ordering};

use dosh_fat::{Block, Error, SECTOR};

use super::cpu::{inb, inl, inw, outb, outl, outw};
use super::pci::Function;
use super::Pages;

pub const VENDOR: u16 = 0x1AF4;
/// The legacy ("transitional") block device's PCI id.
pub const DEVICE_BLOCK: u16 = 0x1001;

const REG_GUEST_FEATURES: u16 = 4;
const REG_QUEUE_PFN: u16 = 8;
const REG_QUEUE_SIZE: u16 = 12;
const REG_QUEUE_SELECT: u16 = 14;
const REG_QUEUE_NOTIFY: u16 = 16;
const REG_STATUS: u16 = 18;
const REG_CONFIG: u16 = 20;

const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FAILED: u8 = 0x80;

const DESC_NEXT: u16 = 1;
const DESC_WRITE: u16 = 2;
const TYPE_IN: u32 = 0;
const TYPE_OUT: u32 = 1;
/// Most sectors moved by one request; bounds the bounce buffer.
const MAX_SECTORS: usize = 128;

#[repr(C)]
#[derive(Clone, Copy)]
struct Desc {
    addr: u64,
    len: u32,
    flags: u16,
    next: u16,
}

pub struct VirtioBlk {
    io: u16,
    queue: Pages,
    queue_size: u16,
    used_offset: usize,
    /// The request header, a bounce buffer for data, and the status byte.
    scratch: Pages,
    last_used: u16,
    sectors: u64,
}

fn align_up(n: usize) -> usize {
    n.next_multiple_of(4096)
}

impl VirtioBlk {
    /// Bring up the device at `function`, or say why not.
    pub fn open(function: Function) -> Result<Self, &'static str> {
        let io = function.io_bar(0).ok_or("no I/O BAR")?;
        function.enable_io_dma();
        outb(io + REG_STATUS, 0); // reset: firmware may have left it running
        outb(io + REG_STATUS, STATUS_ACKNOWLEDGE);
        outb(io + REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER);
        outl(io + REG_GUEST_FEATURES, 0); // plain reads and writes need none

        outw(io + REG_QUEUE_SELECT, 0);
        let queue_size = inw(io + REG_QUEUE_SIZE);
        if queue_size == 0 || !queue_size.is_power_of_two() {
            outb(io + REG_STATUS, STATUS_FAILED);
            return Err("no request queue");
        }
        let n = queue_size as usize;
        let used_offset = align_up(16 * n + 6 + 2 * n);
        let mut queue = Pages::zeroed(used_offset + align_up(6 + 8 * n));
        let _ = queue.bytes();
        outl(io + REG_QUEUE_PFN, (queue.addr() / 4096) as u32);
        outb(io + REG_STATUS, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_DRIVER_OK);

        let sectors = inl(io + REG_CONFIG) as u64 | (inl(io + REG_CONFIG + 4) as u64) << 32;
        Ok(Self {
            io,
            queue,
            queue_size,
            used_offset,
            scratch: Pages::zeroed(4096 + MAX_SECTORS * SECTOR),
            last_used: 0,
            sectors,
        })
    }

    fn request(&mut self, kind: u32, sector: u64, count: usize) -> Result<(), Error> {
        let scratch = self.scratch.addr();
        let (header, data, status) = (scratch, scratch + 4096, scratch + 64);
        // SAFETY: `scratch` and `queue` are ours, identity-mapped, and big
        // enough for the offsets used; the device reads and writes them only
        // between the notify and the used-ring update we wait for.
        unsafe {
            let h = scratch as *mut u8;
            h.cast::<u32>().write_volatile(kind);
            h.add(4).cast::<u32>().write_volatile(0);
            h.add(8).cast::<u64>().write_volatile(sector);
            (status as *mut u8).write_volatile(0xFF);

            let desc = self.queue.addr() as *mut Desc;
            let bytes = (count * SECTOR) as u32;
            desc.write_volatile(Desc { addr: header, len: 16, flags: DESC_NEXT, next: 1 });
            let data_flags = DESC_NEXT | if kind == TYPE_IN { DESC_WRITE } else { 0 };
            desc.add(1).write_volatile(Desc { addr: data, len: bytes, flags: data_flags, next: 2 });
            desc.add(2).write_volatile(Desc { addr: status, len: 1, flags: DESC_WRITE, next: 0 });

            let avail = (self.queue.addr() + 16 * self.queue_size as u64) as *mut u16;
            let idx = avail.add(1).read_volatile();
            avail.add(2 + (idx % self.queue_size) as usize).write_volatile(0);
            fence(Ordering::SeqCst);
            avail.add(1).write_volatile(idx.wrapping_add(1));
            fence(Ordering::SeqCst);
        }
        outw(self.io + REG_QUEUE_NOTIFY, 0);

        let used = (self.queue.addr() + self.used_offset as u64) as *const u16;
        let mut spins = 0u64;
        // SAFETY: the used ring's index is at offset 2 of the used area.
        while unsafe { used.add(1).read_volatile() } == self.last_used {
            spins += 1;
            if spins > 200_000_000 {
                return Err(Error::Io);
            }
            core::hint::spin_loop();
        }
        fence(Ordering::SeqCst);
        self.last_used = self.last_used.wrapping_add(1);
        let _ = inb(self.io + 19); // reading the ISR status acknowledges the device
                                   // SAFETY: as above.
        match unsafe { (status as *const u8).read_volatile() } {
            0 => Ok(()),
            _ => Err(Error::Io),
        }
    }
}

impl Block for VirtioBlk {
    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), Error> {
        for (n, chunk) in buf.chunks_mut(MAX_SECTORS * SECTOR).enumerate() {
            let count = chunk.len() / SECTOR;
            self.request(TYPE_IN, sector + (n * MAX_SECTORS) as u64, count)?;
            chunk.copy_from_slice(&self.scratch.bytes()[4096..4096 + chunk.len()]);
        }
        Ok(())
    }

    fn write(&mut self, sector: u64, buf: &[u8]) -> Result<(), Error> {
        for (n, chunk) in buf.chunks(MAX_SECTORS * SECTOR).enumerate() {
            self.scratch.bytes()[4096..4096 + chunk.len()].copy_from_slice(chunk);
            self.request(TYPE_OUT, sector + (n * MAX_SECTORS) as u64, chunk.len() / SECTOR)?;
        }
        Ok(())
    }

    fn sectors(&self) -> u64 {
        self.sectors
    }
}
