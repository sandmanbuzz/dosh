//! Real-mode view of a capsule's memory.

/// The first megabyte: everything a real-mode program can address with the
/// A20 line off, which is how a capsule starts.
pub const RAM_SIZE: usize = 0x10_0000;

pub struct Mem<'a>(&'a mut [u8]);

impl<'a> Mem<'a> {
    pub fn new(ram: &'a mut [u8]) -> Self {
        assert_eq!(ram.len(), RAM_SIZE);
        Self(ram)
    }

    /// `seg:off` to a physical address, wrapping at 1 MiB (A20 off).
    pub fn linear(seg: u16, off: u16) -> usize {
        (((seg as usize) << 4) + off as usize) & (RAM_SIZE - 1)
    }

    pub fn r8(&self, seg: u16, off: u16) -> u8 {
        self.0[Self::linear(seg, off)]
    }

    pub fn r16(&self, seg: u16, off: u16) -> u16 {
        u16::from_le_bytes([self.r8(seg, off), self.r8(seg, off.wrapping_add(1))])
    }

    pub fn w8(&mut self, seg: u16, off: u16, value: u8) {
        self.0[Self::linear(seg, off)] = value;
    }

    pub fn w16(&mut self, seg: u16, off: u16, value: u16) {
        let [lo, hi] = value.to_le_bytes();
        self.w8(seg, off, lo);
        self.w8(seg, off.wrapping_add(1), hi);
    }

    /// Copy out of guest memory; the offset wraps within the segment.
    pub fn read(&self, seg: u16, off: u16, buf: &mut [u8]) {
        for (i, b) in buf.iter_mut().enumerate() {
            *b = self.r8(seg, off.wrapping_add(i as u16));
        }
    }

    /// Copy into guest memory; the offset wraps within the segment.
    pub fn write(&mut self, seg: u16, off: u16, data: &[u8]) {
        for (i, b) in data.iter().enumerate() {
            self.w8(seg, off.wrapping_add(i as u16), *b);
        }
    }

    /// A contiguous run starting at `seg:0`, for loading program images that
    /// span more than one segment. `None` if it would run past 1 MiB.
    pub fn span_mut(&mut self, seg: u16, len: usize) -> Option<&mut [u8]> {
        let start = (seg as usize) << 4;
        self.0.get_mut(start..start.checked_add(len)?)
    }
}
