//! Minimal modern VirtIO PCI block driver using a single polled split virtqueue.
//!
//! Supported transport: modern PCI common/notify/device capabilities (VirtIO 1.x),
//! one queue, one sector per synchronous request. Interrupt delivery, multiqueue,
//! packed rings, indirect descriptors, discard/write-zeroes and flush negotiation
//! are intentionally not implemented. ATA PIO remains the fallback backend.

use crate::memory;
use crate::pci::{self, BarKind, PciDevice};
use crate::sync::SpinLock;
use core::ptr::{read_volatile, write_volatile};
use core::sync::atomic::{compiler_fence, AtomicBool, Ordering, fence};

const VIRTIO_VENDOR: u16 = 0x1AF4;
const VIRTIO_BLK_MODERN: u16 = 0x1042;
const PCI_CAP_VENDOR_SPECIFIC: u8 = 0x09;
const CAP_COMMON_CFG: u8 = 1;
const CAP_NOTIFY_CFG: u8 = 2;
const CAP_DEVICE_CFG: u8 = 4;
const PAGE_SIZE: usize = 4096;
const DMA_PAGES: usize = 4;
const QUEUE_MAX: u16 = 8;
const REQUEST_HEADER_OFFSET: usize = PAGE_SIZE * 3;
const REQUEST_DATA_OFFSET: usize = REQUEST_HEADER_OFFSET + 64;
const REQUEST_STATUS_OFFSET: usize = REQUEST_DATA_OFFSET + 512;
const STATUS_ACKNOWLEDGE: u8 = 1;
const STATUS_DRIVER: u8 = 2;
const STATUS_DRIVER_OK: u8 = 4;
const STATUS_FEATURES_OK: u8 = 8;
const STATUS_FAILED: u8 = 128;
const FEATURE_VERSION_1: u32 = 1;
const DESC_F_NEXT: u16 = 1;
const DESC_F_WRITE: u16 = 2;
const BLOCK_T_IN: u32 = 0;
const BLOCK_T_OUT: u32 = 1;
const VIRTIO_BLK_S_OK: u8 = 0;
const POLL_LIMIT: usize = 50_000_000;

static READY: AtomicBool = AtomicBool::new(false);
static STATE: SpinLock<DriverState> = SpinLock::new(DriverState::new());

#[derive(Clone, Copy)]
struct DriverState {
    ready: bool,
    sectors: u64,
    dma_physical: u64,
    dma_virtual: usize,
    common: usize,
    notify: usize,
    queue_size: u16,
    available_index: u16,
}

impl DriverState {
    const fn new() -> Self {
        Self { ready: false, sectors: 0, dma_physical: 0, dma_virtual: 0,
            common: 0, notify: 0, queue_size: 0, available_index: 0 }
    }
}

#[derive(Clone, Copy)]
struct VirtioCapability {
    bar: u8,
    offset: u32,
    length: u32,
}

#[derive(Clone, Copy)]
struct VirtioCaps {
    common: Option<VirtioCapability>,
    notify: Option<VirtioCapability>,
    device: Option<VirtioCapability>,
    notify_multiplier: u32,
}

pub fn is_ready() -> bool { READY.load(Ordering::Acquire) }
pub fn sector_count() -> u64 { STATE.lock().sectors }
pub fn status_text() -> &'static str { if is_ready() { "READY (MODERN PCI)" } else { "NOT READY" } }

pub fn initialize() -> Result<u64, &'static str> {
    READY.store(false, Ordering::Release);
    if !pci::is_initialized() { pci::initialize()?; }
    let device = match pci::find(VIRTIO_VENDOR, VIRTIO_BLK_MODERN) {
        Some(value) => value,
        None => return Err("NO MODERN VIRTIO-BLK PCI DEVICE FOUND"),
    };
    let caps = parse_capabilities(device)?;
    let common_cap = caps.common.ok_or("VIRTIO-BLK COMMON CONFIG CAPABILITY IS MISSING")?;
    let notify_cap = caps.notify.ok_or("VIRTIO-BLK NOTIFY CAPABILITY IS MISSING")?;
    let device_cap = caps.device.ok_or("VIRTIO-BLK DEVICE CONFIG CAPABILITY IS MISSING")?;
    // Copy the multiplier before validating it; keep the parsed capability value
    // available for the later queue-notification address calculation.
    let notify_multiplier = caps.notify_multiplier;
    if common_cap.length < 56 || device_cap.length < 8 || notify_cap.length < 2 {
        return Err("VIRTIO-BLK PCI CAPABILITY WINDOW IS TOO SMALL");
    }
    // The block driver negotiates VERSION_1 only, not NOTIFICATION_DATA.
    // Virtio PCI therefore requires a 2-byte notification offset and a
    // zero or even power-of-two notification multiplier.
    if notify_cap.offset & 0x01 != 0 {
        return Err("VIRTIO-BLK NOTIFY CAPABILITY OFFSET IS NOT 2-BYTE-ALIGNED");
    }
    if notify_multiplier != 0
        && (notify_multiplier & 1 != 0 || !notify_multiplier.is_power_of_two())
    {
        return Err("VIRTIO-BLK NOTIFY MULTIPLIER IS NOT ZERO OR AN EVEN POWER OF TWO");
    }
    if device_cap.offset & 0x03 != 0 {
        return Err("VIRTIO-BLK DEVICE CONFIG OFFSET IS NOT 4-BYTE-ALIGNED");
    }

    let common = map_capability(device, common_cap)?;
    let notify_base = map_capability(device, notify_cap)?;
    let device_config = map_capability(device, device_cap)?;
    if notify_multiplier > 0x0100_0000 { return Err("VIRTIO-BLK NOTIFY MULTIPLIER IS UNREASONABLE"); }
    pci::enable_memory_and_bus_mastering(device);

    unsafe { mmio_write_u8(common, 0x14, 0); }
    let mut reset_complete = false;
    for _ in 0..1_000_000 {
        if unsafe { mmio_read_u8(common, 0x14) } == 0 {
            reset_complete = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !reset_complete { return Err("VIRTIO-BLK DEVICE DID NOT COMPLETE RESET"); }
    unsafe { mmio_write_u8(common, 0x14, STATUS_ACKNOWLEDGE); }
    unsafe { mmio_write_u8(common, 0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER); }

    // Negotiate only VIRTIO_F_VERSION_1. No optional block features are assumed.
    unsafe {
        mmio_write_u32(common, 0x00, 0);
        let _features_low = mmio_read_u32(common, 0x04);
        mmio_write_u32(common, 0x00, 1);
        let features_high = mmio_read_u32(common, 0x04);
        if features_high & FEATURE_VERSION_1 == 0 {
            mmio_write_u8(common, 0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FAILED);
            return Err("VIRTIO-BLK DOES NOT OFFER VIRTIO_F_VERSION_1");
        }
        mmio_write_u32(common, 0x08, 0);
        mmio_write_u32(common, 0x0C, 0);
        mmio_write_u32(common, 0x08, 1);
        mmio_write_u32(common, 0x0C, FEATURE_VERSION_1);
        mmio_write_u16(common, 0x10, u16::MAX); // No MSI-X vector assigned.
        mmio_write_u8(common, 0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK);
        if mmio_read_u8(common, 0x14) & STATUS_FEATURES_OK == 0 {
            mmio_write_u8(common, 0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FAILED);
            return Err("VIRTIO-BLK REJECTED THE NEGOTIATED FEATURES");
        }
    }

    unsafe { mmio_write_u16(common, 0x16, 0); } // queue_select = 0
    let queue_count = unsafe { mmio_read_u16(common, 0x12) };
    if queue_count == 0 { return Err("VIRTIO-BLK EXPOSES NO VIRTQUEUES"); }
    let maximum_queue_size = unsafe { mmio_read_u16(common, 0x18) };
    let queue_enabled = unsafe { mmio_read_u16(common, 0x1C) };
    if queue_enabled != 0 { return Err("VIRTIO-BLK QUEUE WAS ALREADY ENABLED AFTER RESET"); }
    let queue_size = if maximum_queue_size >= QUEUE_MAX { QUEUE_MAX }
        else if maximum_queue_size >= 4 { 4 }
        else { return Err("VIRTIO-BLK QUEUE IS TOO SMALL FOR A SECTOR REQUEST"); };

    let dma_physical = memory::allocate_contiguous_pages(DMA_PAGES)
        .ok_or("NOT ENOUGH CONTIGUOUS DMA MEMORY FOR VIRTIO-BLK")?;
    let dma_virtual = memory::physical_to_virtual(dma_physical)
        .ok_or("VIRTIO-BLK DMA MEMORY IS NOT HHDM-ACCESSIBLE")?;
    unsafe { core::ptr::write_bytes(dma_virtual, 0, DMA_PAGES * PAGE_SIZE); }

    // Queue structures occupy separate page-aligned regions for simple DMA layout.
    let desc_physical = dma_physical;
    let avail_physical = dma_physical + PAGE_SIZE as u64;
    let used_physical = dma_physical + (PAGE_SIZE * 2) as u64;
    let queue_avail = unsafe { dma_virtual.add(PAGE_SIZE) };
    let queue_used = unsafe { dma_virtual.add(PAGE_SIZE * 2) };
    unsafe {
        mmio_write_u16(common, 0x18, queue_size);
        mmio_write_u16(common, 0x1A, u16::MAX); // queue_msix_vector
        mmio_write_u64(common, 0x20, desc_physical);
        mmio_write_u64(common, 0x28, avail_physical);
        mmio_write_u64(common, 0x30, used_physical);
        mmio_write_u16(common, 0x1C, 1); // queue_enable
    }

    // Configure the first request descriptors. They are rewritten for every sync I/O.
    unsafe {
        let notify_offset = mmio_read_u16(common, 0x1E) as u32;
        let notify_byte_offset = notify_offset.checked_mul(notify_multiplier)
            .ok_or("VIRTIO-BLK NOTIFY OFFSET OVERFLOW")?;
        if notify_byte_offset.checked_add(2).filter(|value| *value <= notify_cap.length).is_none() {
            return Err("VIRTIO-BLK QUEUE NOTIFY OFFSET EXCEEDS ITS CAPABILITY");
        }
        let notify = notify_base.add(notify_byte_offset as usize);
        let capacity = read_capacity_stable(common, device_config)
            .ok_or("VIRTIO-BLK CAPACITY CHANGED DURING CONFIGURATION READ")?;
        if capacity == 0 { return Err("VIRTIO-BLK REPORTED ZERO CAPACITY"); }
        mmio_write_u8(common, 0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FEATURES_OK | STATUS_DRIVER_OK);
        if mmio_read_u8(common, 0x14) & STATUS_DRIVER_OK == 0 {
            mmio_write_u8(common, 0x14, STATUS_ACKNOWLEDGE | STATUS_DRIVER | STATUS_FAILED);
            return Err("VIRTIO-BLK DID NOT ENTER DRIVER_OK STATE");
        }
        compiler_fence(Ordering::SeqCst);
        let mut state = STATE.lock();
        *state = DriverState {
            ready: true, sectors: capacity, dma_physical, dma_virtual: dma_virtual as usize,
            common: common as usize, notify: notify as usize, queue_size, available_index: 0,
        };
    }

    READY.store(true, Ordering::Release);
    Ok(sector_count())
}

pub fn read_sector(lba: u64, output: &mut [u8; 512]) -> Result<(), &'static str> {
    request_sector(lba, output, false)
}

pub fn write_sector(lba: u64, input: &[u8; 512]) -> Result<(), &'static str> {
    let mut staging = *input;
    request_sector(lba, &mut staging, true)
}

fn request_sector(lba: u64, buffer: &mut [u8; 512], write: bool) -> Result<(), &'static str> {
    if !is_ready() { return Err("VIRTIO-BLK DRIVER IS NOT READY"); }
    let mut state = STATE.lock();
    if !state.ready { return Err("VIRTIO-BLK STATE IS NOT READY"); }
    if lba >= state.sectors { return Err("VIRTIO-BLK SECTOR IS OUT OF RANGE"); }

    let dma = state.dma_virtual as *mut u8;
    let request = unsafe { dma.add(REQUEST_HEADER_OFFSET) };
    let data = unsafe { dma.add(REQUEST_DATA_OFFSET) };
    let status = unsafe { dma.add(REQUEST_STATUS_OFFSET) };
    let desc = dma.cast::<VirtqDescriptor>();
    let avail = unsafe { dma.add(PAGE_SIZE) };
    let used = unsafe { dma.add(PAGE_SIZE * 2) };
    let (request_type, data_flags) = if write { (BLOCK_T_OUT, 0) } else { (BLOCK_T_IN, DESC_F_WRITE) };

    if write { unsafe { core::ptr::copy_nonoverlapping(buffer.as_ptr(), data, 512); } }
    unsafe {
        write_volatile(request.cast::<u32>(), request_type);
        write_volatile(request.add(4).cast::<u32>(), 0);
        write_volatile(request.add(8).cast::<u64>(), lba);
        write_volatile(status, 0xFF);
        write_volatile(desc.add(0), VirtqDescriptor { address: state.dma_physical + REQUEST_HEADER_OFFSET as u64, length: 16, flags: DESC_F_NEXT, next: 1 });
        write_volatile(desc.add(1), VirtqDescriptor { address: state.dma_physical + REQUEST_DATA_OFFSET as u64, length: 512, flags: DESC_F_NEXT | data_flags, next: 2 });
        write_volatile(desc.add(2), VirtqDescriptor { address: state.dma_physical + REQUEST_STATUS_OFFSET as u64, length: 1, flags: DESC_F_WRITE, next: 0 });
    }

    let used_index_before = unsafe { read_volatile(used.add(2).cast::<u16>()) };
    let ring_slot = state.available_index % state.queue_size;
    unsafe {
        write_volatile(avail.add(4).cast::<u16>().add(ring_slot as usize), 0);
        compiler_fence(Ordering::Release);
        state.available_index = state.available_index.wrapping_add(1);
        write_volatile(avail.add(2).cast::<u16>(), state.available_index);
        fence(Ordering::SeqCst);
        write_volatile((state.notify as *mut u8).cast::<u16>(), 0);
    }

    let mut completed = false;
    for _ in 0..POLL_LIMIT {
        let used_index = unsafe { read_volatile(used.add(2).cast::<u16>()) };
        if used_index != used_index_before { completed = true; break; }
        core::hint::spin_loop();
    }
    if !completed { return Err("VIRTIO-BLK REQUEST TIMED OUT"); }
    fence(Ordering::SeqCst);
    let request_status = unsafe { read_volatile(status) };
    if request_status != VIRTIO_BLK_S_OK { return Err("VIRTIO-BLK DEVICE REPORTED AN I/O ERROR"); }
    let used_head = unsafe { read_volatile(used.add(4).cast::<u32>()) };
    if used_head != 0 { return Err("VIRTIO-BLK RETURNED AN UNEXPECTED DESCRIPTOR ID"); }
    if !write { unsafe { core::ptr::copy_nonoverlapping(data, buffer.as_mut_ptr(), 512); } }
    Ok(())
}

#[repr(C)]
#[derive(Clone, Copy)]
struct VirtqDescriptor {
    address: u64,
    length: u32,
    flags: u16,
    next: u16,
}

fn read_capacity_stable(common: *mut u8, device_config: *mut u8) -> Option<u64> {
    // VirtIO exposes a config_generation byte so multiword device configuration
    // reads can detect a concurrent device-side update.
    for _ in 0..8 {
        let before = unsafe { mmio_read_u8(common, 0x15) };
        let low = unsafe { mmio_read_u32(device_config, 0) };
        let high = unsafe { mmio_read_u32(device_config, 4) };
        let after = unsafe { mmio_read_u8(common, 0x15) };
        if before == after { return Some(((high as u64) << 32) | low as u64); }
    }
    None
}

fn parse_capabilities(device: PciDevice) -> Result<VirtioCaps, &'static str> {
    let status = pci::read_config_u16(device, 0x06);
    if status & (1 << 4) == 0 { return Err("VIRTIO-BLK HAS NO PCI CAPABILITY LIST"); }
    let initial_pointer = pci::read_config_u8(device, 0x34);
    if initial_pointer != 0 && initial_pointer & 0x03 != 0 {
        return Err("VIRTIO-BLK PCI CAPABILITY POINTER IS NOT DWORD-ALIGNED");
    }
    let mut pointer = initial_pointer;
    let mut visited = [false; 64];
    let mut terminated = false;
    let mut caps = VirtioCaps { common: None, notify: None, device: None, notify_multiplier: 0 };
    for _ in 0..48 {
        if pointer < 0x40 || pointer > 0xFC || pointer & 0x03 != 0 {
            return Err("VIRTIO-BLK PCI CAPABILITY POINTER IS OUT OF RANGE");
        }
        let slot = (pointer as usize) / 4;
        if visited[slot] { return Err("VIRTIO-BLK PCI CAPABILITY LIST CONTAINS A LOOP"); }
        visited[slot] = true;
        let cap_id = pci::read_config_u8(device, pointer);
        let next = pci::read_config_u8(device, pointer + 1);
        if next != 0 && next & 0x03 != 0 {
            return Err("VIRTIO-BLK PCI CAPABILITY NEXT POINTER IS NOT DWORD-ALIGNED");
        }
        if cap_id == PCI_CAP_VENDOR_SPECIFIC {
            let cap_len = pci::read_config_u8(device, pointer + 2);
            let cap_end = (pointer as usize).checked_add(cap_len as usize)
                .ok_or("VIRTIO-BLK PCI CAPABILITY LENGTH OVERFLOW")?;
            if cap_len < 16 || cap_end > 256 {
                return Err("VIRTIO-BLK VENDOR PCI CAPABILITY HAS AN INVALID LENGTH");
            }
            let cfg_type = pci::read_config_u8(device, pointer + 3);
            let bar = pci::read_config_u8(device, pointer + 4);
            let offset = pci::read_config_u32(device, pointer + 8);
            let length = pci::read_config_u32(device, pointer + 12);
            let capability = VirtioCapability { bar, offset, length };
            match cfg_type {
                CAP_COMMON_CFG if caps.common.is_none() => caps.common = Some(capability),
                CAP_NOTIFY_CFG if cap_len >= 20 && caps.notify.is_none() => {
                    caps.notify = Some(capability);
                    caps.notify_multiplier = pci::read_config_u32(device, pointer + 16);
                }
                CAP_DEVICE_CFG if caps.device.is_none() => caps.device = Some(capability),
                _ => {}
            }
        }
        if next == 0 { terminated = true; break; }
        if next < 0x40 { return Err("VIRTIO-BLK PCI CAPABILITY NEXT POINTER IS INVALID"); }
        pointer = next;
    }
    if !terminated { return Err("VIRTIO-BLK PCI CAPABILITY LIST DID NOT TERMINATE WITHIN 48 ENTRIES"); }
    Ok(caps)
}

fn map_capability(device: PciDevice, capability: VirtioCapability) -> Result<*mut u8, &'static str> {
    let bar = pci::bar_info(device, capability.bar).ok_or("VIRTIO-BLK CAPABILITY REFERENCES AN INVALID BAR")?;
    if bar.kind == BarKind::Io { return Err("VIRTIO-BLK PCI CAPABILITY REFERENCES AN I/O BAR"); }
    let physical = bar.base.checked_add(capability.offset as u64).ok_or("VIRTIO-BLK BAR OFFSET OVERFLOW")?;
    if physical >= 0x1_0000_0000 || physical.checked_add(capability.length as u64).filter(|end| *end <= 0x1_0000_0000).is_none() {
        return Err("VIRTIO-BLK MMIO WINDOW IS OUTSIDE THE CURRENT 4-GIB HHDM LIMIT");
    }
    memory::map_mmio_region(physical, capability.length as u64)?;
    memory::physical_to_virtual(physical).filter(|ptr| !ptr.is_null()).ok_or("VIRTIO-BLK MMIO WINDOW IS NOT HHDM-ACCESSIBLE")
}

unsafe fn mmio_read_u8(base: *mut u8, offset: usize) -> u8 { unsafe { read_volatile(base.add(offset)) } }
unsafe fn mmio_read_u16(base: *mut u8, offset: usize) -> u16 { unsafe { read_volatile(base.add(offset).cast::<u16>()) } }
unsafe fn mmio_read_u32(base: *mut u8, offset: usize) -> u32 { unsafe { read_volatile(base.add(offset).cast::<u32>()) } }
unsafe fn mmio_write_u8(base: *mut u8, offset: usize, value: u8) { unsafe { write_volatile(base.add(offset), value); } }
unsafe fn mmio_write_u16(base: *mut u8, offset: usize, value: u16) { unsafe { write_volatile(base.add(offset).cast::<u16>(), value); } }
unsafe fn mmio_write_u32(base: *mut u8, offset: usize, value: u32) { unsafe { write_volatile(base.add(offset).cast::<u32>(), value); } }
unsafe fn mmio_write_u64(base: *mut u8, offset: usize, value: u64) { unsafe { write_volatile(base.add(offset).cast::<u64>(), value); } }

pub fn self_test() -> Result<(), &'static str> {
    if !is_ready() { return Err("VIRTIO-BLK DRIVER IS NOT READY"); }
    let state = STATE.lock();
    if state.sectors == 0 || state.dma_physical == 0 || state.dma_virtual == 0
        || state.common == 0 || state.notify == 0 || state.queue_size < 4
    { return Err("VIRTIO-BLK RUNTIME STATE IS INCOMPLETE"); }
    if !state.queue_size.is_power_of_two() { return Err("VIRTIO-BLK QUEUE SIZE IS NOT A POWER OF TWO"); }
    if state.dma_physical & (PAGE_SIZE as u64 - 1) != 0 { return Err("VIRTIO-BLK DMA AREA IS NOT PAGE ALIGNED"); }
    if state.common == 0 || state.notify == 0 { return Err("VIRTIO-BLK MMIO REGISTERS ARE NOT MAPPED"); }
    Ok(())
}
