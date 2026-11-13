//! Presentation: carrying a capsule's console device to somewhere a person
//! or program can reach it.
//!
//! The only transport so far is a serial port carrying the raw byte stream.
//! COM1 belongs to the supervisor log; capsule N is offered the next port
//! along, if the machine has one. A capsule with no endpoint still runs: its
//! output has nowhere to go and it never receives input.
//!
//! A serial line has no notion of a session, and firmware is fond of
//! printing on every port it can find before DOSH starts. So attaching an
//! endpoint announces itself with one line, `--- DOSH capsule N ---`.
//! Everything after that line, in both directions, belongs to the capsule
//! and passes through untouched.

use core::fmt::Write;

use crate::device::Console;
use crate::platform::uart::Uart;

const CAPSULE_PORTS: [Uart; 3] = [Uart::COM2, Uart::COM3, Uart::COM4];
/// Input a capsule has not read yet is left in the transport beyond this,
/// so a peer that floods a console exhausts its own line, not our memory.
const RX_BACKLOG: usize = 4096;

pub struct Endpoint {
    uart: Option<Uart>,
}

impl Endpoint {
    pub fn for_capsule(id: usize) -> Self {
        let uart = CAPSULE_PORTS.get(id).copied().filter(|uart| uart.init());
        if let Some(mut uart) = uart {
            while uart.recv().is_some() {} // typed before the session began
            let _ = write!(uart, "\r\n--- DOSH capsule {id} ---\r\n");
        }
        Self { uart }
    }

    pub fn name(&self) -> &'static str {
        match self.uart {
            Some(Uart::COM2) => "com2",
            Some(Uart::COM3) => "com3",
            Some(Uart::COM4) => "com4",
            _ => "none",
        }
    }

    /// Move pending bytes in both directions.
    pub fn pump(&mut self, console: &mut Console) {
        match self.uart {
            Some(uart) => {
                console.tx.drain(..).for_each(|byte| uart.send(byte));
                while console.rx.len() < RX_BACKLOG {
                    let Some(byte) = uart.recv() else { break };
                    console.rx.push_back(byte);
                }
            }
            None => console.tx.clear(),
        }
    }
}
