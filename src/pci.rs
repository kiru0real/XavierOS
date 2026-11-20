//! PCI configuration-space enumerator (configuration mechanism #1).
//!
//! This is a single-function, polling discovery layer for the legacy PC platform.
//! It does not configure bridges, MSI/MSI-X, PCIe extended capabilities, or hotplug.

use crate::sync::SpinLock;
use core::sync::atomic::{AtomicBool, Ordering};

const CONFIG_ADDRESS: u16 = 0x0CF8;
const CONFIG_DATA: u16 = 0x0CFC;
const MAX_DEVICES: usize = 128;
const PCI_VENDOR_ABSENT: u16 = 0xFFFF;

static CONFIG_LOCK: SpinLock<()> = SpinLock::new(());
static STATE: SpinLock<PciState> = SpinLock::new(PciState::new());
static INITIALIZED: AtomicBool = AtomicBool::new(false);

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum BarKind {
    Io,
    Memory32,
    Memory64,
}

#[derive(Clone, Copy)]
pub struct BarInfo {
    pub index: u8,
    pub kind: BarKind,
    pub base: u64,
    pub prefetchable: bool,
}

#[derive(Clone, Copy)]
pub struct PciDevice {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub vendor_id: u16,
    pub device_id: u16,
    pub revision: u8,
    pub programming_interface: u8,
    pub subclass: u8,
    pub class_code: u8,
    pub header_type: u8,
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
}

impl PciDevice {
    pub const EMPTY: Self = Self {
        bus: 0, device: 0, function: 0, vendor_id: 0xFFFF, device_id: 0,
        revision: 0, programming_interface: 0, subclass: 0, class_code: 0,
        header_type: 0, interrupt_line: 0xFF, interrupt_pin: 0,
    };

    pub fn bdf(&self) -> (u8, u8, u8) { (self.bus, self.device, self.function) }
}

struct PciState {
    devices: [PciDevice; MAX_DEVICES],
    count: usize,
    overflow: bool,
}

impl PciState {
    const fn new() -> Self {
        Self { devices: [PciDevice::EMPTY; MAX_DEVICES], count: 0, overflow: false }
    }
}

#[derive(Clone, Copy)]
pub struct PciStats {
    pub devices: usize,
    pub table_overflow: bool,
}

pub fn is_initialized() -> bool { INITIALIZED.load(Ordering::Acquire) }

/// Enumerate all 256 buses, 32 slots and up to 8 functions per multifunction device.
pub fn initialize() -> Result<PciStats, &'static str> {
    if is_initialized() {
        let state = STATE.lock();
        return Ok(PciStats { devices: state.count, table_overflow: state.overflow });
    }

    let mut discovered = [PciDevice::EMPTY; MAX_DEVICES];
    let mut count = 0usize;
    let mut overflow = false;
    {
        // CF8/CFC is a shared address/data pair. Keep the pair serialized so a
        // future second caller cannot interleave the address between the I/O ops.
        let _config_guard = CONFIG_LOCK.lock();
        for bus in 0u16..=255 {
            for slot in 0u8..32 {
                let id0 = read_dword_unlocked(bus as u8, slot, 0, 0x00);
                let vendor0 = id0 as u16;
                if vendor0 == PCI_VENDOR_ABSENT || vendor0 == 0 { continue; }
                let header0 = read_dword_unlocked(bus as u8, slot, 0, 0x0C);
                let header_type0 = ((header0 >> 16) & 0xFF) as u8;
                let function_limit = if header_type0 & 0x80 != 0 { 8 } else { 1 };
                for function in 0u8..function_limit {
                    let id = if function == 0 { id0 } else {
                        read_dword_unlocked(bus as u8, slot, function, 0x00)
                    };
                    let vendor_id = id as u16;
                    if vendor_id == PCI_VENDOR_ABSENT || vendor_id == 0 { continue; }
                    let class = read_dword_unlocked(bus as u8, slot, function, 0x08);
                    let header = read_dword_unlocked(bus as u8, slot, function, 0x0C);
                    let interrupt = read_dword_unlocked(bus as u8, slot, function, 0x3C);
                    let entry = PciDevice {
                        bus: bus as u8,
                        device: slot,
                        function,
                        vendor_id,
                        device_id: (id >> 16) as u16,
                        revision: class as u8,
                        programming_interface: (class >> 8) as u8,
                        subclass: (class >> 16) as u8,
                        class_code: (class >> 24) as u8,
                        header_type: ((header >> 16) & 0x7F) as u8,
                        interrupt_line: interrupt as u8,
                        interrupt_pin: (interrupt >> 8) as u8,
                    };
                    if count < MAX_DEVICES {
                        discovered[count] = entry;
                        count += 1;
                    } else {
                        overflow = true;
                    }
                }
            }
        }
    }

    {
        let mut state = STATE.lock();
        state.devices = discovered;
        state.count = count;
        state.overflow = overflow;
    }
    INITIALIZED.store(true, Ordering::Release);
    Ok(PciStats { devices: count, table_overflow: overflow })
}

pub fn stats() -> PciStats {
    let state = STATE.lock();
    PciStats { devices: state.count, table_overflow: state.overflow }
}

pub fn get(index: usize) -> Option<PciDevice> {
    let state = STATE.lock();
    if index < state.count { Some(state.devices[index]) } else { None }
}

pub fn for_each(mut visit: impl FnMut(PciDevice)) {
    let (snapshot, count) = {
        let state = STATE.lock();
        (state.devices, state.count)
    };
    for device in snapshot.iter().take(count).copied() { visit(device); }
}

pub fn find(vendor_id: u16, device_id: u16) -> Option<PciDevice> {
    let state = STATE.lock();
    state.devices[..state.count].iter().copied()
        .find(|entry| entry.vendor_id == vendor_id && entry.device_id == device_id)
}

pub fn find_class(class_code: u8, subclass: u8, programming_interface: Option<u8>) -> Option<PciDevice> {
    let state = STATE.lock();
    state.devices[..state.count].iter().copied().find(|entry| {
        entry.class_code == class_code && entry.subclass == subclass
            && programming_interface.map(|value| value == entry.programming_interface).unwrap_or(true)
    })
}

pub fn read_config_u32(device: PciDevice, offset: u8) -> u32 {
    let _guard = CONFIG_LOCK.lock();
    read_dword_unlocked(device.bus, device.device, device.function, offset)
}

pub fn read_config_u16(device: PciDevice, offset: u8) -> u16 {
    let value = read_config_u32(device, offset & 0xFC);
    (value >> ((offset & 2) * 8)) as u16
}

pub fn read_config_u8(device: PciDevice, offset: u8) -> u8 {
    let value = read_config_u32(device, offset & 0xFC);
    (value >> ((offset & 3) * 8)) as u8
}

pub fn write_config_u16(device: PciDevice, offset: u8, value: u16) {
    let _guard = CONFIG_LOCK.lock();
    let address = config_address(device.bus, device.device, device.function, offset);
    unsafe {
        outl(CONFIG_ADDRESS, address);
        outw(CONFIG_DATA + (offset & 2) as u16, value);
    }
}

/// Enable memory decoding and bus mastering without writing back PCI status bits.
pub fn enable_memory_and_bus_mastering(device: PciDevice) {
    let command = read_config_u16(device, 0x04);
    write_config_u16(device, 0x04, command | 0x0006);
}

/// Read an assigned BAR without probing its size (which would disrupt a live device).
pub fn bar_info(device: PciDevice, index: u8) -> Option<BarInfo> {
    if index >= 6 { return None; }
    if index > 0 {
        let previous = read_config_u32(device, 0x10 + (index - 1) * 4);
        if previous & 1 == 0 && ((previous >> 1) & 0x3) == 0x2 { return None; }
    }
    let raw = read_config_u32(device, 0x10 + index * 4);
    if raw == 0 { return None; }
    if raw & 1 != 0 {
        return Some(BarInfo { index, kind: BarKind::Io, base: (raw & !0x3) as u64, prefetchable: false });
    }
    let memory_type = (raw >> 1) & 0x3;
    let prefetchable = raw & 0x8 != 0;
    match memory_type {
        0 => Some(BarInfo { index, kind: BarKind::Memory32, base: (raw & !0xF) as u64, prefetchable }),
        2 if index < 5 => {
            let high = read_config_u32(device, 0x10 + (index + 1) * 4);
            Some(BarInfo { index, kind: BarKind::Memory64, base: ((high as u64) << 32) | (raw & !0xF) as u64, prefetchable })
        }
        _ => None,
    }
}

pub fn class_name(device: PciDevice) -> &'static str {
    match (device.class_code, device.subclass) {
        (0x01, 0x00) => "SCSI STORAGE",
        (0x01, 0x01) => "IDE CONTROLLER",
        (0x01, 0x06) => "SATA CONTROLLER",
        (0x02, 0x00) => "ETHERNET CONTROLLER",
        (0x03, 0x00) => "VGA CONTROLLER",
        (0x06, 0x00) => "HOST BRIDGE",
        (0x06, 0x01) => "ISA BRIDGE",
        (0x06, 0x04) => "PCI BRIDGE",
        (0x0C, 0x03) if device.programming_interface == 0x30 => "USB XHCI CONTROLLER",
        (0x0C, 0x03) => "USB CONTROLLER",
        (0x0C, 0x05) => "SMBUS CONTROLLER",
        _ => "PCI DEVICE",
    }
}

fn config_address(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    0x8000_0000 | ((bus as u32) << 16) | ((device as u32) << 11)
        | ((function as u32) << 8) | ((offset as u32) & 0xFC)
}

fn read_dword_unlocked(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = config_address(bus, device, function, offset);
    unsafe { outl(CONFIG_ADDRESS, address); inl(CONFIG_DATA) }
}

unsafe fn outl(port: u16, value: u32) {
    unsafe { core::arch::asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags)); }
}
unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe { core::arch::asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags)); }
    value
}
unsafe fn outw(port: u16, value: u16) {
    unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)); }
}

pub fn self_test() -> Result<(), &'static str> {
    if !is_initialized() { return Err("PCI ENUMERATOR IS NOT INITIALIZED"); }
    let state = STATE.lock();
    for device in state.devices.iter().take(state.count) {
        if device.vendor_id == 0 || device.vendor_id == PCI_VENDOR_ABSENT {
            return Err("PCI DEVICE TABLE CONTAINS AN INVALID VENDOR ID");
        }
        if device.device >= 32 || device.function >= 8 {
            return Err("PCI DEVICE TABLE CONTAINS AN INVALID BDF");
        }
    }
    Ok(())
}
