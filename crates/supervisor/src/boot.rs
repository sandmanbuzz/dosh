//! Firmware phase: gather everything the supervisor needs from the firmware
//! while boot services still exist.
//!
//! Work is provisioned from the EFI system partition DOSH booted from:
//!
//! ```text
//! \DOSH\BOOT.CFG    one `run PROGRAM [arguments]` line per capsule
//! \DOSH\C\          the files each capsule finds on its drive C:
//! ```

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec::Vec;

use linked_list_allocator::LockedHeap;
use uefi::boot::{AllocateType, MemoryType};
use uefi::fs::FileSystem;
use uefi::mem::memory_map::{MemoryMap, MemoryMapOwned};
use uefi::{cstr16, CString16};

/// Everything the supervisor allocates comes from one pool claimed from the
/// firmware up front, so that nothing depends on boot services later.
const POOL_BYTES: usize = 64 << 20;

#[global_allocator]
static HEAP: LockedHeap = LockedHeap::empty();

#[derive(Debug, PartialEq, Eq)]
pub struct CapsuleSpec {
    /// CPU time the capsule may use, in seconds, before the supervisor stops it.
    pub cpu_limit: Option<u64>,
    /// The persistent volume drive C: is kept on, upper case; `NONE` keeps
    /// nothing. Unnamed means the default volume, if there is one. Once
    /// launched, a capsule's is the volume it is on, or none.
    pub volume: Option<String>,
    pub program: String,
    /// The command tail exactly as DOS would pass it, leading space included.
    pub tail: Vec<u8>,
}

#[derive(Default)]
pub struct Provision {
    pub capsules: Vec<CapsuleSpec>,
    /// The files every capsule finds on its drive C: before the persistent
    /// disk lays its own over them.
    pub files: Vec<(String, Vec<u8>)>,
    /// Whether to wait for the operator's commands once the manifest is done,
    /// instead of powering off.
    pub stay: bool,
    /// Whether every capsule is presented on one line, a capsule at a time.
    pub shared_console: bool,
    /// The firmware's clock when the supervisor started, if it has one.
    pub wall_clock: Option<dosh_dos::DateTime>,
    pub error: Option<String>,
}

pub fn provision() -> Provision {
    let pool = uefi::boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, POOL_BYTES / 4096)
        .expect("firmware cannot spare the supervisor pool");
    // SAFETY: the pool is ours alone and is never returned to the firmware.
    unsafe { HEAP.lock().init(pool.as_ptr(), POOL_BYTES) };

    let wall_clock = uefi::runtime::get_time().ok().map(|t| dosh_dos::DateTime {
        year: t.year(),
        month: t.month(),
        day: t.day(),
        hour: t.hour(),
        minute: t.minute(),
        second: t.second(),
    });
    match read_esp() {
        Ok(mut provision) => {
            provision.wall_clock = wall_clock;
            provision
        }
        Err(error) => Provision { wall_clock, error: Some(error), ..Provision::default() },
    }
}

fn read_esp() -> Result<Provision, String> {
    let volume =
        uefi::boot::get_image_file_system(uefi::boot::image_handle()).map_err(|e| format!("boot volume: {e}"))?;
    let mut fs = FileSystem::new(volume);

    let manifest = fs.read(cstr16!("\\DOSH\\BOOT.CFG")).map_err(|e| format!("\\DOSH\\BOOT.CFG: {e}"))?;
    let manifest = String::from_utf8(manifest).map_err(|_| "\\DOSH\\BOOT.CFG: not text".to_string())?;

    let mut files = Vec::new();
    for entry in fs.read_dir(cstr16!("\\DOSH\\C")).map_err(|e| format!("\\DOSH\\C: {e}"))? {
        let entry = entry.map_err(|e| format!("\\DOSH\\C: {e}"))?;
        if entry.is_regular_file() {
            let name = entry.file_name().to_string();
            let path = format!("\\DOSH\\C\\{name}");
            let wide = CString16::try_from(path.as_str()).map_err(|_| format!("{path}: bad name"))?;
            files.push((name, fs.read(wide.as_ref() as &uefi::CStr16).map_err(|e| format!("{path}: {e}"))?));
        }
    }

    let mut capsules = Vec::new();
    let mut shared_console = false;
    let mut stay = false;
    for line in manifest.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        if line == "console mux" {
            shared_console = true;
            continue;
        }
        if line == "supervisor stay" {
            stay = true;
            continue;
        }
        let rest = line.strip_prefix("run ").ok_or_else(|| format!("BOOT.CFG: unknown directive: {line}"))?;
        capsules.push(parse_run(rest).map_err(|why| format!("BOOT.CFG: {why}: {line}"))?);
    }
    Ok(Provision { capsules, files, stay, shared_console, wall_clock: None, error: None })
}

/// A capsule's `[limit=SECONDS] [vol=NAME] PROGRAM [ARGUMENTS]`, as the
/// manifest and the operator's `launch` both write it.
pub fn parse_run(text: &str) -> Result<CapsuleSpec, String> {
    let mut rest = text.trim_start();
    let (mut cpu_limit, mut volume) = (None, None);
    loop {
        let (option, after) = rest.split_at(rest.find(' ').unwrap_or(rest.len()));
        if let Some(seconds) = option.strip_prefix("limit=") {
            cpu_limit = Some(seconds.parse().map_err(|_| "bad limit".to_string())?);
        } else if let Some(name) = option.strip_prefix("vol=") {
            volume = Some(name.to_ascii_uppercase());
        } else {
            break;
        }
        rest = after.trim_start();
    }
    if rest.is_empty() {
        return Err("needs a program".to_string());
    }
    let (program, tail) = rest.split_at(rest.find(' ').unwrap_or(rest.len()));
    Ok(CapsuleSpec { cpu_limit, volume, program: program.to_string(), tail: tail.as_bytes().to_vec() })
}

/// The end of the highest region of actual memory in the firmware's map.
/// Device windows, which firmware may place anywhere in the address space,
/// do not count.
pub fn top_of_memory(map: &MemoryMapOwned) -> u64 {
    const DEVICES: [MemoryType; 4] =
        [MemoryType::MMIO, MemoryType::MMIO_PORT_SPACE, MemoryType::RESERVED, MemoryType::UNUSABLE];
    map.entries().filter(|d| !DEVICES.contains(&d.ty)).map(|d| d.phys_start + d.page_count * 4096).max().unwrap_or(0)
}
