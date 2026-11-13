//! The text display: an 80x25 colour text buffer at B800:0000 and the video
//! BIOS (`INT 10h`) that drives it.
//!
//! The buffer is ordinary guest memory, so programs may write it directly, as
//! most full-screen programs do. The cursor and mode live in the BIOS data
//! area. Presentation reads the same memory; nothing here knows where it ends
//! up.

use crate::firmware::BDA;
use crate::mem::Mem;
use crate::{Fault, Outcome, Regs};

pub const TEXT_SEG: u16 = 0xB800;
pub const COLS: usize = 80;
pub const ROWS: usize = 25;
/// A space, light grey on black.
const BLANK: u16 = 0x0720;
const DEFAULT_ATTR: u8 = 0x07;
const DEFAULT_SHAPE: u16 = 0x0607;

const BDA_CURSOR: u16 = 0x50; // page 0: column, row
const BDA_SHAPE: u16 = 0x60; // end line, start line

/// Cleared, with the cursor at the top left. Called once, at machine boot.
pub(crate) fn init(mem: &mut Mem) {
    clear(mem);
    mem.w16(BDA, BDA_SHAPE, DEFAULT_SHAPE);
    mem.w16(BDA, 0x4C, (COLS * ROWS * 2) as u16); // regen buffer length
    mem.w16(BDA, 0x4E, 0); // active page offset
    mem.w8(BDA, 0x62, 0); // active page
}

fn clear(mem: &mut Mem) {
    for i in 0..COLS * ROWS {
        mem.w16(TEXT_SEG, (i * 2) as u16, BLANK);
    }
    set_cursor(mem, 0, 0);
}

fn at(row: usize, col: usize) -> u16 {
    ((row * COLS + col) * 2) as u16
}

fn cursor(mem: &Mem) -> (usize, usize) {
    let (col, row) = (mem.r8(BDA, BDA_CURSOR) as usize, mem.r8(BDA, BDA_CURSOR + 1) as usize);
    (row.min(ROWS - 1), col.min(COLS - 1))
}

fn set_cursor(mem: &mut Mem, row: usize, col: usize) {
    mem.w8(BDA, BDA_CURSOR, col as u8);
    mem.w8(BDA, BDA_CURSOR + 1, row as u8);
}

/// Scroll the window `top..=bottom`, `left..=right` by `lines` (0 clears it),
/// filling vacated rows with blanks of attribute `attr`.
fn scroll(mem: &mut Mem, window: (usize, usize, usize, usize), lines: usize, attr: u8, up: bool) {
    let (top, left, bottom, right) = (window.0, window.1, window.2.min(ROWS - 1), window.3.min(COLS - 1));
    if top > bottom || left > right {
        return;
    }
    let blank = (attr as u16) << 8 | 0x20;
    let height = bottom - top + 1;
    let lines = if lines == 0 { height } else { lines.min(height) };
    for row in 0..height {
        // Visit rows in the order that never reads one already overwritten.
        let row = if up { row } else { height - 1 - row };
        let dest = top + row;
        let src = if up { row + lines } else { row.wrapping_sub(lines) };
        for col in left..=right {
            let cell = if src < height { mem.r16(TEXT_SEG, at(top + src, col)) } else { blank };
            mem.w16(TEXT_SEG, at(dest, col), cell);
        }
    }
}

fn line_feed(mem: &mut Mem, row: usize, col: usize) {
    if row + 1 == ROWS {
        scroll(mem, (0, 0, ROWS - 1, COLS - 1), 1, DEFAULT_ATTR, true);
        set_cursor(mem, row, col);
    } else {
        set_cursor(mem, row + 1, col);
    }
}

/// Teletype output of one character at the cursor. CR, LF, BS and BEL act;
/// anything else is drawn, keeping the attribute already on screen unless
/// `attr` gives one. Tab is expanded to spaces, as the DOS console does.
pub fn teletype(mem: &mut Mem, byte: u8, attr: Option<u8>) {
    let (row, col) = cursor(mem);
    match byte {
        0x07 => {}
        0x08 => set_cursor(mem, row, col.saturating_sub(1)),
        b'\r' => set_cursor(mem, row, 0),
        b'\n' => line_feed(mem, row, col),
        b'\t' => (0..8 - col % 8).for_each(|_| teletype(mem, b' ', attr)),
        _ => {
            let old = mem.r16(TEXT_SEG, at(row, col));
            let attr = attr.unwrap_or((old >> 8) as u8);
            mem.w16(TEXT_SEG, at(row, col), (attr as u16) << 8 | byte as u16);
            if col + 1 == COLS {
                line_feed(mem, row, 0);
            } else {
                set_cursor(mem, row, col + 1);
            }
        }
    }
}

/// What DOS console output becomes once a program has a screen.
pub fn write_console(mem: &mut Mem, bytes: impl IntoIterator<Item = u8>) {
    bytes.into_iter().for_each(|b| teletype(mem, b, None));
}

/// Everything presentation needs to draw the display.
pub struct Snapshot {
    pub cells: [u16; COLS * ROWS],
    pub row: u8,
    pub col: u8,
    pub cursor_visible: bool,
}

pub fn snapshot(mem: &Mem) -> Snapshot {
    let mut cells = [0; COLS * ROWS];
    for (i, cell) in cells.iter_mut().enumerate() {
        *cell = mem.r16(TEXT_SEG, (i * 2) as u16);
    }
    let (row, col) = cursor(mem);
    // Bit 5 of the cursor start line turns the cursor off.
    let cursor_visible = mem.r8(BDA, BDA_SHAPE + 1) & 0x20 == 0;
    Snapshot { cells, row: row as u8, col: col as u8, cursor_visible }
}

/// Only page 0 exists.
fn page_zero(r: &Regs) -> Result<(), Outcome> {
    if r.bx >> 8 == 0 {
        Ok(())
    } else {
        Err(unimplemented(r))
    }
}

fn unimplemented(r: &Regs) -> Outcome {
    Outcome::Fault(Fault::Unimplemented { vector: 0x10, ah: r.ah(), al: r.al() })
}

/// `INT 10h`, for every function but teletype output (which is console output
/// like any other). The adapter is a colour text display with one page: the
/// mode-set accepts the two 80x25 colour text modes, and the EGA and VGA
/// probes answer as a machine without them does, by leaving the registers
/// alone.
pub(crate) fn int10(r: &mut Regs, mem: &mut Mem) -> Outcome {
    let (ah, al) = (r.ah(), r.al());
    let (ch, cl, dh, dl) = ((r.cx >> 8) as usize, r.cx as u8 as usize, (r.dx >> 8) as usize, r.dx as u8 as usize);
    match ah {
        0x00 => match al & 0x7F {
            2 | 3 => {
                mem.w8(BDA, 0x49, al & 0x7F);
                mem.w16(BDA, 0x4A, COLS as u16);
                mem.w8(BDA, 0x84, ROWS as u8 - 1);
                mem.w16(BDA, BDA_SHAPE, DEFAULT_SHAPE);
                if al & 0x80 == 0 {
                    clear(mem);
                }
            }
            _ => return unimplemented(r),
        },
        0x01 => mem.w16(BDA, BDA_SHAPE, r.cx),
        0x02 => {
            if let Err(fault) = page_zero(r) {
                return fault;
            }
            set_cursor(mem, dh.min(ROWS - 1), dl.min(COLS - 1));
        }
        0x03 => {
            if let Err(fault) = page_zero(r) {
                return fault;
            }
            let (row, col) = cursor(mem);
            r.cx = mem.r16(BDA, BDA_SHAPE);
            r.dx = (row as u16) << 8 | col as u16;
        }
        0x05 if al == 0 => {}
        0x06 | 0x07 => scroll(mem, (ch, cl, dh, dl), al as usize, (r.bx >> 8) as u8, ah == 0x06),
        0x08 => {
            if let Err(fault) = page_zero(r) {
                return fault;
            }
            let (row, col) = cursor(mem);
            r.ax = mem.r16(TEXT_SEG, at(row, col)); // attribute in AH, character in AL
        }
        0x09 | 0x0A => {
            if let Err(fault) = page_zero(r) {
                return fault;
            }
            let (row, col) = cursor(mem);
            for i in (col..COLS).take(r.cx as usize) {
                let old = mem.r16(TEXT_SEG, at(row, i));
                let attr = if ah == 0x09 { r.bx as u8 } else { (old >> 8) as u8 };
                mem.w16(TEXT_SEG, at(row, i), (attr as u16) << 8 | al as u16);
            }
        }
        0x0F => {
            r.set_al(mem.r8(BDA, 0x49));
            r.ax = (COLS as u16) << 8 | r.ax & 0xFF;
            r.bx &= 0x00FF; // active page 0
        }
        0x13 if al < 4 => {
            if let Err(fault) = page_zero(r) {
                return fault;
            }
            let (row, col) = (dh.min(ROWS - 1), dl.min(COLS - 1));
            let start = cursor(mem);
            set_cursor(mem, row, col);
            let with_attr = al & 2 != 0;
            let mut off = r.bp;
            for _ in 0..r.cx {
                let c = mem.r8(r.es, off);
                off = off.wrapping_add(1);
                let attr = if with_attr {
                    let a = mem.r8(r.es, off);
                    off = off.wrapping_add(1);
                    a
                } else {
                    r.bx as u8
                };
                teletype(mem, c, Some(attr));
            }
            if al & 1 == 0 {
                set_cursor(mem, start.0, start.1);
            }
        }
        // EGA information and VGA display combination: no such adapter, which
        // the BIOS signals by returning the registers unchanged.
        0x12 if r.bx as u8 == 0x10 => {}
        0x1A => {}
        _ => return unimplemented(r),
    }
    Outcome::Resume
}
