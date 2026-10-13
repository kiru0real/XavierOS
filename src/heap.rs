//! A small first-fit kernel heap backed by a contiguous PMM allocation.
//! The current kernel runs on one CPU with interrupts disabled. The list uses a spin lock,
//! but PMM is not SMP-safe and IRQ handlers must not allocate until locking is IRQ-safe.

use core::alloc::{GlobalAlloc, Layout};
use core::cell::UnsafeCell;
use core::mem::{align_of, size_of};
use core::ptr::{null_mut, write_bytes};
use core::sync::atomic::{AtomicBool, Ordering};

const HEAP_BYTES: usize = 1024 * 1024;
const HEAP_PAGES: usize = HEAP_BYTES / 4096;

#[repr(C)]
struct FreeBlock {
    size: usize,
    next: *mut FreeBlock,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct AllocationHeader {
    start: usize,
    size: usize,
}

struct KernelHeap {
    lock: AtomicBool,
    initialized: AtomicBool,
    head: UnsafeCell<*mut FreeBlock>,
}

unsafe impl Sync for KernelHeap {}

struct LockGuard<'a>(&'a AtomicBool);
impl Drop for LockGuard<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}

impl KernelHeap {
    const fn new() -> Self {
        Self {
            lock: AtomicBool::new(false),
            initialized: AtomicBool::new(false),
            head: UnsafeCell::new(null_mut()),
        }
    }

    fn lock(&self) -> LockGuard<'_> {
        while self.lock.compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed).is_err() {
            core::hint::spin_loop();
        }
        LockGuard(&self.lock)
    }

    unsafe fn allocate(&self, layout: Layout) -> *mut u8 {
        if !self.initialized.load(Ordering::Acquire) {
            return null_mut();
        }
        if layout.size() == 0 {
            return layout.align() as *mut u8;
        }
        let _guard = self.lock();
        unsafe { self.allocate_locked(layout) }
    }

    unsafe fn allocate_locked(&self, layout: Layout) -> *mut u8 {
        let header_size = size_of::<AllocationHeader>();
        let required_align = layout.align().max(align_of::<AllocationHeader>());
        let mut link = self.head.get();

        unsafe {
            while !(*link).is_null() {
                let block = *link;
                let start = block as usize;
                let end = match start.checked_add((*block).size) {
                    Some(value) => value,
                    None => {
                        link = &mut (*block).next;
                        continue;
                    }
                };
                let payload = match start.checked_add(header_size).and_then(|v| align_up(v, required_align)) {
                    Some(value) => value,
                    None => return null_mut(),
                };
                let payload_end = match payload.checked_add(layout.size()) {
                    Some(value) => value,
                    None => return null_mut(),
                };
                if payload_end > end {
                    link = &mut (*block).next;
                    continue;
                }

                let aligned_end = align_up(payload_end, align_of::<FreeBlock>()).unwrap_or(end);
                let can_split = aligned_end <= end && end - aligned_end >= size_of::<FreeBlock>();
                let used_size;
                if can_split {
                    let remainder = aligned_end as *mut FreeBlock;
                    (*remainder).size = end - aligned_end;
                    (*remainder).next = (*block).next;
                    *link = remainder;
                    used_size = aligned_end - start;
                } else {
                    *link = (*block).next;
                    used_size = end - start;
                }

                let header_ptr = (payload - header_size) as *mut AllocationHeader;
                header_ptr.write(AllocationHeader { start, size: used_size });
                return payload as *mut u8;
            }
        }
        null_mut()
    }

    unsafe fn deallocate(&self, ptr: *mut u8) {
        if ptr.is_null() || !self.initialized.load(Ordering::Acquire) {
            return;
        }
        let _guard = self.lock();
        let header_ptr = unsafe { (ptr as usize - size_of::<AllocationHeader>()) as *mut AllocationHeader };
        let header = unsafe { header_ptr.read() };
        if header.start == 0 || header.size < size_of::<FreeBlock>() {
            return;
        }
        let start_ptr = header.start as *mut FreeBlock;
        unsafe {
            (*start_ptr).size = header.size;
            (*start_ptr).next = null_mut();

            let mut previous: *mut FreeBlock = null_mut();
            let mut current = *self.head.get();
            while !current.is_null() && (current as usize) < header.start {
                previous = current;
                current = (*current).next;
            }
            (*start_ptr).next = current;
            if previous.is_null() {
                *self.head.get() = start_ptr;
            } else {
                (*previous).next = start_ptr;
            }

            // Merge with the following free block, then with the previous one.
            if !current.is_null() && header.start.saturating_add(header.size) == current as usize {
                (*start_ptr).size = (*start_ptr).size.saturating_add((*current).size);
                (*start_ptr).next = (*current).next;
            }
            if !previous.is_null()
                && (previous as usize).saturating_add((*previous).size) == start_ptr as usize
            {
                (*previous).size = (*previous).size.saturating_add((*start_ptr).size);
                (*previous).next = (*start_ptr).next;
            }
        }
    }

    fn stats(&self) -> Option<HeapStats> {
        if !self.initialized.load(Ordering::Acquire) {
            return None;
        }
        let _guard = self.lock();
        let mut free_bytes = 0usize;
        unsafe {
            let mut block = *self.head.get();
            while !block.is_null() {
                free_bytes = free_bytes.saturating_add((*block).size);
                block = (*block).next;
            }
        }
        Some(HeapStats { capacity_bytes: HEAP_BYTES, free_bytes })
    }
}

#[global_allocator]
static HEAP: KernelHeap = KernelHeap::new();

#[derive(Clone, Copy)]
pub struct HeapStats {
    pub capacity_bytes: usize,
    pub free_bytes: usize,
}

unsafe impl GlobalAlloc for KernelHeap {
    unsafe fn alloc(&self, layout: Layout) -> *mut u8 {
        unsafe { self.allocate(layout) }
    }

    unsafe fn dealloc(&self, ptr: *mut u8, _layout: Layout) {
        unsafe { self.deallocate(ptr) }
    }
}

/// Reserve and initialize a 1 MiB heap from a contiguous run of PMM pages.
pub fn initialize() -> Result<HeapStats, &'static str> {
    if HEAP.initialized.load(Ordering::Acquire) {
        return Err("kernel heap is already initialized");
    }
    let physical = crate::memory::allocate_contiguous_pages(HEAP_PAGES)
        .ok_or("PMM could not provide 1 MiB of contiguous pages for the heap")?;
    let virtual_base = match crate::memory::physical_to_virtual(physical) {
        Some(ptr) => ptr,
        None => {
            for offset in 0..HEAP_PAGES {
                let _ = crate::memory::free_page(physical + (offset as u64) * 4096);
            }
            return Err("HHDM could not map the heap allocation");
        }
    };
    if (virtual_base as usize) % align_of::<FreeBlock>() != 0 {
        for offset in 0..HEAP_PAGES {
            let _ = crate::memory::free_page(physical + (offset as u64) * 4096);
        }
        return Err("heap base alignment is invalid");
    }

    unsafe {
        write_bytes(virtual_base, 0, HEAP_BYTES);
        let initial = virtual_base.cast::<FreeBlock>();
        (*initial).size = HEAP_BYTES;
        (*initial).next = null_mut();
        *HEAP.head.get() = initial;
    }
    HEAP.initialized.store(true, Ordering::Release);
    crate::serial_write("Kernel heap initialized: 1024 KiB from 256 contiguous PMM pages.\r\n");
    HEAP.stats().ok_or("kernel heap did not publish initialization")
}

pub fn is_initialized() -> bool {
    HEAP.initialized.load(Ordering::Acquire)
}

pub fn stats() -> Option<HeapStats> {
    HEAP.stats()
}

pub fn self_test() -> Result<(), &'static str> {
    if !is_initialized() {
        return Err("kernel heap is not initialized");
    }
    let before = stats().ok_or("kernel heap statistics are unavailable")?.free_bytes;
    let layout_a = Layout::from_size_align(37, 16).map_err(|_| "invalid test layout A")?;
    let layout_b = Layout::from_size_align(333, 64).map_err(|_| "invalid test layout B")?;
    let a = unsafe { HEAP.alloc(layout_a) };
    if a.is_null() {
        return Err("heap failed to allocate test block A");
    }
    let b = unsafe { HEAP.alloc(layout_b) };
    if b.is_null() {
        unsafe { HEAP.dealloc(a, layout_a); }
        return Err("heap failed to allocate test block B");
    }

    let valid_alignment = (a as usize) % layout_a.align() == 0 && (b as usize) % layout_b.align() == 0;
    unsafe {
        write_bytes(a, 0xA5, layout_a.size());
        write_bytes(b, 0x5A, layout_b.size());
    }
    let data_valid = unsafe { *a == 0xA5 && *a.add(layout_a.size() - 1) == 0xA5
        && *b == 0x5A && *b.add(layout_b.size() - 1) == 0x5A };
    unsafe {
        HEAP.dealloc(b, layout_b);
        HEAP.dealloc(a, layout_a);
    }
    let after = stats().ok_or("kernel heap statistics are unavailable after test")?.free_bytes;
    if !valid_alignment {
        return Err("heap returned a misaligned allocation");
    }
    if !data_valid {
        return Err("heap allocation data verification failed");
    }
    if before != after {
        return Err("heap free-list size did not recover after allocate/free test");
    }
    Ok(())
}

fn align_up(value: usize, align: usize) -> Option<usize> {
    if align == 0 || !align.is_power_of_two() {
        return None;
    }
    value.checked_add(align - 1).map(|value| value & !(align - 1))
}
