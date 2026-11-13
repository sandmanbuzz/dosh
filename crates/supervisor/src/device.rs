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
