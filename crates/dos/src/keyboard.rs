//! The keyboard: turning the terminal's byte stream into BIOS keystrokes.
//!
//! A key is a word, scan code in the high byte and ASCII in the low byte, as
//! `INT 16h` returns it. Keys with no ASCII value (arrows, function keys)
//! have 0 there. A terminal sends those as escape sequences, which this
//! decodes. A terminal says nothing about modifier keys or key releases, so
//! none are reported.

use alloc::collections::VecDeque;

use crate::CharDevice;

#[derive(Default, PartialEq, Eq)]
enum State {
    #[default]
    Idle,
    Esc,
    /// Inside `ESC [` or `ESC O`: the bytes so far.
    Seq(bool, [u8; 6], u8),
}

#[derive(Default)]
pub(crate) struct Keyboard {
    keys: VecDeque<u16>,
    state: State,
    after_cr: bool,
}

/// Scan code of the key that produces `b` on a US keyboard.
fn scan_of(b: u8) -> u8 {
    const ROW: [(&[u8], u8); 3] = [(b"qwertyuiop", 0x10), (b"asdfghjkl", 0x1E), (b"zxcvbnm", 0x2C)];
    let lower = b.to_ascii_lowercase();
    for (keys, first) in ROW {
        if let Some(i) = keys.iter().position(|&k| k == lower) {
            return first + i as u8;
        }
    }
    match b {
        b'1'..=b'9' => 2 + b - b'1',
        b'0' => 0x0B,
        b'!' => 2,
        b'@' => 3,
        b'#' => 4,
        b'$' => 5,
        b'%' => 6,
        b'^' => 7,
        b'&' => 8,
        b'*' => 9,
        b'(' => 0x0A,
        b')' => 0x0B,
        b'-' | b'_' => 0x0C,
        b'=' | b'+' => 0x0D,
        b'[' | b'{' => 0x1A,
        b']' | b'}' => 0x1B,
        b';' | b':' => 0x27,
        b'\'' | b'"' => 0x28,
        b'`' | b'~' => 0x29,
        b'\\' | b'|' => 0x2B,
        b',' | b'<' => 0x33,
        b'.' | b'>' => 0x34,
        b'/' | b'?' => 0x35,
        b' ' => 0x39,
        _ => 0,
    }
}

fn ascii_key(b: u8) -> u16 {
    let (scan, ascii) = match b {
        0x1B => (0x01, 0x1B),
        0x08 | 0x7F => (0x0E, 0x08),
        b'\t' => (0x0F, b'\t'),
        b'\r' => (0x1C, b'\r'),
        b'\n' => (0x1C, b'\n'),
        1..=26 => (scan_of(b + 0x60), b), // Ctrl-A to Ctrl-Z
        _ => (scan_of(b), b),
    };
    (scan as u16) << 8 | ascii as u16
}

/// The key for an `ESC [` or `ESC O` sequence: parameters then a final byte.
fn sequence_key(ss3: bool, bytes: &[u8]) -> Option<u16> {
    let (&last, params) = bytes.split_last()?;
    let number = params.iter().try_fold(0u16, |n, &b| b.is_ascii_digit().then(|| n * 10 + (b - b'0') as u16));
    let scan = match (ss3, last) {
        // Linux console: ESC [ [ A..E
        (false, b'A'..=b'E') if params == b"[" => 0x3B + (last - b'A'),
        (_, b'A') => 0x48,
        (_, b'B') => 0x50,
        (_, b'C') => 0x4D,
        (_, b'D') => 0x4B,
        (_, b'H') => 0x47,
        (_, b'F') => 0x4F,
        (true, b'P'..=b'S') => 0x3B + (last - b'P'),
        (false, b'~') => match number? {
            1 | 7 => 0x47,
            2 => 0x52,
            3 => 0x53,
            4 | 8 => 0x4F,
            5 => 0x49,
            6 => 0x51,
            n @ 11..=15 => 0x3B + (n - 11) as u8,
            n @ 17..=21 => 0x40 + (n - 17) as u8,
            23 => 0x85,
            24 => 0x86,
            _ => return None,
        },
        _ => return None,
    };
    Some((scan as u16) << 8)
}

impl Keyboard {
    /// Decode everything the console has to offer into keys.
    pub(crate) fn fill(&mut self, con: &mut dyn CharDevice) {
        while let Some(b) = con.read() {
            self.feed(b);
        }
        // A lone ESC with nothing behind it is the Esc key: a terminal sends
        // the rest of a sequence in the same burst.
        if self.state == State::Esc {
            self.state = State::Idle;
            self.keys.push_back(ascii_key(0x1B));
        }
    }

    fn feed(&mut self, b: u8) {
        match core::mem::take(&mut self.state) {
            State::Idle => {
                let after_cr = core::mem::replace(&mut self.after_cr, b == b'\r');
                match b {
                    0x1B => self.state = State::Esc,
                    // A peer that ends lines with CR LF presses Enter once.
                    b'\n' if after_cr => {}
                    _ => self.keys.push_back(ascii_key(b)),
                }
            }
            State::Esc => match b {
                b'[' => self.state = State::Seq(false, [0; 6], 0),
                b'O' => self.state = State::Seq(true, [0; 6], 0),
                _ => {
                    self.keys.push_back(ascii_key(0x1B));
                    self.feed(b);
                }
            },
            State::Seq(ss3, mut buf, n) => {
                buf[n as usize] = b;
                let n = n + 1;
                if (0x40..=0x7E).contains(&b) && !(b == b'[' && n == 1) {
                    self.keys.extend(sequence_key(ss3, &buf[..n as usize]));
                } else if (n as usize) < buf.len() {
                    self.state = State::Seq(ss3, buf, n);
                } // an over-long sequence is dropped
            }
        }
    }

    pub(crate) fn peek(&mut self, con: &mut dyn CharDevice) -> Option<u16> {
        self.fill(con);
        self.keys.front().copied()
    }

    pub(crate) fn pop(&mut self, con: &mut dyn CharDevice) -> Option<u16> {
        self.fill(con);
        self.keys.pop_front()
    }
}
