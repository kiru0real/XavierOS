//! Limine memory-map parsing and a small physical page allocator.
//!
//! The first implementation intentionally manages only physical addresses below 4 GiB.
//! Limine's HHDM is used to access and zero allocated physical pages.
//! The allocator is single-core and is not SMP-safe yet.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};

const LIMINE_COMMON_MAGIC: [u64; 2] = [
    0xC7B1_DD30_DF4C_8B88,
    0x0A82_E883_A194_F07B,
];
const LIMINE_MEMMAP_REQUEST_ID: [u64; 2] = [
    0x67CF_3D9D_378A_806F,
    0xE304_ACDF_C50C_3C62,
];
const LIMINE_HHDM_REQUEST_ID: [u64; 2] = [
    0x48DC_F1CB_8AD2_B852,
    0x6398_4E95_9A98_244B,
];

const LIMINE_MEMMAP_USABLE: u64 = 0;
const PAGE_SIZE: u64 = 4096;
const MAX_MANAGED_BYTES: u64 = 4 * 1024 * 1024 * 1024;
const MAX_MANAGED_PAGES: usize = (MAX_MANAGED_BYTES / PAGE_SIZE) as usize;
const BITMAP_WORDS: usize = MAX_MANAGED_PAGES / 64;
const MAX_MEMORY_MAP_ENTRIES: u64 = 4096;

#[repr(C)]
struct HhdmResponse {
    revision: u64,
    offset: u64,
}

#[repr(C)]
struct HhdmRequest {
    magic: [u64; 2],
    id: [u64; 2],
    revision: u64,
    response: *mut HhdmResponse,
}

#[repr(C)]
struct MemoryMapRequest {
    magic: [u64; 2],
    id: [u64; 2],
    revision: u64,
    response: *mut MemoryMapResponse,
}

#[repr(C)]
struct MemoryMapResponse {
    revision: u64,
    entry_count: u64,
    entries: *mut *mut MemoryMapEntry,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct MemoryMapEntry {
    base: u64,
    length: u64,
    entry_type: u64,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static mut HHDM_REQUEST: HhdmRequest = HhdmRequest {
    magic: LIMINE_COMMON_MAGIC,
    id: LIMINE_HHDM_REQUEST_ID,
    revision: 0,
    response: core::ptr::null_mut(),
};

#[used]
#[unsafe(link_section = ".limine_requests")]
static mut MEMORY_MAP_REQUEST: MemoryMapRequest = MemoryMapRequest {
    magic: LIMINE_COMMON_MAGIC,
    id: LIMINE_MEMMAP_REQUEST_ID,
    revision: 0,
    response: core::ptr::null_mut(),
};

// Zero-initialized BSS arrays. initialize() marks every page used first, then releases
// only full pages explicitly declared usable by Limine. Each bitmap covers 4 GiB / 4 KiB.
static mut USED_BITMAP: [u64; BITMAP_WORDS] = [0; BITMAP_WORDS];
static mut USABLE_BITMAP: [u64; BITMAP_WORDS] = [0; BITMAP_WORDS];
static mut FREE_PAGE_COUNT: usize = 0;
static mut HHDM_OFFSET: u64 = 0;
static mut INITIALIZED: bool = false;

#[derive(Clone, Copy)]
pub struct MemoryStats {
    pub entry_count: usize,
    pub total_usable_bytes: u64,
    pub managed_pages: usize,
    pub free_pages: usize,
    pub hhdm_offset: u64,
}

/// Read Limine's memory map and initialize the page bitmaps.
pub fn initialize() -> Result<MemoryStats, &'static str> {
    if unsafe { read_volatile(addr_of!(INITIALIZED)) } {
        return Err("physical memory manager was already initialized");
    }

    let hhdm_response = unsafe { read_volatile(addr_of!(HHDM_REQUEST.response)) };
    if hhdm_response.is_null() {
        return Err("Limine did not return an HHDM response");
    }
    let hhdm_offset = unsafe { read_volatile(addr_of!((*hhdm_response).offset)) };
    if hhdm_offset == 0 {
        return Err("Limine returned an invalid zero HHDM offset");
    }

    let response = unsafe { read_volatile(addr_of!(MEMORY_MAP_REQUEST.response)) };
    if response.is_null() {
        return Err("Limine did not return a memory-map response");
    }

    let entry_count = unsafe { read_volatile(addr_of!((*response).entry_count)) };
    let entries = unsafe { read_volatile(addr_of!((*response).entries)) };
    if entry_count == 0 {
        return Err("Limine returned an empty memory map");
    }
    if entry_count > MAX_MEMORY_MAP_ENTRIES {
        return Err("memory map has an unexpectedly large entry count");
    }
    if entries.is_null() {
        return Err("Limine returned a null memory-map entry list");
    }

    // Validate all pointers before changing the bitmaps.
    for index in 0..entry_count as usize {
        let entry_ptr = unsafe { read_volatile(entries.add(index)) };
        if entry_ptr.is_null() {
            return Err("Limine returned a null memory-map entry");
        }
    }

    let used_ptr = unsafe { addr_of_mut!(USED_BITMAP).cast::<u64>() };
    let usable_ptr = unsafe { addr_of_mut!(USABLE_BITMAP).cast::<u64>() };
    for word in 0..BITMAP_WORDS {
        unsafe {
            write_volatile(used_ptr.add(word), u64::MAX);
            write_volatile(usable_ptr.add(word), 0);
        }
    }

    let mut total_usable_bytes = 0u64;
    let mut managed_pages = 0usize;

    for index in 0..entry_count as usize {
        let entry_ptr = unsafe { read_volatile(entries.add(index)) };
        let entry = unsafe { read_volatile(entry_ptr) };
        if entry.entry_type != LIMINE_MEMMAP_USABLE {
            continue;
        }

        total_usable_bytes = total_usable_bytes.saturating_add(entry.length);

        // This first PMM manages only the lower 4 GiB, and only complete pages.
        if entry.base >= MAX_MANAGED_BYTES || entry.length == 0 {
            continue;
        }
        let end_address = entry.base.saturating_add(entry.length).min(MAX_MANAGED_BYTES);
        let start_address = entry
            .base
            .saturating_add(PAGE_SIZE - 1)
            & !(PAGE_SIZE - 1);
        let start_page = ((start_address / PAGE_SIZE) as usize).max(1); // Keep page zero reserved.
        let end_page = (end_address & !(PAGE_SIZE - 1)) / PAGE_SIZE;
        if (end_page as usize) <= start_page {
            continue;
        }

        managed_pages = managed_pages.saturating_add(mark_usable_pages(
            used_ptr,
            usable_ptr,
            start_page,
            end_page as usize,
        ));
    }

    if managed_pages == 0 {
        return Err("no usable physical pages were found below 4 GiB");
    }

    unsafe {
        write_volatile(addr_of_mut!(FREE_PAGE_COUNT), managed_pages);
        write_volatile(addr_of_mut!(HHDM_OFFSET), hhdm_offset);
        // Publish INITIALIZED last so public helpers cannot observe partial state.
        write_volatile(addr_of_mut!(INITIALIZED), true);
    }

    Ok(MemoryStats {
        entry_count: entry_count as usize,
        total_usable_bytes,
        managed_pages,
        free_pages: managed_pages,
        hhdm_offset,
    })
}

fn mark_usable_pages(
    used_bitmap: *mut u64,
    usable_bitmap: *mut u64,
    start_page: usize,
    end_page: usize,
) -> usize {
    let mut added = 0usize;
    for page in start_page..end_page.min(MAX_MANAGED_PAGES) {
        let word_index = page / 64;
        let mask = 1u64 << (page % 64);
        unsafe {
            let usable_word_ptr = usable_bitmap.add(word_index);
            let usable_word = read_volatile(usable_word_ptr);
            // Limine's map entries should not overlap, but do not double-count if they do.
            if usable_word & mask != 0 {
                continue;
            }
            write_volatile(usable_word_ptr, usable_word | mask);

            let used_word_ptr = used_bitmap.add(word_index);
            let used_word = read_volatile(used_word_ptr);
            write_volatile(used_word_ptr, used_word & !mask);
        }
        added = added.saturating_add(1);
    }
    added
}

/// Translate a physical address below 4 GiB into its Limine HHDM virtual address.
///
/// This only performs checked address arithmetic. The caller must ensure the supplied
/// physical address refers to RAM covered by Limine's HHDM; PMM-allocated pages satisfy
/// that requirement in this implementation. Dereferencing the returned pointer is unsafe.
pub fn physical_to_virtual(physical_address: u64) -> Option<*mut u8> {
    if !unsafe { read_volatile(addr_of!(INITIALIZED)) }
        || physical_address >= MAX_MANAGED_BYTES
    {
        return None;
    }

    let offset = unsafe { read_volatile(addr_of!(HHDM_OFFSET)) };
    if offset == 0 {
        return None;
    }

    offset.checked_add(physical_address).map(|address| address as *mut u8)
}


/// Map a physical MMIO range into Limine's higher-half direct-map address space.
///
/// Limine's HHDM offset is a linear address calculation, not a guarantee that
/// every reserved physical range (including PCI BARs) is actually mapped. Device
/// registers must therefore be mapped explicitly before they are dereferenced.
/// The function uses the active four-level x86_64 page tables and marks mappings
/// uncached (PWT|PCD). This kernel currently supports physical MMIO below 4 GiB.
pub fn map_mmio_region(physical_start: u64, length: u64) -> Result<(), &'static str> {
    if !unsafe { read_volatile(addr_of!(INITIALIZED)) } {
        return Err("physical memory manager is not initialized");
    }
    if length == 0 {
        return Err("MMIO mapping length is zero");
    }
    let end = physical_start.checked_add(length).ok_or("MMIO range overflow")?;
    if physical_start >= MAX_MANAGED_BYTES || end > MAX_MANAGED_BYTES {
        return Err("MMIO range must be below 4 GiB");
    }

    let start_page = physical_start & !(PAGE_SIZE - 1);
    let end_page = end.checked_add(PAGE_SIZE - 1).ok_or("MMIO page range overflow")? & !(PAGE_SIZE - 1);
    let hhdm_offset = unsafe { read_volatile(addr_of!(HHDM_OFFSET)) };
    if hhdm_offset == 0 {
        return Err("HHDM is unavailable");
    }

    let mut physical = start_page;
    while physical < end_page {
        let virtual_address = hhdm_offset.checked_add(physical).ok_or("MMIO virtual address overflow")?;
        unsafe { map_one_hhdm_mmio_page(virtual_address, physical)?; }
        unsafe { asm_invlpg(virtual_address); }
        physical += PAGE_SIZE;
    }
    Ok(())
}

unsafe fn map_one_hhdm_mmio_page(virtual_address: u64, physical_address: u64) -> Result<(), &'static str> {
    const ENTRY_PRESENT: u64 = 1 << 0;
    const ENTRY_WRITABLE: u64 = 1 << 1;
    const ENTRY_PWT: u64 = 1 << 3;
    const ENTRY_PCD: u64 = 1 << 4;
    const ENTRY_LARGE: u64 = 1 << 7;
    const ADDRESS_MASK: u64 = 0x000F_FFFF_FFFF_F000;
    const PDPT_LARGE_MASK: u64 = 0x000F_FFFF_C000_0000;
    const PD_LARGE_MASK: u64 = 0x000F_FFFF_FFE0_0000;
    let cache_flags = ENTRY_PWT | ENTRY_PCD;

    let cr3: u64;
    unsafe { core::arch::asm!("mov {}, cr3", out(reg) cr3, options(nomem, nostack, preserves_flags)); }
    let root_physical = cr3 & ADDRESS_MASK;
    let root_virtual = physical_to_virtual(root_physical).ok_or("cannot access active PML4 through HHDM")?;
    let indexes = [
        ((virtual_address >> 39) & 0x1FF) as usize,
        ((virtual_address >> 30) & 0x1FF) as usize,
        ((virtual_address >> 21) & 0x1FF) as usize,
        ((virtual_address >> 12) & 0x1FF) as usize,
    ];

    let mut table = root_virtual.cast::<u64>();
    for level in 0..3 {
        let entry_ptr = unsafe { table.add(indexes[level]) };
        let mut entry = unsafe { read_volatile(entry_ptr) };
        if entry & ENTRY_PRESENT == 0 {
            let child_physical = allocate_page().ok_or("out of physical pages for MMIO page tables")?;
            let child_virtual = physical_to_virtual(child_physical).ok_or("cannot access new MMIO page table")?;
            unsafe { core::ptr::write_bytes(child_virtual, 0, PAGE_SIZE as usize); }
            entry = child_physical | ENTRY_PRESENT | ENTRY_WRITABLE;
            unsafe { write_volatile(entry_ptr, entry); }
            table = child_virtual.cast::<u64>();
            continue;
        }

        // If an existing 1 GiB or 2 MiB mapping already covers the HHDM address,
        // verify that it maps the requested physical range and make it uncached.
        if level == 1 && entry & ENTRY_LARGE != 0 {
            if (entry & PDPT_LARGE_MASK) != (physical_address & !((1u64 << 30) - 1)) {
                return Err("existing 1 GiB HHDM mapping does not match MMIO physical address");
            }
            unsafe { write_volatile(entry_ptr, entry | cache_flags | ENTRY_WRITABLE); }
            return Ok(());
        }
        if level == 2 && entry & ENTRY_LARGE != 0 {
            if (entry & PD_LARGE_MASK) != (physical_address & !((1u64 << 21) - 1)) {
                return Err("existing 2 MiB HHDM mapping does not match MMIO physical address");
            }
            unsafe { write_volatile(entry_ptr, entry | cache_flags | ENTRY_WRITABLE); }
            return Ok(());
        }
        table = physical_to_virtual(entry & ADDRESS_MASK).ok_or("cannot access MMIO page-table level through HHDM")?.cast::<u64>();
    }

    let leaf_ptr = unsafe { table.add(indexes[3]) };
    let old_entry = unsafe { read_volatile(leaf_ptr) };
    if old_entry & ENTRY_PRESENT != 0 {
        if old_entry & ADDRESS_MASK != physical_address {
            return Err("HHDM PTE already maps a different physical page");
        }
        unsafe { write_volatile(leaf_ptr, old_entry | ENTRY_WRITABLE | cache_flags); }
    } else {
        unsafe { write_volatile(leaf_ptr, physical_address | ENTRY_PRESENT | ENTRY_WRITABLE | cache_flags); }
    }
    Ok(())
}

unsafe fn asm_invlpg(virtual_address: u64) {
    unsafe { core::arch::asm!("invlpg [{}]", in(reg) virtual_address as usize, options(nostack, preserves_flags)); }
}

/// Allocate one physical 4 KiB page. The returned value is a physical address.
pub fn allocate_page() -> Option<u64> {
    if !unsafe { read_volatile(addr_of!(INITIALIZED)) } || free_page_count() == 0 {
        return None;
    }

    let used_ptr = unsafe { addr_of_mut!(USED_BITMAP).cast::<u64>() };
    let usable_ptr = unsafe { addr_of_mut!(USABLE_BITMAP).cast::<u64>() };

    for word_index in 0..BITMAP_WORDS {
        let used_word_ptr = unsafe { used_ptr.add(word_index) };
        let mut used_word = unsafe { read_volatile(used_word_ptr) };
        let mut candidates = !used_word;

        while candidates != 0 {
            let bit = candidates.trailing_zeros() as usize;
            let mask = 1u64 << bit;
            let page_index = word_index * 64 + bit;
            let usable_word = unsafe { read_volatile(usable_ptr.add(word_index)) };

            if usable_word & mask == 0 {
                // Defensive repair: never return a page not marked as usable.
                used_word |= mask;
                unsafe { write_volatile(used_word_ptr, used_word) };
                candidates &= !mask;
                continue;
            }

            used_word |= mask;
            unsafe { write_volatile(used_word_ptr, used_word) };
            let free = free_page_count();
            if free == 0 {
                return None;
            }
            unsafe { write_volatile(addr_of_mut!(FREE_PAGE_COUNT), free - 1) };
            return Some((page_index as u64) * PAGE_SIZE);
        }
    }

    None
}

/// Allocate a physically contiguous run of pages. The returned address is physical.
/// This is needed for subsystems (such as the first kernel heap) that require one
/// contiguous virtual interval through the linear HHDM mapping.
pub fn allocate_contiguous_pages(count: usize) -> Option<u64> {
    if count == 0
        || count > MAX_MANAGED_PAGES
        || !unsafe { read_volatile(addr_of!(INITIALIZED)) }
        || free_page_count() < count
    {
        return None;
    }

    let used_ptr = unsafe { addr_of_mut!(USED_BITMAP).cast::<u64>() };
    let usable_ptr = unsafe { addr_of_mut!(USABLE_BITMAP).cast::<u64>() };
    let mut run_start = 0usize;
    let mut run_length = 0usize;

    // Page zero is deliberately reserved. A run resets at every reserved or busy page.
    for page in 1..MAX_MANAGED_PAGES {
        let word_index = page / 64;
        let mask = 1u64 << (page % 64);
        let is_usable = unsafe { read_volatile(usable_ptr.add(word_index)) } & mask != 0;
        let is_used = unsafe { read_volatile(used_ptr.add(word_index)) } & mask != 0;
        if is_usable && !is_used {
            if run_length == 0 {
                run_start = page;
            }
            run_length += 1;
            if run_length == count {
                for allocated_page in run_start..run_start + count {
                    let allocated_word = allocated_page / 64;
                    let allocated_mask = 1u64 << (allocated_page % 64);
                    let used_value = unsafe { read_volatile(used_ptr.add(allocated_word)) };
                    unsafe { write_volatile(used_ptr.add(allocated_word), used_value | allocated_mask) };
                }
                let free = free_page_count();
                unsafe { write_volatile(addr_of_mut!(FREE_PAGE_COUNT), free.saturating_sub(count)) };
                return Some((run_start as u64) * PAGE_SIZE);
            }
        } else {
            run_length = 0;
        }
    }
    None
}

/// Allocate one page and clear its full 4096-byte contents through the HHDM.
/// If the HHDM translation is unavailable, the page is returned to the allocator.
pub fn allocate_zeroed_page() -> Option<u64> {
    let physical_address = allocate_page()?;
    let virtual_address = match physical_to_virtual(physical_address) {
        Some(address) => address,
        None => {
            let _ = free_page(physical_address);
            return None;
        }
    };

    unsafe {
        core::ptr::write_bytes(virtual_address, 0, PAGE_SIZE as usize);
    }
    Some(physical_address)
}

/// Release a page previously returned by allocate_page(). Returns false for invalid,
/// reserved, unaligned, out-of-range, or already-free addresses.
pub fn free_page(physical_address: u64) -> bool {
    if !unsafe { read_volatile(addr_of!(INITIALIZED)) }
        || physical_address == 0
        || physical_address % PAGE_SIZE != 0
        || physical_address >= MAX_MANAGED_BYTES
    {
        return false;
    }

    let page_index = (physical_address / PAGE_SIZE) as usize;
    let word_index = page_index / 64;
    let mask = 1u64 << (page_index % 64);
    let used_ptr = unsafe { addr_of_mut!(USED_BITMAP).cast::<u64>() };
    let usable_ptr = unsafe { addr_of_mut!(USABLE_BITMAP).cast::<u64>() };
    let used_word_ptr = unsafe { used_ptr.add(word_index) };
    let usable_word = unsafe { read_volatile(usable_ptr.add(word_index)) };
    let used_word = unsafe { read_volatile(used_word_ptr) };

    if usable_word & mask == 0 || used_word & mask == 0 {
        return false;
    }

    unsafe {
        write_volatile(used_word_ptr, used_word & !mask);
        let free = read_volatile(addr_of!(FREE_PAGE_COUNT));
        write_volatile(addr_of_mut!(FREE_PAGE_COUNT), free.saturating_add(1));
    }
    true
}

pub fn free_page_count() -> usize {
    unsafe { read_volatile(addr_of!(FREE_PAGE_COUNT)) }
}

/// Basic allocate/free/double-free test. It restores the allocator to its original count.
pub fn self_test() -> Result<(), &'static str> {
    if !unsafe { read_volatile(addr_of!(INITIALIZED)) } {
        return Err("PMM is not initialized");
    }

    let free_before = free_page_count();
    let first = allocate_page().ok_or("PMM could not allocate the first page")?;
    let second = match allocate_page() {
        Some(page) => page,
        None => {
            let _ = free_page(first);
            return Err("PMM could not allocate a second page");
        }
    };

    if first == second || first % PAGE_SIZE != 0 || second % PAGE_SIZE != 0 {
        let _ = free_page(first);
        let _ = free_page(second);
        return Err("PMM returned duplicate or unaligned page addresses");
    }
    if !free_page(first) {
        let _ = free_page(second);
        return Err("PMM failed to free an allocated page");
    }
    if free_page(first) {
        let _ = free_page(second);
        return Err("PMM incorrectly accepted a double free");
    }
    if !free_page(second) {
        return Err("PMM failed to free the second allocated page");
    }
    if free_page_count() != free_before {
        return Err("PMM page count changed after self-test");
    }

    // Verify that HHDM translation reaches writable RAM and that zeroed allocation
    // really clears the entire page. Use volatile reads/writes to keep the test observable.
    let zeroed_page = allocate_zeroed_page().ok_or("HHDM zeroed-page allocation failed")?;
    let virtual_address = match physical_to_virtual(zeroed_page) {
        Some(address) => address,
        None => {
            let _ = free_page(zeroed_page);
            return Err("HHDM could not translate an allocated page");
        }
    };

    for offset in 0..PAGE_SIZE as usize {
        if unsafe { read_volatile(virtual_address.add(offset)) } != 0 {
            let _ = free_page(zeroed_page);
            return Err("zeroed-page allocation left non-zero bytes");
        }
    }

    let first_pattern = 0x5841_5649_4552_4F53u64;
    let last_pattern = 0x504D_4D48_4844_4D21u64;
    unsafe {
        write_volatile(virtual_address.cast::<u64>(), first_pattern);
        write_volatile(
            virtual_address.add(PAGE_SIZE as usize - core::mem::size_of::<u64>()).cast::<u64>(),
            last_pattern,
        );
    }
    let first_read = unsafe { read_volatile(virtual_address.cast::<u64>()) };
    let last_read = unsafe {
        read_volatile(
            virtual_address.add(PAGE_SIZE as usize - core::mem::size_of::<u64>()).cast::<u64>(),
        )
    };
    if first_read != first_pattern || last_read != last_pattern {
        let _ = free_page(zeroed_page);
        return Err("HHDM read/write verification failed");
    }
    if !free_page(zeroed_page) {
        return Err("failed to free the HHDM test page");
    }
    if free_page_count() != free_before {
        return Err("PMM page count changed after HHDM self-test");
    }

    Ok(())
}
