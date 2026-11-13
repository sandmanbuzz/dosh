//! Drawing a text-mode display on an ANSI terminal.
//!
//! A [`Terminal`] remembers what the far end last showed and, given the
//! display's current cells, emits just the escape sequences and characters
//! that bring it up to date. It knows nothing about DOS or about how bytes
//! reach the terminal: cells in, bytes out.

#![cfg_attr(not(test), no_std)]

/// IBM PC code page 437 for 80h-FFh, as the glyphs the display adapter draws.
const CP437_HIGH: &str = "ÇüéâäàåçêëèïîìÄÅÉæÆôöòûùÿÖÜ¢£¥₧ƒáíóúñÑªº¿⌐¬½¼¡«»░▒▓│┤╡╢╖╕╣║╗╝╜╛┐└┴┬├─┼╞╟╚╔╩╦╠═╬╧╨╤╥╙╘╒╓╫╪┘┌█▄▌▐▀αßΓπΣσµτΦΘΩδ∞φε∩≡±≥≤⌠⌡÷≈°∙·√ⁿ²■\u{a0}";
/// 00h-1Fh: what the adapter draws for control codes.
const CP437_LOW: &str = " ☺☻♥♦♣♠•◘○◙♂♀♪♫☼►◄↕‼¶§▬↨↑↓→←∟↔▲▼";

/// The glyph the display adapter shows for a character code.
pub fn glyph(code: u8) -> char {
    match code {
        0x00..=0x1F => CP437_LOW.chars().nth(code as usize),
        0x20..=0x7E => Some(code as char),
        0x7F => Some('⌂'),
        _ => CP437_HIGH.chars().nth(code as usize - 0x80),
    }
    .unwrap_or('?')
}

const BLANK: u16 = 0x0720;

pub struct Terminal<const CELLS: usize> {
    shown: [u16; CELLS],
    started: bool,
    cursor: (u8, u8),
    cursor_visible: bool,
    attr: u8,
}

impl<const CELLS: usize> Default for Terminal<CELLS> {
    fn default() -> Self {
        Self::new()
    }
}

/// CGA colour bits are blue, green, red; ANSI's are red, green, blue.
fn ansi_colour(c: u8) -> u8 {
    (c & 2) | (c & 1) << 2 | (c & 4) >> 2
}

impl<const CELLS: usize> Terminal<CELLS> {
    pub const fn new() -> Self {
        Self { shown: [BLANK; CELLS], started: false, cursor: (0, 0), cursor_visible: true, attr: 0x07 }
    }

    /// Bring a terminal `cols` wide up to date with `cells`, then park its
    /// cursor at `cursor` (row, column). Each output byte goes to `out`.
    pub fn draw(
        &mut self,
        cells: &[u16; CELLS],
        cols: usize,
        cursor: (u8, u8),
        visible: bool,
        out: &mut impl FnMut(u8),
    ) {
        if !self.started {
            self.started = true;
            // No autowrap, so the last cell of the last row cannot scroll the
            // terminal; clear it all and start from default colours.
            put(out, "\x1b[?7l\x1b[0m\x1b[2J\x1b[H");
            self.attr = 0x07;
            self.cursor_visible = true;
            self.cursor = (0, 0);
        }
        let mut moved = false;
        for row in 0..CELLS / cols {
            let range = row * cols..(row + 1) * cols;
            let changed = |i: &usize| cells[*i] != self.shown[*i];
            let Some(first) = range.clone().find(changed) else { continue };
            let last = range.rev().find(changed).unwrap_or(first);
            number_seq(out, row + 1, first - row * cols + 1);
            for (i, &cell) in cells.iter().enumerate().take(last + 1).skip(first) {
                let attr = (cell >> 8) as u8;
                if attr != self.attr {
                    self.attr = attr;
                    sgr(out, attr);
                }
                let mut buf = [0; 4];
                glyph(cell as u8).encode_utf8(&mut buf).bytes().for_each(&mut *out);
                self.shown[i] = cell;
            }
            moved = true;
        }
        if visible != self.cursor_visible {
            self.cursor_visible = visible;
            put(out, if visible { "\x1b[?25h" } else { "\x1b[?25l" });
        }
        if moved || cursor != self.cursor {
            number_seq(out, cursor.0 as usize + 1, cursor.1 as usize + 1);
            self.cursor = cursor;
        }
    }
}

fn put(out: &mut impl FnMut(u8), text: &str) {
    text.bytes().for_each(out);
}

fn decimal(out: &mut impl FnMut(u8), mut n: usize) {
    let mut digits = [0u8; 20];
    let mut len = 0;
    loop {
        digits[len] = b'0' + (n % 10) as u8;
        len += 1;
        n /= 10;
        if n == 0 {
            break;
        }
    }
    digits[..len].iter().rev().copied().for_each(out);
}

/// `ESC [ row ; col H`
fn number_seq(out: &mut impl FnMut(u8), row: usize, col: usize) {
    put(out, "\x1b[");
    decimal(out, row);
    out(b';');
    decimal(out, col);
    out(b'H');
}

fn sgr(out: &mut impl FnMut(u8), attr: u8) {
    put(out, "\x1b[0;");
    decimal(out, 30 + ansi_colour(attr & 7) as usize);
    put(out, ";");
    decimal(out, 40 + ansi_colour(attr >> 4 & 7) as usize);
    if attr & 8 != 0 {
        put(out, ";1");
    }
    if attr & 0x80 != 0 {
        put(out, ";5");
    }
    out(b'm');
}

#[cfg(test)]
mod tests {
    use super::*;

    fn draw(t: &mut Terminal<8>, cells: &[u16; 8], cursor: (u8, u8), visible: bool) -> String {
        let mut out = Vec::new();
        t.draw(cells, 4, cursor, visible, &mut |b| out.push(b));
        String::from_utf8(out).unwrap()
    }

    #[test]
    fn code_page_437_glyphs() {
        assert_eq!(CP437_HIGH.chars().count(), 128);
        assert_eq!(CP437_LOW.chars().count(), 32);
        assert_eq!(
            [glyph(b'A'), glyph(0xC9), glyph(0xCD), glyph(0xDB), glyph(0x01), glyph(0xE1)],
            ['A', '╔', '═', '█', '☺', 'ß']
        );
    }

    #[test]
    fn first_draw_clears_and_later_draws_send_only_changes() {
        let mut t = Terminal::<8>::new();
        let mut cells = [BLANK; 8];
        assert_eq!(draw(&mut t, &cells, (0, 0), true), "\x1b[?7l\x1b[0m\x1b[2J\x1b[H");
        assert_eq!(draw(&mut t, &cells, (0, 0), true), "", "nothing changed");
        cells[5] = 0x1F00 | b'x' as u16;
        cells[6] = 0x1F00 | 0xC9;
        assert_eq!(draw(&mut t, &cells, (1, 3), true), "\x1b[2;2H\x1b[0;37;44;1mx╔\x1b[2;4H");
        // Same attribute continues without a new colour sequence.
        cells[7] = 0x1F00 | b'y' as u16;
        assert_eq!(draw(&mut t, &cells, (1, 3), false), "\x1b[2;4Hy\x1b[?25l\x1b[2;4H");
        // Cursor moves alone cost only a move.
        assert_eq!(draw(&mut t, &cells, (0, 2), false), "\x1b[1;3H");
    }

    #[test]
    fn cga_colours_become_ansi_colours() {
        let mut out = Vec::new();
        sgr(&mut |b| out.push(b), 0x4C | 0x80);
        // fg light red (CGA 12 = red+bright), bg blue (CGA 4 = red -> ANSI red).
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[0;31;41;1;5m");
        let mut out = Vec::new();
        sgr(&mut |b| out.push(b), 0x01);
        assert_eq!(String::from_utf8(out).unwrap(), "\x1b[0;34;40m"); // blue is ANSI 4
    }
}
