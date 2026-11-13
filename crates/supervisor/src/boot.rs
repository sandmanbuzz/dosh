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

pub struct CapsuleSpec {
    pub program: String,
    /// The command tail exactly as DOS would pass it, leading space included.
    pub tail: Vec<u8>,
    /// The contents of this capsule's own drive C:.
    pub files: Vec<(String, Vec<u8>)>,
}

#[derive(Default)]
pub struct Provision {
    pub capsules: Vec<CapsuleSpec>,
    pub error: Option<String>,
}

pub fn provision() -> Provision {
    let pool = uefi::boot::allocate_pages(AllocateType::AnyPages, MemoryType::LOADER_DATA, POOL_BYTES / 4096)
        .expect("firmware cannot spare the supervisor pool");
    // SAFETY: the pool is ours alone and is never returned to the firmware.
    unsafe { HEAP.lock().init(pool.as_ptr(), POOL_BYTES) };

    match read_esp() {
        Ok(capsules) => Provision { capsules, error: None },
        Err(error) => Provision { capsules: Vec::new(), error: Some(error) },
    }
}

fn read_esp() -> Result<Vec<CapsuleSpec>, String> {
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
    for line in manifest.lines().map(str::trim).filter(|l| !l.is_empty() && !l.starts_with('#')) {
        let rest = line.strip_prefix("run ").ok_or_else(|| format!("BOOT.CFG: unknown directive: {line}"))?;
        let rest = rest.trim_start();
        let (program, tail) = rest.split_at(rest.find(' ').unwrap_or(rest.len()));
        capsules.push(CapsuleSpec {
            program: program.to_string(),
            tail: tail.as_bytes().to_vec(),
            files: files.clone(),
        });
    }
    Ok(capsules)
}

/// The end of the highest region of actual memory in the firmware's map.
/// Device windows, which firmware may place anywhere in the address space,
/// do not count.
pub fn top_of_memory(map: &MemoryMapOwned) -> u64 {
    const DEVICES: [MemoryType; 4] =
        [MemoryType::MMIO, MemoryType::MMIO_PORT_SPACE, MemoryType::RESERVED, MemoryType::UNUSABLE];
    map.entries().filter(|d| !DEVICES.contains(&d.ty)).map(|d| d.phys_start + d.page_count * 4096).max().unwrap_or(0)
}
