use std::collections::VecDeque;

use crate::*;

#[derive(Default)]
struct Console {
    input: VecDeque<u8>,
    output: Vec<u8>,
}

impl CharDevice for Console {
    fn read(&mut self) -> Option<u8> {
        self.input.pop_front()
    }
    fn write(&mut self, byte: u8) {
        self.output.push(byte);
    }
}

struct Machine {
    ram: Vec<u8>,
    dos: Dos,
    con: Console,
    regs: Regs,
}

impl Machine {
    fn boot(files: &[(&str, &[u8])], program: &str, tail: &[u8]) -> Result<Self, LoadError> {
        let mut ram = vec![0; RAM_SIZE];
        let mut volume = Volume::new(1 << 20);
        for (name, data) in files {
            volume.put(name, data.to_vec());
        }
        let mut dos = Dos::new(&mut Mem::new(&mut ram), volume);
        let regs = dos.exec(&mut Mem::new(&mut ram), program, tail)?;
        Ok(Self { ram, dos, con: Console::default(), regs })
    }

    fn mem(&mut self) -> Mem<'_> {
        Mem::new(&mut self.ram)
    }

    /// Raise `INT 21h` the way the processor would: push an interrupt frame,
    /// call the service, pop the frame. Returns the outcome and the flags the
    /// caller sees afterwards.
    fn int21(&mut self) -> (Outcome, u16) {
        let (ss, sp) = (self.regs.ss, self.regs.sp.wrapping_sub(6));
        let mut mem = Mem::new(&mut self.ram);
        mem.w16(ss, sp.wrapping_add(4), self.regs.flags);
        self.regs.sp = sp;
        let outcome = self.dos.interrupt(0x21, &mut self.regs, &mut mem, &mut self.con);
        self.regs.sp = sp.wrapping_add(6);
        self.regs.flags = mem.r16(ss, sp.wrapping_add(4));
        (outcome, self.regs.flags)
    }

    fn call(&mut self, ax: u16) -> bool {
        self.regs.ax = ax;
        let (outcome, flags) = self.int21();
        assert_eq!(outcome, Outcome::Resume);
        flags & FLAG_CF == 0
    }
}

#[test]
fn com_program_gets_a_psp_and_a_return_path() {
    let mut m = Machine::boot(&[("hello.com", &[0xC3])], "hello.com", b" one two").unwrap();
    let r = m.regs;
    assert_eq!((r.cs, r.ds, r.es, r.ss, r.ip), (r.cs, r.cs, r.cs, r.cs, 0x100));
    let mem = m.mem();
    assert_eq!(mem.r8(r.cs, 0x100), 0xC3);
    assert_eq!(mem.r16(r.cs, 0), 0x20CD, "PSP starts with INT 20h");
    assert_eq!(mem.r16(r.ss, r.sp), 0, "a near ret lands on PSP:0");
    assert_eq!(mem.r8(r.cs, 0x80), 8);
    let mut tail = [0; 9];
    mem.read(r.cs, 0x81, &mut tail);
    assert_eq!(&tail, b" one two\r");
    assert_eq!(mem.r16(r.cs, 2), firmware::CONVENTIONAL_TOP, "a COM program owns all free memory");
    // The environment carries the program's own path after the variables.
    let env = mem.r16(r.cs, 0x2C);
    let mut block = [0; 64];
    mem.read(env, 0, &mut block);
    let text = String::from_utf8_lossy(&block);
    assert!(text.contains("\0\0\u{1}\0C:\\HELLO.COM\0"), "{text:?}");
}

fn mz(image: &[u8], relocs: &[(u16, u16)], min_extra: u16, entry: (u16, u16), stack: (u16, u16)) -> Vec<u8> {
    let header_len = (0x1C + relocs.len() * 4).div_ceil(16) * 16;
    let total = header_len + image.len();
    let mut f = vec![0; header_len];
    let mut w = |at: usize, v: u16| f[at..at + 2].copy_from_slice(&v.to_le_bytes());
    w(0, 0x5A4D);
    w(2, (total % 512) as u16);
    w(4, total.div_ceil(512) as u16);
    w(6, relocs.len() as u16);
    w(8, (header_len / 16) as u16);
    w(0x0A, min_extra);
    w(0x0C, 0xFFFF);
    w(0x0E, stack.0);
    w(0x10, stack.1);
    w(0x14, entry.1);
    w(0x16, entry.0);
    w(0x18, 0x1C);
    for (i, (off, seg)) in relocs.iter().enumerate() {
        w(0x1C + i * 4, *off);
        w(0x1E + i * 4, *seg);
    }
    f.extend_from_slice(image);
    f
}

#[test]
fn mz_program_is_relocated() {
    // Two segments; a far pointer at 1:0002 refers to segment 1.
    let mut image = vec![0x90; 16];
    image.extend_from_slice(&[0xAA, 0xBB, 0x01, 0x00]);
    let exe = mz(&image, &[(2, 1)], 0x20, (0, 5), (2, 0x80));
    let mut m = Machine::boot(&[("PROG.EXE", &exe)], "prog.exe", b"").unwrap();
    let r = m.regs;
    let base = r.ds + 0x10;
    assert_eq!((r.cs, r.ip, r.ss, r.sp, r.es), (base, 5, base + 2, 0x80, r.ds));
    let mem = m.mem();
    assert_eq!(mem.r16(base + 1, 2), base + 1, "segment reference fixed up");
    assert_eq!(mem.r16(base + 1, 0), 0xBBAA, "neighbouring data untouched");
}

#[test]
fn bad_images_are_refused() {
    let no_reloc_room = mz(&[0x90; 4], &[(8, 0)], 0, (0, 0), (0, 0x80));
    for (file, want) in [
        (&b"MZ\x01"[..], LoadError::BadImage),
        (&no_reloc_room[..], LoadError::BadImage),
        (&vec![0x90; 0xFF00][..], LoadError::BadImage),
        (&mz(&[0x90; 4], &[], 0xF000, (0, 0), (0, 0x80))[..], LoadError::NoMemory),
    ] {
        assert_eq!(Machine::boot(&[("P.EXE", file)], "P.EXE", b"").err(), Some(want));
    }
    assert_eq!(Machine::boot(&[], "P.EXE", b"").err(), Some(LoadError::NotFound));
    assert_eq!(Machine::boot(&[("P.COM", &[0xC3])], "P.COM", &[b'x'; 127]).err(), Some(LoadError::TailTooLong));
}

#[test]
fn memory_blocks_allocate_resize_and_coalesce() {
    // An MZ program that asks for little leaves the rest of memory free.
    let exe = mz(&[0x90; 16], &[], 0x10, (0, 0), (0, 0x80));
    let mut exe = exe;
    exe[0x0C..0x0E].copy_from_slice(&0x10u16.to_le_bytes()); // max extra
    let mut m = Machine::boot(&[("P.EXE", &exe)], "P.EXE", b"").unwrap();

    m.regs.bx = 0xFFFF;
    assert!(!m.call(0x4800));
    let largest = m.regs.bx;
    assert_eq!(m.regs.ax, 8);
    assert!(largest > 0x8000);

    m.regs.bx = 0x100;
    assert!(m.call(0x4800));
    let a = m.regs.ax;
    m.regs.bx = 0x100;
    assert!(m.call(0x4800));
    let b = m.regs.ax;
    assert_eq!(b, a + 0x101);

    // Growing `a` into `b` fails and reports what it has; after freeing `b`
    // it succeeds.
    m.regs.es = a;
    m.regs.bx = 0x180;
    assert!(!m.call(0x4A00));
    assert_eq!((m.regs.ax, m.regs.bx), (8, 0x100));
    m.regs.es = b;
    assert!(m.call(0x4900));
    assert!(!m.call(0x4900), "double free is refused");
    assert_eq!(m.regs.ax, 9);
    m.regs.es = a;
    m.regs.bx = 0x180;
    assert!(m.call(0x4A00));

    m.regs.es = a;
    assert!(m.call(0x4900));
    m.regs.bx = largest;
    assert!(m.call(0x4800), "freed blocks coalesce back into one");
}

#[test]
fn console_services_follow_dos_conventions() {
    let mut m = Machine::boot(&[("P.COM", &[0xC3])], "P.COM", b"").unwrap();
    let (ds, flags) = (m.regs.ds, m.regs.flags);

    m.mem().write(ds, 0x200, b"hi$");
    m.regs.dx = 0x200;
    assert!(m.call(0x0900));
    assert_eq!(m.con.output, b"hi");

    // Buffered input waits, leaving the caller's state alone, until Enter.
    m.con.output.clear();
    m.mem().w8(ds, 0x300, 5);
    m.regs.dx = 0x300;
    m.regs.ax = 0x0A00;
    let before = m.regs;
    m.con.input.extend(b"ab");
    assert_eq!(m.int21(), (Outcome::WaitInput, flags));
    assert_eq!(m.regs, before);
    m.con.input.extend(b"X\x08cdefg\r");
    assert_eq!(m.int21().0, Outcome::Resume);
    let mut buf = [0; 7];
    m.mem().read(ds, 0x300, &mut buf);
    assert_eq!(&buf, b"\x05\x04abcd\r");
    assert_eq!(m.con.output, b"abX\x08 \x08cd\x07\x07\x07\r");

    // Handle reads are line-buffered; a line outlives a short read.
    m.con.output.clear();
    m.con.input.extend(b"line\r\nmore\r");
    m.regs.bx = 0;
    m.regs.cx = 3;
    m.regs.dx = 0x400;
    assert!(m.call(0x3F00));
    assert_eq!(m.regs.ax, 3);
    m.regs.cx = 100;
    assert!(m.call(0x3F00));
    assert_eq!(m.regs.ax, 3);
    let mut buf = [0; 3];
    m.mem().read(ds, 0x400, &mut buf);
    assert_eq!(&buf, b"e\r\n");
    assert_eq!(m.con.output, b"line\r\n");

    // Ctrl-Z at the start of a line reads as end of file.
    m.con.input.clear();
    m.con.input.extend(b"\x1a\r");
    assert!(m.call(0x3F00));
    assert_eq!(m.regs.ax, 0);

    // Direct console input reports "nothing there" through ZF.
    m.con.input.clear();
    m.regs.dx = 0x00FF;
    m.regs.ax = 0x0600;
    assert_ne!(m.int21().1 & FLAG_ZF, 0);
    m.con.input.push_back(b'k');
    assert!(m.call(0x0B00));
    assert_eq!(m.regs.al(), 0xFF);
    m.regs.ax = 0x0600;
    assert_eq!(m.int21().1 & FLAG_ZF, 0);
    assert_eq!(m.regs.al(), b'k');
}

#[test]
fn files_live_on_drive_c() {
    let mut m = Machine::boot(&[("P.COM", &[0xC3]), ("DATA.TXT", b"0123456789")], "P.COM", b"").unwrap();
    let ds = m.regs.ds;
    let name = |m: &mut Machine, s: &[u8]| {
        m.mem().write(ds, 0x200, s);
        m.regs.dx = 0x200;
    };

    name(&mut m, b"c:\\data.txt\0");
    assert!(m.call(0x3D00));
    let h = m.regs.ax;
    assert_eq!(h, 5, "first free handle after the five standard ones");

    (m.regs.bx, m.regs.cx, m.regs.dx) = (h, 0xFFFF, 0xFFFD);
    assert!(m.call(0x4202), "seek to three before the end");
    assert_eq!((m.regs.dx, m.regs.ax), (0, 7));
    (m.regs.cx, m.regs.dx) = (100, 0x400);
    assert!(m.call(0x3F00));
    assert_eq!(m.regs.ax, 3);
    (m.regs.cx, m.regs.dx) = (1, 0x400);
    assert!(!m.call(0x4000), "opened read-only");
    assert_eq!(m.regs.ax, 5);
    assert!(m.call(0x4400));
    assert_eq!(m.regs.dx & 0x80, 0, "a file, not a device");
    assert!(m.call(0x3E00));
    assert!(!m.call(0x3E00));
    assert_eq!(m.regs.ax, 6);

    name(&mut m, b"NEW.TXT\0");
    assert!(!m.call(0x3D00));
    assert_eq!(m.regs.ax, 2);
    m.regs.ax = 0x5900;
    assert_eq!(m.int21().0, Outcome::Resume);
    assert_eq!((m.regs.ax, m.regs.bx, m.regs.cx >> 8), (2, 0x0803, 2), "extended error repeats the failure");
    assert!(m.call(0x3C00));
    m.mem().write(ds, 0x400, b"written");
    (m.regs.bx, m.regs.cx, m.regs.dx) = (m.regs.ax, 7, 0x400);
    assert!(m.call(0x4000));
    assert_eq!(m.dos.volume().get("new.txt"), Some(&b"written"[..]));
    (m.regs.cx, m.regs.dx) = (0, 4);
    assert!(m.call(0x4200));
    m.regs.cx = 0;
    assert!(m.call(0x4000), "zero-length write truncates");
    assert_eq!(m.dos.volume().get("NEW.TXT"), Some(&b"writ"[..]));

    // A full disk shows as a short write, wherever the file pointer is.
    let free = m.dos.volume().free();
    assert_eq!(free, (1 << 20) - 4 - 1 - 10);
    (m.regs.cx, m.regs.dx) = (0xFFFF, 0xFFFF);
    assert!(m.call(0x4200), "seek far past the end");
    (m.regs.cx, m.regs.dx) = (1, 0x400);
    assert!(m.call(0x4000));
    assert_eq!(m.regs.ax, 0);
    m.regs.cx = 0;
    assert!(m.call(0x4000));
    assert_eq!(m.dos.volume().free(), free, "nor can a file be extended past it");

    name(&mut m, b"NEW.TXT\0");
    assert!(m.call(0x4100));
    assert_eq!(m.dos.volume().get("NEW.TXT"), None);
    name(&mut m, b"A:\\X\0");
    assert!(!m.call(0x3D00));
    assert_eq!(m.regs.ax, 3);
    name(&mut m, b"SUB\\X\0");
    assert!(!m.call(0x3D00));
    assert_eq!(m.regs.ax, 3);

    m.con.output.clear();
    name(&mut m, b"con\0");
    assert!(m.call(0x3D01));
    m.mem().write(ds, 0x400, b"ok");
    (m.regs.bx, m.regs.cx, m.regs.dx) = (m.regs.ax, 2, 0x400);
    assert!(m.call(0x4000));
    assert_eq!(m.con.output, b"ok");
}

#[test]
fn vectors_are_real_and_unknown_services_fault() {
    let mut m = Machine::boot(&[("P.COM", &[0xC3])], "P.COM", b"").unwrap();
    assert!(m.call(0x3521));
    assert_eq!(firmware::gate_vector(m.regs.es, m.regs.bx), Some(0x21));
    assert_eq!(firmware::gate_vector(m.regs.es, m.regs.bx + 1), None);
    (m.regs.ds, m.regs.dx) = (0x1234, 0x5678);
    assert!(m.call(0x2560));
    assert!(m.call(0x3560));
    assert_eq!((m.regs.es, m.regs.bx), (0x1234, 0x5678));

    m.regs.ax = 0x4B00;
    assert_eq!(m.int21().0, Outcome::Fault(Fault::Unimplemented { vector: 0x21, ah: 0x4B, al: 0 }));
    m.regs.ax = 0x4C2A;
    assert_eq!(m.int21().0, Outcome::Exit(0x2A));
    let (mut regs, mut ram) = (m.regs, std::mem::take(&mut m.ram));
    let out = m.dos.interrupt(0x00, &mut regs, &mut Mem::new(&mut ram), &mut m.con);
    assert_eq!(out, Outcome::Fault(Fault::Exception { vector: 0 }));
}
