//! PCI configuration space through the legacy port mechanism, enough to find
//! devices and read where firmware put them.

use alloc::vec::Vec;

use super::cpu::{inl, outl};

const ADDRESS: u16 = 0xCF8;
const DATA: u16 = 0xCFC;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Function {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
}

impl Function {
    fn address(self, offset: u8) -> u32 {
        0x8000_0000
            | (self.bus as u32) << 16
            | (self.device as u32) << 11
            | (self.function as u32) << 8
            | (offset & 0xFC) as u32
    }

    pub fn read32(self, offset: u8) -> u32 {
        outl(ADDRESS, self.address(offset));
        inl(DATA)
    }

    pub fn write32(self, offset: u8, value: u32) {
        outl(ADDRESS, self.address(offset));
        outl(DATA, value);
    }

    pub fn ids(self) -> (u16, u16) {
        let id = self.read32(0);
        (id as u16, (id >> 16) as u16)
    }

    /// Allow the device to use its I/O ports and to master the bus.
    pub fn enable_io_dma(self) {
        let command = self.read32(4);
        self.write32(4, command | 0b101);
    }

    /// The I/O port base in BAR `n`, if that BAR is an I/O BAR.
    pub fn io_bar(self, n: u8) -> Option<u16> {
        let bar = self.read32(0x10 + 4 * n);
        (bar & 1 == 1).then_some((bar & !3) as u16)
    }
}

/// Every function on the first `buses` buses.
pub fn scan(buses: u16) -> Vec<Function> {
    let mut found = Vec::new();
    for bus in 0..buses as u8 {
        for device in 0..32 {
            let first = Function { bus, device, function: 0 };
            if first.ids().0 == 0xFFFF {
                continue;
            }
            let multifunction = first.read32(0x0C) & 0x80_0000 != 0;
            for function in 0..if multifunction { 8 } else { 1 } {
                let f = Function { function, ..first };
                if f.ids().0 != 0xFFFF {
                    found.push(f);
                }
            }
        }
    }
    found
}
