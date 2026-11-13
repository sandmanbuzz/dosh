//! 16550 serial ports, polled.

use super::cpu::{inb, outb};

#[derive(Clone, Copy, PartialEq, Eq)]
pub struct Uart(u16);

impl Uart {
    pub const COM1: Uart = Uart(0x3F8);
    pub const COM2: Uart = Uart(0x2F8);
    pub const COM3: Uart = Uart(0x3E8);
    pub const COM4: Uart = Uart(0x2E8);

    /// Program the port for 115200 8N1 with FIFOs. Returns whether a UART
    /// actually answers at this address.
    pub fn init(self) -> bool {
        outb(self.0 + 1, 0x00); // no interrupts
        outb(self.0 + 3, 0x80); // divisor latch
        outb(self.0, 0x01);
        outb(self.0 + 1, 0x00);
        outb(self.0 + 3, 0x03); // 8N1
        outb(self.0 + 2, 0xC7); // FIFOs on and cleared
        outb(self.0 + 4, 0x03); // DTR, RTS
        outb(self.0 + 7, 0xA5);
        let a = inb(self.0 + 7);
        outb(self.0 + 7, 0x5A);
        a == 0xA5 && inb(self.0 + 7) == 0x5A
    }

    pub fn send(self, byte: u8) {
        while inb(self.0 + 5) & 0x20 == 0 {
            core::hint::spin_loop();
        }
        outb(self.0, byte);
    }

    pub fn recv(self) -> Option<u8> {
        (inb(self.0 + 5) & 0x01 != 0).then(|| inb(self.0))
    }
}

impl core::fmt::Write for Uart {
    fn write_str(&mut self, s: &str) -> core::fmt::Result {
        s.bytes().for_each(|b| self.send(b));
        Ok(())
    }
}
