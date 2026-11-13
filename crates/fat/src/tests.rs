use super::*;

struct Ram(Vec<u8>);

impl Ram {
    fn new(sectors: usize) -> Self {
        Self(vec![0; sectors * SECTOR])
    }
}

impl Block for Ram {
    fn read(&mut self, sector: u64, buf: &mut [u8]) -> Result<(), Error> {
        let at = sector as usize * SECTOR;
        buf.copy_from_slice(self.0.get(at..at + buf.len()).ok_or(Error::Io)?);
        Ok(())
    }
    fn write(&mut self, sector: u64, buf: &[u8]) -> Result<(), Error> {
        let at = sector as usize * SECTOR;
        self.0.get_mut(at..at + buf.len()).ok_or(Error::Io)?.copy_from_slice(buf);
        Ok(())
    }
    fn sectors(&self) -> u64 {
        (self.0.len() / SECTOR) as u64
    }
}

fn blank(mb: usize) -> Volume<Ram> {
    let mut ram = Ram::new(mb * 2048);
    format(&mut ram, "doshdata").unwrap();
    Volume::open(ram).unwrap()
}

#[test]
fn a_formatted_disk_is_empty_and_labelled() {
    let mut v = blank(16);
    assert_eq!(v.label(), "DOSHDATA");
    assert!(v.names().unwrap().is_empty());
}

#[test]
fn files_round_trip_across_a_reopen() {
    let mut v = blank(16);
    let big: Vec<u8> = (0..100_000u32).map(|i| (i * 7) as u8).collect();
    v.write("hello.txt", b"hi").unwrap();
    v.write("BIG.DAT", &big).unwrap();
    v.write("EMPTY", b"").unwrap();
    let mut v = Volume::open(v.into_inner()).unwrap();
    assert_eq!(v.read("HELLO.TXT").unwrap().unwrap(), b"hi");
    assert_eq!(v.read("big.dat").unwrap().unwrap(), big);
    assert_eq!(v.read("EMPTY").unwrap().unwrap(), b"");
    let mut names = v.names().unwrap();
    names.sort();
    assert_eq!(names, ["BIG.DAT", "EMPTY", "HELLO.TXT"]);
}

#[test]
fn replacing_a_file_frees_the_old_space() {
    let mut v = blank(16);
    let before = v.free_bytes().unwrap();
    v.write("A.DAT", &vec![1; 50_000]).unwrap();
    v.write("A.DAT", &[2; 10]).unwrap();
    assert_eq!(v.read("A.DAT").unwrap().unwrap(), [2; 10]);
    assert!(before - v.free_bytes().unwrap() <= 2048 * 2);
    v.delete("A.DAT").unwrap();
    assert_eq!(v.free_bytes().unwrap(), before);
    assert_eq!(v.read("A.DAT").unwrap(), None);
    assert!(!v.delete("A.DAT").unwrap());
}

#[test]
fn a_full_disk_says_so_and_loses_nothing() {
    let mut v = blank(4);
    v.write("KEEP.DAT", b"keep").unwrap();
    assert_eq!(v.write("HUGE.DAT", &vec![0; 8 << 20]), Err(Error::Full));
    assert_eq!(v.read("KEEP.DAT").unwrap().unwrap(), b"keep");
    assert_eq!(v.read("HUGE.DAT").unwrap(), None);
    // the failed attempt gave its clusters back
    v.write("FITS.DAT", &vec![0; 3 << 20]).unwrap();
}

#[test]
fn a_full_directory_says_so() {
    let mut v = blank(16);
    for i in 0..512 {
        v.write(&format!("F{i}.TXT"), b"x").unwrap();
    }
    assert_eq!(v.write("ONEMORE.TXT", b"x"), Err(Error::Full));
    v.delete("F7.TXT").unwrap();
    v.write("ONEMORE.TXT", b"x").unwrap();
}

#[test]
fn names_that_do_not_fit_are_refused() {
    let mut v = blank(16);
    for bad in ["", "TOOLONGNAME.TXT", "A.TOOL", "A B.TXT", "A*.TXT", ".HID"] {
        assert_eq!(v.write(bad, b"x"), Err(Error::Name), "{bad}");
    }
}

#[test]
fn a_disk_that_is_not_fat_is_refused() {
    assert!(matches!(Volume::open(Ram::new(100)), Err(Error::Format(_))));
}

#[test]
fn long_names_and_directories_are_left_alone() {
    let mut v = blank(16);
    v.write("MINE.TXT", b"mine").unwrap();
    // Plant an LFN fragment, a volume label and a directory by hand.
    let mut dev = v.into_inner();
    let mut sector = [0u8; SECTOR];
    // The root directory is the one sector that starts with our entry.
    let at = (0..dev.sectors()).find(|&s| dev.read(s, &mut sector).is_ok() && &sector[..8] == b"MINE    ").unwrap();
    sector[32..43].copy_from_slice(b"LONGNAME   ");
    sector[43] = ATTR_LONG_NAME;
    sector[64..75].copy_from_slice(b"SUBDIR     ");
    sector[75] = ATTR_DIRECTORY;
    dev.write(at, &sector).unwrap();
    let mut v = Volume::open(dev).unwrap();
    assert_eq!(v.names().unwrap(), ["MINE.TXT"]);
    v.write("NEXT.TXT", b"n").unwrap();
    let mut dev = v.into_inner();
    dev.read(at, &mut sector).unwrap();
    assert_eq!(&sector[32..43], b"LONGNAME   ");
    assert_eq!(&sector[64..75], b"SUBDIR     ");
}

#[test]
fn a_cyclic_chain_is_corrupt_not_a_hang() {
    let mut v = blank(16);
    v.write("A.DAT", &vec![1; 5000]).unwrap();
    let first = {
        let (_, e) = v.find(&short_name("A.DAT").unwrap()).unwrap().unwrap();
        v.first_cluster(&e)
    };
    v.set(first, first).unwrap();
    assert_eq!(v.read("A.DAT"), Err(Error::Corrupt));
}
