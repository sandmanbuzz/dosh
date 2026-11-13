//! The management channel: the operator's way to talk to a supervisor that
//! stays up. It rides on COM1, the same line the supervisor logs to, so the
//! replies are ordinary log lines and a single `socat` is enough to both
//! command and watch the machine.
//!
//! Commands are lines of text; each is answered with one `mgmt` event
//! carrying `cmd=` and `status=ok|error`, preceded by `mgmt item=...` lines
//! for commands that list things:
//!
//! ```text
//! list                         every capsule: id, program, status
//! launch [limit=SEC] PROGRAM [ARGS]
//! suspend ID | resume ID | kill ID
//! files                        the persistent disk's files
//! put NAME HEX                 store a file on the persistent disk
//! get NAME                     read one back, as hex
//! shutdown                     power off
//! ```
//!
//! A line that does not parse is answered with an error and changes nothing.

use alloc::string::{String, ToString};
use alloc::vec::Vec;

use crate::platform::uart::Uart;

/// The longest line accepted: a 256 KiB file in hex, and its command.
const MAX_LINE: usize = 600 << 10;

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    List,
    Launch { limit: Option<u64>, program: String, tail: Vec<u8> },
    Suspend(usize),
    Resume(usize),
    Kill(usize),
    Files,
    Put { name: String, data: Vec<u8> },
    Get(String),
    Shutdown,
}

pub struct Channel {
    uart: Uart,
    line: Vec<u8>,
    /// The line being read overflowed: drop it up to the next newline.
    discarding: bool,
}

pub enum Input {
    Command(Command),
    Invalid(String),
}

impl Channel {
    pub fn open() -> Self {
        let uart = Uart::COM1;
        while uart.recv().is_some() {} // whatever firmware or a prior session left
        Self { uart, line: Vec::new(), discarding: false }
    }

    /// The next complete command waiting on the line, if any.
    pub fn poll(&mut self) -> Option<Input> {
        while let Some(byte) = self.uart.recv() {
            match byte {
                b'\n' => {
                    let line = core::mem::take(&mut self.line);
                    if core::mem::take(&mut self.discarding) {
                        return Some(Input::Invalid("line too long".into()));
                    }
                    let text = String::from_utf8_lossy(&line).trim().to_string();
                    if !text.is_empty() {
                        return Some(match parse(&text) {
                            Ok(command) => Input::Command(command),
                            Err(why) => Input::Invalid(why),
                        });
                    }
                }
                b'\r' => {}
                _ if self.discarding => {}
                _ if self.line.len() >= MAX_LINE => {
                    self.line.clear();
                    self.discarding = true;
                }
                _ => self.line.push(byte),
            }
        }
        None
    }
}

fn parse_id(word: Option<&str>) -> Result<usize, String> {
    word.ok_or("missing capsule id")?.parse().map_err(|_| "capsule id is not a number".to_string())
}

pub fn parse(line: &str) -> Result<Command, String> {
    let (verb, rest) = line.split_once(' ').unwrap_or((line, ""));
    let rest = rest.trim();
    let no_args =
        |command| if rest.is_empty() { Ok(command) } else { Err(alloc::format!("{verb} takes no arguments")) };
    match verb {
        "list" => no_args(Command::List),
        "files" => no_args(Command::Files),
        "shutdown" => no_args(Command::Shutdown),
        "suspend" => Ok(Command::Suspend(parse_id(Some(rest))?)),
        "resume" => Ok(Command::Resume(parse_id(Some(rest))?)),
        "kill" => Ok(Command::Kill(parse_id(Some(rest))?)),
        "launch" => {
            let mut rest = rest;
            let mut limit = None;
            while let Some(option) = rest.strip_prefix("limit=") {
                let (seconds, after) = option.split_at(option.find(' ').unwrap_or(option.len()));
                limit = Some(seconds.parse().map_err(|_| "bad limit".to_string())?);
                rest = after.trim_start();
            }
            if rest.is_empty() {
                return Err("launch needs a program".into());
            }
            let (program, tail) = rest.split_at(rest.find(' ').unwrap_or(rest.len()));
            Ok(Command::Launch { limit, program: program.to_string(), tail: tail.as_bytes().to_vec() })
        }
        "get" if !rest.is_empty() && !rest.contains(' ') => Ok(Command::Get(rest.to_string())),
        "put" => {
            let (name, hex) = rest.split_once(' ').unwrap_or((rest, ""));
            if name.is_empty() {
                return Err("put needs a name and the contents in hex".into());
            }
            Ok(Command::Put { name: name.to_string(), data: from_hex(hex.trim())? })
        }
        _ => Err(alloc::format!("unknown command: {verb}")),
    }
}

fn from_hex(text: &str) -> Result<Vec<u8>, String> {
    if !text.len().is_multiple_of(2) {
        return Err("hex has an odd number of digits".into());
    }
    let digit = |b: u8| (b as char).to_digit(16).map(|d| d as u8).ok_or_else(|| "not hex".to_string());
    text.as_bytes().chunks(2).map(|pair| Ok(digit(pair[0])? << 4 | digit(pair[1])?)).collect()
}

pub fn to_hex(data: &[u8]) -> String {
    use core::fmt::Write;
    data.iter().fold(String::with_capacity(data.len() * 2), |mut s, b| {
        let _ = write!(s, "{b:02x}");
        s
    })
}
