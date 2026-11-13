//! Presentation: carrying a capsule's console device to somewhere a person
//! or program can reach it.
//!
//! The only transport so far is a serial port. It carries the raw byte stream
//! until the capsule's program takes over the display, and from then on an
//! ANSI rendering of the screen.
//! COM1 belongs to the supervisor log; capsule N is offered the next port
//! along, if the machine has one. A capsule with no endpoint still runs: its
//! output has nowhere to go and it never receives input.
//!
//! With `console mux` in the manifest every capsule is presented on COM2
//! instead, one at a time. Ctrl-] followed by a capsule's number moves the
//! line to that capsule (Ctrl-] Ctrl-] sends a Ctrl-]). The capsule that
//! was shown goes on running; its text output is held (the most recent
//! few KiB) and a screen is redrawn in full when it is shown again.
//!
//! A serial line has no notion of a session, and firmware is fond of
//! printing on every port it can find before DOSH starts. So attaching an
//! endpoint announces itself with one line, `--- DOSH capsule N ---`.
//! Everything after that line, in both directions, belongs to the capsule
//! and passes through untouched.

use alloc::collections::VecDeque;
use core::fmt::Write;
use core::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use dosh_dos::video::{Snapshot, COLS, ROWS};
use dosh_term::Terminal;

use crate::device::Console;
use crate::platform::uart::Uart;

const CAPSULE_PORTS: [Uart; 3] = [Uart::COM2, Uart::COM3, Uart::COM4];
/// Input a capsule has not read yet is left in the transport beyond this,
/// so a peer that floods a console exhausts its own line, not our memory.
const RX_BACKLOG: usize = 4096;

/// The capsule shown on the shared line, and how many share it.
static SHOWN: AtomicUsize = AtomicUsize::new(0);
/// Counts moves of the line, so a capsule that was left and shown again
/// between two of its looks still knows the far end was changed under it.
static EPOCH: AtomicUsize = AtomicUsize::new(0);
static SHARING: AtomicUsize = AtomicUsize::new(0);
/// A Ctrl-] has arrived and the next byte says what it meant.
static ESCAPED: AtomicBool = AtomicBool::new(false);
const ESCAPE: u8 = 0x1D;
/// Text output kept for a capsule while it is not shown.
const HELD_MAX: usize = 4096;

pub struct Endpoint {
    id: usize,
    /// Whether the line is shared by every capsule.
    shared: bool,
    /// Whether this capsule is the one the far end shows now.
    attached: bool,
    /// The `EPOCH` this capsule last looked at.
    seen: usize,
    held: VecDeque<u8>,
    uart: Option<Uart>,
    /// What the far end shows, once the capsule has a screen.
    terminal: Terminal<{ COLS * ROWS }>,
}

impl Endpoint {
    pub fn for_capsule(id: usize, shared: bool) -> Self {
        let port = if shared { 0 } else { id };
        // A shared line is set up once, by the first capsule: setting it up again
        // would reset it under the banner already queued.
        let mut uart = CAPSULE_PORTS.get(port).copied().filter(|uart| (shared && id > 0) || uart.init());
        if shared {
            SHARING.fetch_add(1, Ordering::Relaxed);
        }
        let attached = !shared || id == 0;
        if let Some(uart) = uart.as_mut().filter(|_| attached) {
            while uart.recv().is_some() {} // typed before the session began
            let _ = write!(uart, "\r\n--- DOSH capsule {id} ---\r\n");
        }
        Self { id, shared, attached, seen: 0, held: VecDeque::new(), uart, terminal: Terminal::new() }
    }

    /// Follow the shared line: note when this capsule is shown or not, and
    /// bring the far end up to date when it becomes the one shown.
    fn follow(&mut self) {
        if !self.shared {
            return;
        }
        let shown = SHOWN.load(Ordering::Relaxed) == self.id;
        let epoch = EPOCH.load(Ordering::Relaxed);
        if shown && (!self.attached || self.seen != epoch) {
            if let Some(mut uart) = self.uart {
                let _ = write!(uart, "\r\n--- DOSH capsule {} ---\r\n", self.id);
                self.held.drain(..).for_each(|byte| uart.send(byte));
            }
            self.terminal = Terminal::new(); // the far end must be redrawn from scratch
        }
        self.attached = shown;
        self.seen = epoch;
    }

    pub fn name(&self) -> &'static str {
        match self.uart {
            Some(Uart::COM2) => "com2",
            Some(Uart::COM3) => "com3",
            Some(Uart::COM4) => "com4",
            _ => "none",
        }
    }

    /// Show the capsule's display: the far end is an ANSI terminal, and only
    /// what changed since the last call is sent.
    pub fn show(&mut self, screen: &Snapshot) {
        self.follow();
        if let Some(uart) = self.uart.filter(|_| self.attached) {
            let cursor = (screen.row, screen.col);
            self.terminal.draw(&screen.cells, COLS, cursor, screen.cursor_visible, &mut |byte| uart.send(byte));
        }
    }

    /// Move pending bytes in both directions.
    pub fn pump(&mut self, console: &mut Console) {
        self.follow();
        match self.uart {
            Some(_) if !self.attached => {
                self.held.extend(console.tx.drain(..));
                self.held.drain(..self.held.len().saturating_sub(HELD_MAX));
            }
            Some(uart) => {
                console.tx.drain(..).for_each(|byte| uart.send(byte));
                while console.rx.len() < RX_BACKLOG {
                    let Some(byte) = uart.recv() else { break };
                    if self.shared && self.switch(byte, console) {
                        break;
                    }
                    if !self.shared {
                        console.rx.push_back(byte);
                    }
                }
            }
            None => console.tx.clear(),
        }
    }
}

impl Endpoint {
    /// Interpret a byte from the shared line. Returns whether it moved the
    /// line to another capsule, after which the rest belongs to that one.
    fn switch(&mut self, byte: u8, console: &mut Console) -> bool {
        if ESCAPED.swap(false, Ordering::Relaxed) {
            match byte {
                b'0'..=b'9' if ((byte - b'0') as usize) < SHARING.load(Ordering::Relaxed) => {
                    SHOWN.store((byte - b'0') as usize, Ordering::Relaxed);
                    EPOCH.fetch_add(1, Ordering::Relaxed);
                    return true;
                }
                b'0'..=b'9' => {}
                ESCAPE => console.rx.push_back(ESCAPE),
                other => console.rx.extend([ESCAPE, other]),
            }
        } else if byte == ESCAPE {
            ESCAPED.store(true, Ordering::Relaxed);
        } else {
            console.rx.push_back(byte);
        }
        false
    }
}
