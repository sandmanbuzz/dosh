//! Virtual devices: what a guest sees, independent of what backs it.

use alloc::collections::VecDeque;

use dosh_dos::CharDevice;

/// A capsule's console device: two byte queues. The guest's DOS services
/// fill `tx` and drain `rx`; presentation does the opposite, when and if an
/// endpoint is attached. Neither side knows about the other.
#[derive(Default)]
pub struct Console {
    pub rx: VecDeque<u8>,
    pub tx: VecDeque<u8>,
}

impl CharDevice for Console {
    fn read(&mut self) -> Option<u8> {
        self.rx.pop_front()
    }

    fn write(&mut self, byte: u8) {
        self.tx.push_back(byte);
    }
}

/// The colour display adapter's status register (port 3DAh). Programs poll
/// it to find the retrace, when the display may be written without snow.
/// The adapter here has no beam, so the register follows a fixed pattern
/// of reads, not time: one vertical retrace in 64 reads, and the
/// horizontal retrace bit alternating every two. A program that waits
/// for either edge finds it.
#[derive(Default)]
pub struct Cga {
    reads: u32,
}

impl Cga {
    pub const STATUS_PORT: u16 = 0x3DA;

    pub fn status(&mut self) -> u8 {
        let phase = self.reads % 64;
        self.reads = self.reads.wrapping_add(1);
        let vertical = phase < 8;
        let horizontal = phase % 4 < 2;
        // Bit 0: display disabled (either retrace). Bit 3: vertical retrace.
        (horizontal || vertical) as u8 | (vertical as u8) << 3
    }
}
