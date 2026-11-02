//! Four-level x86_64 virtual-memory inspection and kernel mapping primitives.
//!
//! This first VMM manages the currently active Limine address space and supervisor mappings only.
//! It can walk 4 KiB, 2 MiB, and 1 GiB mappings, add/remove 4 KiB kernel mappings, and invalidate
//! local TLB entries. Separate process address spaces, user mappings, page-table reclamation, and
//! SMP TLB shootdowns are intentionally not enabled yet.

use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::sync::SpinLock;

const PAGE_SIZE: u64 = 4096;
const ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
const PDPT_LARGE_MASK: u64 = 0x000F_FFFF_C000_0000;
const PD_LARGE_MASK: u64 = 0x000F_FFFF_FFE0_0000;
const PRESENT: u64 = 1 << 0;
pub const MAP_WRITABLE: u64 = 1 << 1;
const WRITABLE: u64 = MAP_WRITABLE;
const USER: u64 = 1 << 2;
pub const MAP_WRITE_THROUGH: u64 = 1 << 3;
const WRITE_THROUGH: u64 = MAP_WRITE_THROUGH;
pub const MAP_CACHE_DISABLE: u64 = 1 << 4;
const CACHE_DISABLE: u64 = MAP_CACHE_DISABLE;
const LARGE: u64 = 1 << 7;
const CR4_PGE: u64 = 1 << 7;
const CR4_LA57: u64 = 1 << 12;
pub const MAP_GLOBAL: u64 = 1 << 8;
const GLOBAL: u64 = MAP_GLOBAL;
pub const MAP_NO_EXECUTE: u64 = 1 << 63;
const NO_EXECUTE: u64 = MAP_NO_EXECUTE;
const ALLOWED_FLAGS: u64 = WRITABLE | WRITE_THROUGH | CACHE_DISABLE | GLOBAL | NO_EXECUTE;

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static NX_ENABLED: AtomicBool = AtomicBool::new(false);
static ROOT_PHYSICAL: AtomicU64 = AtomicU64::new(0);
static MAPPED_PAGES: AtomicU64 = AtomicU64::new(0);
static UNMAPPED_PAGES: AtomicU64 = AtomicU64::new(0);
static TABLES_ALLOCATED: AtomicU64 = AtomicU64::new(0);
static WALKS: AtomicU64 = AtomicU64::new(0);
static VM_LOCK: SpinLock<()> = SpinLock::new(());

#[derive(Clone, Copy)]
pub struct Translation {
    pub physical_address: u64,
    pub page_size: u64,
    pub flags: u64,
}

#[derive(Clone, Copy)]
pub struct VmStats {
    pub root_physical: u64,
    pub nx_enabled: bool,
    pub mapped_pages: u64,
    pub unmapped_pages: u64,
    pub page_tables_allocated: u64,
    pub page_walks: u64,
}

pub fn initialize() -> Result<(), &'static str> {
    if INITIALIZED.load(Ordering::Acquire) { return Err("VMM IS ALREADY INITIALIZED"); }
    if read_cr4() & CR4_LA57 != 0 {
        return Err("FIVE-LEVEL PAGING IS ENABLED; THIS VMM SUPPORTS FOUR-LEVEL TABLES ONLY");
    }
    let root = read_cr3() & ADDRESS_MASK;
    if root == 0 || root % PAGE_SIZE != 0 {
        return Err("ACTIVE CR3 DOES NOT CONTAIN A VALID PML4 ROOT");
    }
    if crate::memory::physical_to_virtual(root).is_none() {
        return Err("ACTIVE PML4 ROOT IS OUTSIDE THE PMM HHDM WINDOW");
    }
    ROOT_PHYSICAL.store(root, Ordering::Release);
    NX_ENABLED.store(enable_nx_if_supported(), Ordering::Release);
    INITIALIZED.store(true, Ordering::Release);
    Ok(())
}

pub fn is_initialized() -> bool { INITIALIZED.load(Ordering::Acquire) }
pub fn nx_is_enabled() -> bool { NX_ENABLED.load(Ordering::Acquire) }

pub fn current_root_physical() -> Option<u64> {
    if !is_initialized() { None } else { Some(read_cr3() & ADDRESS_MASK) }
}

/// Walk the active page tables and translate a canonical virtual address.
/// Large 1 GiB and 2 MiB leaves are supported in addition to ordinary 4 KiB PTEs.
pub fn translate(virtual_address: u64) -> Option<Translation> {
    if !is_initialized() || !is_canonical(virtual_address) { return None; }
    WALKS.fetch_add(1, Ordering::Relaxed);
    let root_physical = read_cr3() & ADDRESS_MASK;
    let root_virtual = crate::memory::physical_to_virtual(root_physical)?;
    let indexes = page_indexes(virtual_address);
    let mut table = root_virtual.cast::<u64>();

    for level in 0..3 {
        let entry = unsafe { core::ptr::read_volatile(table.add(indexes[level])) };
        if entry & PRESENT == 0 { return None; }
        if level == 1 && entry & LARGE != 0 {
            let base = entry & PDPT_LARGE_MASK;
            let offset = virtual_address & ((1u64 << 30) - 1);
            return Some(Translation { physical_address: base + offset, page_size: 1u64 << 30, flags: entry & !PDPT_LARGE_MASK });
        }
        if level == 2 && entry & LARGE != 0 {
            let base = entry & PD_LARGE_MASK;
            let offset = virtual_address & ((1u64 << 21) - 1);
            return Some(Translation { physical_address: base + offset, page_size: 1u64 << 21, flags: entry & !PD_LARGE_MASK });
        }
        table = crate::memory::physical_to_virtual(entry & ADDRESS_MASK)?.cast::<u64>();
    }

    let entry = unsafe { core::ptr::read_volatile(table.add(indexes[3])) };
    if entry & PRESENT == 0 { return None; }
    Some(Translation {
        physical_address: (entry & ADDRESS_MASK) | (virtual_address & (PAGE_SIZE - 1)),
        page_size: PAGE_SIZE,
        flags: entry & !ADDRESS_MASK,
    })
}

/// Map one 4 KiB page in the active address space. Only supervisor mappings are accepted.
/// The caller owns the physical page and is responsible for not replacing a live kernel mapping.
pub fn map_page(virtual_address: u64, physical_address: u64, flags: u64) -> Result<(), &'static str> {
    if !is_initialized() { return Err("VMM IS NOT INITIALIZED"); }
    if !is_canonical(virtual_address) { return Err("VIRTUAL ADDRESS IS NOT CANONICAL"); }
    if virtual_address % PAGE_SIZE != 0 || physical_address % PAGE_SIZE != 0 {
        return Err("VIRTUAL AND PHYSICAL ADDRESSES MUST BE 4 KIB ALIGNED");
    }
    if physical_address == 0 {
        return Err("PHYSICAL ADDRESS ZERO IS RESERVED BY THIS KERNEL");
    }
    if physical_address >= 4 * 1024 * 1024 * 1024 {
        return Err("THIS VMM RELEASE MAPS PHYSICAL MEMORY BELOW 4 GIB ONLY");
    }
    if flags & !ALLOWED_FLAGS != 0 || flags & USER != 0 {
        return Err("ONLY SUPPORTED SUPERVISOR PAGE FLAGS ARE ACCEPTED");
    }
    if flags & NO_EXECUTE != 0 && !nx_is_enabled() {
        return Err("CPU NX PAGE PROTECTION IS NOT ENABLED");
    }
    if flags & GLOBAL != 0 && read_cr4() & CR4_PGE == 0 {
        return Err("CR4.PGE IS NOT ENABLED; GLOBAL PAGE MAPPING IS UNAVAILABLE");
    }

    let _guard = VM_LOCK.lock();
    let root_physical = read_cr3() & ADDRESS_MASK;
    let root_virtual = crate::memory::physical_to_virtual(root_physical)
        .ok_or("CANNOT ACCESS ACTIVE PML4 THROUGH HHDM")?;
    let indexes = page_indexes(virtual_address);
    let mut table = root_virtual.cast::<u64>();

    for level in 0..3 {
        let entry_ptr = unsafe { table.add(indexes[level]) };
        let mut entry = unsafe { core::ptr::read_volatile(entry_ptr) };
        if entry & PRESENT == 0 {
            let child_physical = crate::memory::allocate_zeroed_page()
                .ok_or("PMM COULD NOT ALLOCATE A PAGE-TABLE PAGE")?;
            let child_virtual = match crate::memory::physical_to_virtual(child_physical) {
                Some(pointer) => pointer,
                None => {
                    let _ = crate::memory::free_page(child_physical);
                    return Err("HHDM COULD NOT ACCESS A NEW PAGE TABLE");
                }
            };
            entry = child_physical | PRESENT | WRITABLE;
            unsafe { core::ptr::write_volatile(entry_ptr, entry); }
            TABLES_ALLOCATED.fetch_add(1, Ordering::Relaxed);
            table = child_virtual.cast::<u64>();
            continue;
        }
        if (level == 1 || level == 2) && entry & LARGE != 0 {
            return Err("CANNOT INSERT A 4 KIB PAGE INSIDE AN EXISTING LARGE PAGE");
        }
        // Never widen an existing parent entry: doing so could make sibling leaves writable.
        // A writable leaf is only possible when every existing ancestor permits writes.
        if flags & WRITABLE != 0 && entry & WRITABLE == 0 {
            return Err("PARENT PAGE-TABLE ENTRY IS READ-ONLY");
        }
        if flags & NO_EXECUTE == 0 && entry & NO_EXECUTE != 0 {
            return Err("PARENT PAGE-TABLE ENTRY DISABLES EXECUTION");
        }
        table = crate::memory::physical_to_virtual(entry & ADDRESS_MASK)
            .ok_or("CANNOT ACCESS A PAGE-TABLE LEVEL THROUGH HHDM")?.cast::<u64>();
    }

    let leaf = unsafe { table.add(indexes[3]) };
    if unsafe { core::ptr::read_volatile(leaf) } & PRESENT != 0 {
        return Err("VIRTUAL PAGE IS ALREADY MAPPED");
    }
    unsafe { core::ptr::write_volatile(leaf, physical_address | PRESENT | flags); }
    invalidate_page(virtual_address);
    MAPPED_PAGES.fetch_add(1, Ordering::Relaxed);
    Ok(())
}

/// Remove a 4 KiB leaf mapping. Intermediate page-table pages are retained for safe reuse.
pub fn unmap_page(virtual_address: u64) -> Result<u64, &'static str> {
    if !is_initialized() { return Err("VMM IS NOT INITIALIZED"); }
    if !is_canonical(virtual_address) || virtual_address % PAGE_SIZE != 0 {
        return Err("VIRTUAL ADDRESS MUST BE CANONICAL AND 4 KIB ALIGNED");
    }
    let _guard = VM_LOCK.lock();
    let root_physical = read_cr3() & ADDRESS_MASK;
    let root_virtual = crate::memory::physical_to_virtual(root_physical)
        .ok_or("CANNOT ACCESS ACTIVE PML4 THROUGH HHDM")?;
    let indexes = page_indexes(virtual_address);
    let mut table = root_virtual.cast::<u64>();
    for level in 0..3 {
        let entry = unsafe { core::ptr::read_volatile(table.add(indexes[level])) };
        if entry & PRESENT == 0 { return Err("VIRTUAL PAGE IS NOT MAPPED"); }
        if (level == 1 || level == 2) && entry & LARGE != 0 {
            return Err("UNMAPPING A SUBPAGE OF A LARGE PAGE IS NOT SUPPORTED");
        }
        table = crate::memory::physical_to_virtual(entry & ADDRESS_MASK)
            .ok_or("CANNOT ACCESS A PAGE-TABLE LEVEL THROUGH HHDM")?.cast::<u64>();
    }
    let leaf = unsafe { table.add(indexes[3]) };
    let entry = unsafe { core::ptr::read_volatile(leaf) };
    if entry & PRESENT == 0 { return Err("VIRTUAL PAGE IS NOT MAPPED"); }
    unsafe { core::ptr::write_volatile(leaf, 0); }
    invalidate_page(virtual_address);
    UNMAPPED_PAGES.fetch_add(1, Ordering::Relaxed);
    Ok(entry & ADDRESS_MASK)
}

pub fn stats() -> VmStats {
    VmStats {
        root_physical: ROOT_PHYSICAL.load(Ordering::Acquire),
        nx_enabled: nx_is_enabled(),
        mapped_pages: MAPPED_PAGES.load(Ordering::Acquire),
        unmapped_pages: UNMAPPED_PAGES.load(Ordering::Acquire),
        page_tables_allocated: TABLES_ALLOCATED.load(Ordering::Acquire),
        page_walks: WALKS.load(Ordering::Acquire),
    }
}

/// Verify CR3 access and a real translation through the Limine HHDM.
/// This test does not alter the active page tables.
pub fn self_test() -> Result<(), &'static str> {
    if !is_initialized() { return Err("VMM IS NOT INITIALIZED"); }
    let root = current_root_physical().ok_or("ACTIVE PML4 ROOT IS UNAVAILABLE")?;
    if root == 0 || root % PAGE_SIZE != 0 { return Err("ACTIVE PML4 ROOT IS MISALIGNED"); }
    let page = crate::memory::allocate_zeroed_page().ok_or("PMM COULD NOT ALLOCATE A VMM TEST PAGE")?;
    let virtual_address = match crate::memory::physical_to_virtual(page) {
        Some(pointer) => pointer as u64,
        None => {
            let _ = crate::memory::free_page(page);
            return Err("HHDM COULD NOT ACCESS THE VMM TEST PAGE");
        }
    };
    let result = translate(virtual_address);
    let freed = crate::memory::free_page(page);
    if !freed { return Err("VMM TEST PAGE COULD NOT BE RETURNED TO PMM"); }
    let translation = result.ok_or("ACTIVE PAGE TABLES DID NOT TRANSLATE THE HHDM TEST PAGE")?;
    if translation.physical_address != page { return Err("HHDM TRANSLATION RETURNED AN INCORRECT PHYSICAL ADDRESS"); }
    Ok(())
}

fn page_indexes(address: u64) -> [usize; 4] {
    [((address >> 39) & 0x1ff) as usize, ((address >> 30) & 0x1ff) as usize,
     ((address >> 21) & 0x1ff) as usize, ((address >> 12) & 0x1ff) as usize]
}

fn is_canonical(address: u64) -> bool {
    let upper = address >> 48;
    if address & (1 << 47) == 0 { upper == 0 } else { upper == 0xffff }
}

fn read_cr4() -> u64 {
    let value: u64;
    unsafe { core::arch::asm!("mov {}, cr4", out(reg) value, options(nomem, nostack, preserves_flags)); }
    value
}

fn read_cr3() -> u64 {
    let value: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) value, options(nomem, nostack, preserves_flags)); }
    value
}

fn invalidate_page(address: u64) {
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) address as usize, options(nostack, preserves_flags)); }
}

fn enable_nx_if_supported() -> bool {
    use core::arch::x86_64::__cpuid;
    let max_extended = unsafe { __cpuid(0x8000_0000).eax };
    if max_extended < 0x8000_0001 { return false; }
    let features = unsafe { __cpuid(0x8000_0001).edx };
    if features & (1 << 20) == 0 { return false; }

    let low: u32;
    let high: u32;
    unsafe {
        core::arch::asm!("rdmsr", in("ecx") 0xC000_0080u32, out("eax") low, out("edx") high, options(nomem, nostack));
        let mut efer = ((high as u64) << 32) | low as u64;
        efer |= 1 << 11; // EFER.NXE
        core::arch::asm!("wrmsr", in("ecx") 0xC000_0080u32, in("eax") efer as u32, in("edx") (efer >> 32) as u32, options(nomem, nostack));
    }
    true
}
