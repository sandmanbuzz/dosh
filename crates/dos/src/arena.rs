//! Conventional-memory arena: the DOS memory control block chain.
//!
//! The chain lives in guest memory in the documented layout, because
//! programs walk it. Each block is preceded by a 16-byte MCB:
//! signature (`M`, or `Z` for the last), owner PSP (0 = free), size in
//! paragraphs.

use crate::firmware::CONVENTIONAL_TOP;
use crate::mem::Mem;

pub const ERR_ARENA_TRASHED: u16 = 7;
pub const ERR_NO_MEMORY: u16 = 8;
pub const ERR_BAD_BLOCK: u16 = 9;

/// First MCB. Programs load above 64 KiB, as they would under a DOS with a
/// few drivers resident; some packed executables fail when loaded lower.
const FIRST_MCB: u16 = 0x1000;

struct Mcb {
    seg: u16,
    last: bool,
    owner: u16,
    size: u16,
}

fn load(mem: &Mem, seg: u16) -> Result<Mcb, u16> {
    let last = match mem.r8(seg, 0) {
        b'M' => false,
        b'Z' => true,
        _ => return Err(ERR_ARENA_TRASHED),
    };
    let size = mem.r16(seg, 3);
    if seg as u32 + 1 + size as u32 > CONVENTIONAL_TOP as u32 {
        return Err(ERR_ARENA_TRASHED);
    }
    Ok(Mcb { seg, last, owner: mem.r16(seg, 1), size })
}

fn store(mem: &mut Mem, mcb: &Mcb) {
    mem.w8(mcb.seg, 0, if mcb.last { b'Z' } else { b'M' });
    mem.w16(mcb.seg, 1, mcb.owner);
    mem.w16(mcb.seg, 3, mcb.size);
}

/// Absorb any free blocks directly after `mcb` into it.
fn absorb_free(mem: &mut Mem, mcb: &mut Mcb) -> Result<(), u16> {
    while !mcb.last {
        let next = load(mem, mcb.seg + mcb.size + 1)?;
        if next.owner != 0 {
            break;
        }
        mcb.size += next.size + 1;
        mcb.last = next.last;
    }
    store(mem, mcb);
    Ok(())
}

/// Cut `mcb` down to `paras`, leaving the remainder as a free block.
fn split(mem: &mut Mem, mcb: &mut Mcb, paras: u16) {
    if mcb.size > paras {
        let rest = Mcb { seg: mcb.seg + paras + 1, last: mcb.last, owner: 0, size: mcb.size - paras - 1 };
        store(mem, &rest);
        mcb.size = paras;
        mcb.last = false;
    }
    store(mem, mcb);
}

pub fn init(mem: &mut Mem) {
    let all = Mcb { seg: FIRST_MCB, last: true, owner: 0, size: CONVENTIONAL_TOP - FIRST_MCB - 1 };
    store(mem, &all);
}

/// First-fit allocation. On failure returns the error and the largest block
/// that could have been allocated.
pub fn alloc(mem: &mut Mem, paras: u16, owner: u16) -> Result<u16, (u16, u16)> {
    let mut largest = 0;
    let mut seg = FIRST_MCB;
    loop {
        let mut mcb = load(mem, seg).map_err(|e| (e, 0))?;
        if mcb.owner == 0 {
            absorb_free(mem, &mut mcb).map_err(|e| (e, 0))?;
            if mcb.size >= paras {
                mcb.owner = owner;
                split(mem, &mut mcb, paras);
                return Ok(mcb.seg + 1);
            }
            largest = largest.max(mcb.size);
        }
        if mcb.last {
            return Err((ERR_NO_MEMORY, largest));
        }
        seg = mcb.seg + mcb.size + 1;
    }
}

/// The MCB for the block whose data starts at `block`, if it is allocated.
fn owned(mem: &Mem, block: u16) -> Result<Mcb, u16> {
    if block <= FIRST_MCB || block >= CONVENTIONAL_TOP {
        return Err(ERR_BAD_BLOCK);
    }
    let mcb = load(mem, block - 1).map_err(|_| ERR_BAD_BLOCK)?;
    if mcb.owner == 0 {
        return Err(ERR_BAD_BLOCK);
    }
    Ok(mcb)
}

pub fn free(mem: &mut Mem, block: u16) -> Result<(), u16> {
    let mut mcb = owned(mem, block)?;
    mcb.owner = 0;
    store(mem, &mcb);
    Ok(())
}

/// Grow or shrink a block in place. On failure returns the error and the
/// largest size the block could take; as in DOS, the block is left at that
/// size.
pub fn resize(mem: &mut Mem, block: u16, paras: u16) -> Result<(), (u16, u16)> {
    let mut mcb = owned(mem, block).map_err(|e| (e, 0))?;
    absorb_free(mem, &mut mcb).map_err(|e| (e, 0))?;
    if mcb.size < paras {
        return Err((ERR_NO_MEMORY, mcb.size));
    }
    split(mem, &mut mcb, paras);
    Ok(())
}

pub fn set_owner(mem: &mut Mem, block: u16, owner: u16) {
    mem.w16(block - 1, 1, owner);
}

pub fn size(mem: &Mem, block: u16) -> u16 {
    mem.r16(block - 1, 3)
}
