//! Minimal polled Intel E1000 networking for XavierOS/QEMU user-mode networking.
//!
//! The driver uses an HHDM MMIO pointer and a physically contiguous DMA region.
//! The first implementation supports the common QEMU E1000 device, static
//! 10.0.2.15/24 addressing, ARP, IPv4, ICMP echo, and a small DNS A-record client.
//! It is single-core, polling-only, and intentionally has no TCP/DHCP/IPv6.

use core::arch::asm;
use core::mem::MaybeUninit;
use core::ptr::{addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{compiler_fence, AtomicU8, Ordering};

use crate::{interrupts, memory};

const PCI_CONFIG_ADDRESS: u16 = 0x0CF8;
const PCI_CONFIG_DATA: u16 = 0x0CFC;
const PCI_VENDOR_INTEL: u16 = 0x8086;

const PAGE_SIZE: usize = 4096;
const DMA_PAGES: usize = 9;
const RX_COUNT: usize = 8;
const TX_COUNT: usize = 8;
const DESC_SIZE: usize = 16;
const RX_RING_OFFSET: usize = 0;
const TX_RING_OFFSET: usize = RX_COUNT * DESC_SIZE;
const RX_BUFFERS_OFFSET: usize = PAGE_SIZE;
const RX_BUFFER_SIZE: usize = 2048;
const TX_BUFFERS_OFFSET: usize = RX_BUFFERS_OFFSET + RX_COUNT * RX_BUFFER_SIZE;
const MAX_FRAME_SIZE: usize = 1518;
const MAX_PACKET_SIZE: usize = 2048;
const POLL_TIMEOUT_TICKS: u64 = 300; // 3 seconds at the 100 Hz PIT rate.
const MAX_POLL_SPINS: usize = 40_000_000;

const REG_CTRL: usize = 0x0000;
const REG_STATUS: usize = 0x0008;
const REG_RCTL: usize = 0x0100;
const REG_TCTL: usize = 0x0400;
const REG_TIPG: usize = 0x0410;
const REG_IMC: usize = 0x00D8;
const REG_RDBAL: usize = 0x2800;
const REG_RDBAH: usize = 0x2804;
const REG_RDLEN: usize = 0x2808;
const REG_RDH: usize = 0x2810;
const REG_RDT: usize = 0x2818;
const REG_TDBAL: usize = 0x3800;
const REG_TDBAH: usize = 0x3804;
const REG_TDLEN: usize = 0x3808;
const REG_TDH: usize = 0x3810;
const REG_TDT: usize = 0x3818;
const REG_RAL: usize = 0x5400;
const REG_RAH: usize = 0x5404;

const CTRL_RST: u32 = 1 << 26;
const CTRL_SLU: u32 = 1 << 6;
const RCTL_EN: u32 = 1 << 1;
const RCTL_BAM: u32 = 1 << 15;
const RCTL_SECRC: u32 = 1 << 26;
const TCTL_EN: u32 = 1 << 1;
const TCTL_PSP: u32 = 1 << 3;
const TX_CMD_EOP: u8 = 1 << 0;
const TX_CMD_IFCS: u8 = 1 << 1;
const TX_CMD_RS: u8 = 1 << 3;
const DESC_STATUS_DD: u8 = 1;

const LOCAL_IP: [u8; 4] = [10, 0, 2, 15];
const GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
const DNS_IP: [u8; 4] = [10, 0, 2, 3];
const NETMASK: [u8; 4] = [255, 255, 255, 0];
const DNS_LOCAL_PORT: u16 = 53000;
const DNS_ID: u16 = 0x5841;
const ICMP_ID: u16 = 0x5841;

const STATUS_UNINITIALIZED: u8 = 0;
const STATUS_READY: u8 = 1;
const STATUS_NOT_FOUND: u8 = 2;
const STATUS_FAILED: u8 = 3;
static DRIVER_STATUS: AtomicU8 = AtomicU8::new(STATUS_UNINITIALIZED);
static TSC_FREQUENCY_HZ: core::sync::atomic::AtomicU64 = core::sync::atomic::AtomicU64::new(0);
static mut DRIVER_STATE: MaybeUninit<DriverState> = MaybeUninit::uninit();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct E1000Device {
    pub bus: u8,
    pub device: u8,
    pub function: u8,
    pub device_id: u16,
    pub revision: u8,
    pub bar0: u32,
    pub interrupt_line: u8,
    pub interrupt_pin: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProbeResult {
    Found(E1000Device),
    NotFound,
}

#[derive(Clone, Copy)]
struct DriverState {
    mmio: *mut u32,
    dma_virtual: *mut u8,
    dma_physical: u64,
    tx_next: usize,
    rx_next: usize,
    mac: [u8; 6],
    arp_valid: bool,
    arp_ip: [u8; 4],
    arp_mac: [u8; 6],
}

#[derive(Clone, Copy)]
pub struct NetworkInfo {
    pub mac: [u8; 6],
    pub link_up: bool,
    pub local_ip: [u8; 4],
    pub netmask: [u8; 4],
    pub gateway: [u8; 4],
    pub dns: [u8; 4],
}

#[derive(Clone, Copy)]
pub struct PingReply {
    pub address: [u8; 4],
    pub sequence: u16,
    /// Round-trip time in microseconds when CPUID exposes a usable TSC frequency.
    pub rtt_microseconds: Option<u64>,
    /// Coarse 100 Hz PIT ticks, used only when a TSC frequency cannot be determined.
    pub elapsed_ticks: u64,
    /// TTL read from the received IPv4 header, not a hard-coded display value.
    pub ttl: u8,
}

/// Read-only PCI scan for a supported Intel E1000 Ethernet controller.
pub fn probe_e1000() -> ProbeResult {
    for bus in 0u16..=255 {
        for device in 0u8..32 {
            let first_id = pci_read(bus as u8, device, 0, 0x00);
            let first_vendor = first_id as u16;
            if first_vendor == 0xFFFF || first_vendor == 0 { continue; }
            let header = pci_read(bus as u8, device, 0, 0x0C);
            let function_count = if ((header >> 16) as u8) & 0x80 != 0 { 8 } else { 1 };
            for function in 0u8..function_count {
                let id = if function == 0 { first_id } else { pci_read(bus as u8, device, function, 0x00) };
                let vendor = id as u16;
                let device_id = (id >> 16) as u16;
                if vendor != PCI_VENDOR_INTEL || !is_supported_e1000(device_id) { continue; }
                let class_reg = pci_read(bus as u8, device, function, 0x08);
                let class_code = (class_reg >> 24) as u8;
                let subclass = ((class_reg >> 16) & 0xFF) as u8;
                if class_code == 0x02 && subclass == 0x00 {
                    let bar0 = pci_read(bus as u8, device, function, 0x10);
                    let interrupt = pci_read(bus as u8, device, function, 0x3C);
                    return ProbeResult::Found(E1000Device {
                        bus: bus as u8,
                        device,
                        function,
                        device_id,
                        revision: (class_reg & 0xFF) as u8,
                        bar0,
                        interrupt_line: (interrupt & 0xFF) as u8,
                        interrupt_pin: ((interrupt >> 8) & 0xFF) as u8,
                    });
                }
            }
        }
    }
    ProbeResult::NotFound
}

/// Initialize E1000 MMIO registers, DMA descriptor rings, and the fixed QEMU-usernet profile.
pub fn initialize() -> Result<(), &'static str> {
    DRIVER_STATUS.store(STATUS_UNINITIALIZED, Ordering::Release);
    let device = match probe_e1000() {
        ProbeResult::Found(device) => device,
        ProbeResult::NotFound => {
            DRIVER_STATUS.store(STATUS_NOT_FOUND, Ordering::Release);
            return Err("no supported Intel E1000 Ethernet controller found on PCI");
        }
    };

    // BAR0 must be a 32-bit memory BAR; common QEMU -device e1000 uses this form.
    if device.bar0 & 1 != 0 || (device.bar0 >> 1) & 0x3 == 0x2 {
        DRIVER_STATUS.store(STATUS_FAILED, Ordering::Release);
        return Err("E1000 BAR0 is not a supported 32-bit MMIO BAR");
    }
    let bar_address = (device.bar0 & 0xFFFF_FFF0) as u64;
    if bar_address == 0 {
        DRIVER_STATUS.store(STATUS_FAILED, Ordering::Release);
        return Err("E1000 BAR0 is unassigned");
    }
    // E1000 registers used below extend through 0x5404; map the BAR register
    // window explicitly. An HHDM pointer is only arithmetic until page tables
    // actually map reserved PCI MMIO space (otherwise the first register write PFs).
    if let Err(error) = memory::map_mmio_region(bar_address, 0x6000) {
        DRIVER_STATUS.store(STATUS_FAILED, Ordering::Release);
        return Err(error);
    }
    let mmio_ptr = match memory::physical_to_virtual(bar_address) {
        Some(pointer) if !pointer.is_null() => pointer.cast::<u32>(),
        _ => {
            DRIVER_STATUS.store(STATUS_FAILED, Ordering::Release);
            return Err("E1000 MMIO BAR could not be translated through HHDM");
        }
    };

    // Enable PCI memory-space decoding and bus mastering without touching PCI status bits.
    let command = pci_read_word(device.bus, device.device, device.function, 0x04);
    pci_write_word(device.bus, device.device, device.function, 0x04, command | 0x0006);

    unsafe {
        mmio_write(mmio_ptr, REG_IMC, u32::MAX);
        mmio_write(mmio_ptr, REG_RCTL, 0);
        mmio_write(mmio_ptr, REG_TCTL, 0);
        mmio_write(mmio_ptr, REG_CTRL, mmio_read(mmio_ptr, REG_CTRL) | CTRL_RST);
    }
    let mut reset_done = false;
    for _ in 0..2_000_000 {
        if unsafe { mmio_read(mmio_ptr, REG_CTRL) } & CTRL_RST == 0 {
            reset_done = true;
            break;
        }
        core::hint::spin_loop();
    }
    if !reset_done {
        DRIVER_STATUS.store(STATUS_FAILED, Ordering::Release);
        return Err("E1000 reset timed out");
    }
    for _ in 0..20_000 { core::hint::spin_loop(); }
    unsafe { mmio_write(mmio_ptr, REG_IMC, u32::MAX); }

    let ral = unsafe { mmio_read(mmio_ptr, REG_RAL) };
    let rah = unsafe { mmio_read(mmio_ptr, REG_RAH) };
    let mac = [ral as u8, (ral >> 8) as u8, (ral >> 16) as u8, (ral >> 24) as u8,
        rah as u8, (rah >> 8) as u8];
    if mac == [0; 6] || mac == [0xFF; 6] || mac[0] & 1 != 0 {
        DRIVER_STATUS.store(STATUS_FAILED, Ordering::Release);
        return Err("E1000 returned an invalid station MAC address");
    }

    let dma_physical = match memory::allocate_contiguous_pages(DMA_PAGES) {
        Some(address) => address,
        None => {
            DRIVER_STATUS.store(STATUS_FAILED, Ordering::Release);
            return Err("could not allocate contiguous DMA memory for E1000 rings");
        }
    };
    let dma_virtual = match memory::physical_to_virtual(dma_physical) {
        Some(pointer) if !pointer.is_null() => pointer,
        _ => {
            DRIVER_STATUS.store(STATUS_FAILED, Ordering::Release);
            return Err("could not map E1000 DMA memory through HHDM");
        }
    };
    unsafe { core::ptr::write_bytes(dma_virtual, 0, DMA_PAGES * PAGE_SIZE); }

    // RX descriptors occupy page zero [0,128); TX descriptors [128,256).
    // Eight 2 KiB RX buffers occupy pages 1-4, TX buffers pages 5-8.
    for index in 0..RX_COUNT {
        let desc = unsafe { dma_virtual.add(RX_RING_OFFSET + index * DESC_SIZE) };
        let buffer_phys = dma_physical + (RX_BUFFERS_OFFSET + index * RX_BUFFER_SIZE) as u64;
        unsafe {
            write_volatile(desc.cast::<u64>(), buffer_phys);
            write_volatile(desc.add(8).cast::<u64>(), 0);
        }
    }
    for index in 0..TX_COUNT {
        let desc = unsafe { dma_virtual.add(TX_RING_OFFSET + index * DESC_SIZE) };
        let buffer_phys = dma_physical + (TX_BUFFERS_OFFSET + index * RX_BUFFER_SIZE) as u64;
        unsafe {
            write_volatile(desc.cast::<u64>(), buffer_phys);
            write_volatile(desc.add(8).cast::<u32>(), 0);
            write_volatile(desc.add(12), DESC_STATUS_DD);
        }
    }
    compiler_fence(Ordering::SeqCst);

    unsafe {
        mmio_write(mmio_ptr, REG_RDBAL, dma_physical as u32);
        mmio_write(mmio_ptr, REG_RDBAH, 0);
        mmio_write(mmio_ptr, REG_RDLEN, (RX_COUNT * DESC_SIZE) as u32);
        mmio_write(mmio_ptr, REG_RDH, 0);
        mmio_write(mmio_ptr, REG_RDT, (RX_COUNT - 1) as u32);

        mmio_write(mmio_ptr, REG_TDBAL, (dma_physical as u32).wrapping_add(TX_RING_OFFSET as u32));
        mmio_write(mmio_ptr, REG_TDBAH, 0);
        mmio_write(mmio_ptr, REG_TDLEN, (TX_COUNT * DESC_SIZE) as u32);
        mmio_write(mmio_ptr, REG_TDH, 0);
        mmio_write(mmio_ptr, REG_TDT, 0);
        mmio_write(mmio_ptr, REG_TIPG, 0x0060_200A);
        mmio_write(mmio_ptr, REG_CTRL, mmio_read(mmio_ptr, REG_CTRL) | CTRL_SLU);
        mmio_write(mmio_ptr, REG_TCTL, 0x0004_0100 | TCTL_EN | TCTL_PSP);
        mmio_write(mmio_ptr, REG_RCTL, RCTL_EN | RCTL_BAM | RCTL_SECRC);
        // Polling-only driver: mask every E1000 interrupt source.
        mmio_write(mmio_ptr, REG_IMC, u32::MAX);
    }

    let state = DriverState {
        mmio: mmio_ptr,
        dma_virtual,
        dma_physical,
        tx_next: 0,
        rx_next: 0,
        mac,
        arp_valid: false,
        arp_ip: [0; 4],
        arp_mac: [0; 6],
    };
    unsafe { addr_of_mut!(DRIVER_STATE).cast::<DriverState>().write(state); }
    DRIVER_STATUS.store(STATUS_READY, Ordering::Release);
    Ok(())
}

pub fn is_ready() -> bool {
    DRIVER_STATUS.load(Ordering::Acquire) == STATUS_READY
}

pub fn status_text() -> &'static str {
    match DRIVER_STATUS.load(Ordering::Acquire) {
        STATUS_READY => "READY",
        STATUS_NOT_FOUND => "NO E1000 DEVICE",
        STATUS_FAILED => "INITIALIZATION FAILED",
        _ => "NOT INITIALIZED",
    }
}

pub fn info() -> Option<NetworkInfo> {
    let state_pointer = state_ptr()?;
    let state = unsafe { &mut *state_pointer };
    Some(NetworkInfo {
        mac: state.mac,
        link_up: (unsafe { mmio_read(state.mmio, REG_STATUS) } & (1 << 1)) != 0,
        local_ip: LOCAL_IP,
        netmask: NETMASK,
        gateway: GATEWAY_IP,
        dns: DNS_IP,
    })
}

/// Resolve an IPv4 literal or DNS hostname once before issuing repeated probes.
pub fn resolve_target(target: &str) -> Result<[u8; 4], &'static str> {
    if !is_ready() { return Err("E1000 driver is not ready; run NET for status"); }
    if info().map(|net| !net.link_up).unwrap_or(true) {
        return Err("Ethernet link is down; check NET and the QEMU NIC configuration");
    }
    let address = match parse_ipv4(target) {
        Some(address) => address,
        None => resolve_hostname(target)?,
    };
    if address == [0; 4] || address == [255; 4] || address[0] >= 224 {
        return Err("destination is not a unicast IPv4 address");
    }
    Ok(address)
}

/// Send one ICMP echo request using a caller-provided sequence number.
pub fn ping(target: &str, sequence: u16) -> Result<PingReply, &'static str> {
    let address = resolve_target(target)?;
    ping_address(address, sequence)
}

/// Send one ICMP echo request to a resolved IPv4 address.
pub fn ping_address(address: [u8; 4], sequence: u16) -> Result<PingReply, &'static str> {
    if !is_ready() { return Err("E1000 driver is not ready; run NET for status"); }
    if info().map(|net| !net.link_up).unwrap_or(true) {
        return Err("Ethernet link is down; check NET and the QEMU NIC configuration");
    }
    if address == [0; 4] || address == [255; 4] || address[0] >= 224 {
        return Err("destination is not a unicast IPv4 address");
    }

    let hop = next_hop(address);
    let destination_mac = resolve_mac(hop)?;
    let mut icmp = [0u8; 8 + 13];
    icmp[0] = 8; // Echo request.
    icmp[1] = 0;
    put_be16(&mut icmp, 4, ICMP_ID);
    put_be16(&mut icmp, 6, sequence);
    icmp[8..].copy_from_slice(b"XavierOS ping");
    let sum = checksum(&icmp);
    put_be16(&mut icmp, 2, sum);

    // Prefer TSC-based timing for sub-millisecond responses. If the virtual CPU
    // exposes no reliable frequency in CPUID, report the actual PIT resolution
    // instead of claiming that a zero-tick response took exactly zero ms.
    let tsc_hz = tsc_frequency_hz();
    let start_tsc = tsc_hz.map(|_| read_tsc());
    let start_tick = interrupts::timer_ticks();
    send_ipv4_to_mac(address, destination_mac, 1, &icmp)?;
    let mut frame = [0u8; MAX_PACKET_SIZE];
    let mut spins = 0usize;
    while spins < MAX_POLL_SPINS {
        spins += 1;
        if let Some(length) = receive_frame(&mut frame) {
            if let Some(ip) = parse_ipv4_frame(&frame, length) {
                if ip.protocol != 1 || ip.destination != LOCAL_IP || ip.payload.len() < 8 {
                    continue;
                }
                let icmp_packet = ip.payload;
                if ip.source == address
                    && icmp_packet[0] == 0 && icmp_packet[1] == 0
                    && be16(icmp_packet, 4) == Some(ICMP_ID)
                    && be16(icmp_packet, 6) == Some(sequence)
                    && checksum(icmp_packet) == 0
                {
                    let elapsed_ticks = interrupts::timer_ticks().wrapping_sub(start_tick);
                    let rtt_microseconds = match (tsc_hz, start_tsc) {
                        (Some(hz), Some(start)) => {
                            let delta = read_tsc().wrapping_sub(start);
                            let scaled = delta.saturating_mul(1_000_000);
                            Some(scaled.saturating_add(hz.saturating_sub(1)) / hz)
                        }
                        _ => None,
                    };
                    return Ok(PingReply { address, sequence, rtt_microseconds, elapsed_ticks, ttl: ip.ttl });
                }
                // ICMP errors quote the original IP header and the first eight bytes
                // of its payload. Only accept an error that refers to this exact probe.
                if checksum(icmp_packet) == 0 && icmp_packet[0] == 3
                    && quoted_echo_matches(icmp_packet, address, sequence)
                {
                    return Err(match icmp_packet[1] {
                        0 => "destination network is unreachable",
                        1 => "destination host is unreachable",
                        3 => "destination port is unreachable",
                        4 => "path MTU / fragmentation required",
                        9 | 10 | 13 => "destination is administratively prohibited",
                        _ => "destination is unreachable",
                    });
                }
                if checksum(icmp_packet) == 0 && icmp_packet[0] == 11
                    && quoted_echo_matches(icmp_packet, address, sequence)
                {
                    return Err("TTL exceeded before reaching destination");
                }
            }
        }
        if interrupts::timer_ticks().wrapping_sub(start_tick) >= POLL_TIMEOUT_TICKS {
            return Err("request timed out after 3 seconds");
        }
        core::hint::spin_loop();
    }
    Err("request timed out (poll limit reached)")
}

fn tsc_frequency_hz() -> Option<u64> {
    use core::arch::x86_64::__cpuid;
    let cached = TSC_FREQUENCY_HZ.load(Ordering::Acquire);
    if cached != 0 { return Some(cached); }

    let max_leaf = unsafe { __cpuid(0) }.eax;
    // CPUID.16 reports nominal core MHz, not necessarily TSC frequency, so do
    // not silently treat it as a TSC clock. Only CPUID.15's crystal ratio is
    // used as a trustworthy frequency source; otherwise the caller uses PIT.
    let frequency = if max_leaf >= 0x15 {
        let leaf = unsafe { __cpuid(0x15) };
        if leaf.eax != 0 && leaf.ebx != 0 && leaf.ecx != 0 {
            (leaf.ecx as u64).checked_mul(leaf.ebx as u64).map(|value| value / leaf.eax as u64)
        } else { None }
    } else { None };
    let frequency = frequency.filter(|hz| (1_000_000..=10_000_000_000).contains(hz));
    if let Some(hz) = frequency { TSC_FREQUENCY_HZ.store(hz, Ordering::Release); }
    frequency
}

#[inline(always)]
fn read_tsc() -> u64 {
    let low: u32;
    let high: u32;
    unsafe {
        asm!("lfence", "rdtsc", out("eax") low, out("edx") high, options(nomem, nostack, preserves_flags));
    }
    ((high as u64) << 32) | low as u64
}

fn quoted_echo_matches(icmp_error: &[u8], destination: [u8; 4], sequence: u16) -> bool {
    if icmp_error.len() < 8 + 20 + 8 { return false; }
    let quoted = &icmp_error[8..];
    if quoted[0] >> 4 != 4 || quoted[9] != 1 { return false; }
    let header_len = ((quoted[0] & 0x0F) as usize) * 4;
    if header_len < 20 || quoted.len() < header_len + 8 { return false; }
    if quoted[12..16] != LOCAL_IP || quoted[16..20] != destination { return false; }
    let echo = &quoted[header_len..];
    echo[0] == 8 && be16(echo, 4) == Some(ICMP_ID) && be16(echo, 6) == Some(sequence)
}

fn resolve_mac(address: [u8; 4]) -> Result<[u8; 6], &'static str> {
    {
        let state_pointer = state_ptr().ok_or("E1000 driver is not ready")?;
        let state = unsafe { &mut *state_pointer };
        if state.arp_valid && state.arp_ip == address {
            return Ok(state.arp_mac);
        }
    }
    let state_mac = unsafe { &mut *state_ptr().ok_or("E1000 driver is not ready")? }.mac;
    let mut frame = [0u8; 60];
    frame[0..6].fill(0xFF);
    frame[6..12].copy_from_slice(&state_mac);
    put_be16(&mut frame, 12, 0x0806);
    put_be16(&mut frame, 14, 1); // Ethernet hardware.
    put_be16(&mut frame, 16, 0x0800);
    frame[18] = 6;
    frame[19] = 4;
    put_be16(&mut frame, 20, 1); // ARP request.
    frame[22..28].copy_from_slice(&state_mac);
    frame[28..32].copy_from_slice(&LOCAL_IP);
    frame[32..38].fill(0);
    frame[38..42].copy_from_slice(&address);
    transmit_frame(&frame)?;

    let start = interrupts::timer_ticks();
    let mut response = [0u8; MAX_PACKET_SIZE];
    let mut spins = 0usize;
    while spins < MAX_POLL_SPINS {
        spins += 1;
        if let Some(length) = receive_frame(&mut response) {
            if length >= 42 && be16(&response, 12) == Some(0x0806)
                && be16(&response, 20) == Some(2)
                && response[28..32] == address
                && response[38..42] == LOCAL_IP
            {
                let mut mac = [0u8; 6];
                mac.copy_from_slice(&response[22..28]);
                if mac != [0; 6] && mac[0] & 1 == 0 {
                    let state_pointer = state_ptr().ok_or("E1000 driver is not ready")?;
                    let state = unsafe { &mut *state_pointer };
                    state.arp_valid = true;
                    state.arp_ip = address;
                    state.arp_mac = mac;
                    return Ok(mac);
                }
            }
        }
        if interrupts::timer_ticks().wrapping_sub(start) >= POLL_TIMEOUT_TICKS {
            return Err("ARP resolution timed out; check QEMU user networking");
        }
        core::hint::spin_loop();
    }
    Err("ARP resolution timed out (poll limit reached)")
}

fn resolve_hostname(hostname: &str) -> Result<[u8; 4], &'static str> {
    let host = hostname.strip_suffix('.').unwrap_or(hostname);
    if host.is_empty() || host.len() > 253 { return Err("invalid DNS hostname length"); }
    let mut query = [0u8; 512];
    put_be16(&mut query, 0, DNS_ID);
    put_be16(&mut query, 2, 0x0100); // Recursion desired.
    put_be16(&mut query, 4, 1); // One question.
    let mut offset = 12usize;
    for label in host.split('.') {
        if label.is_empty() || label.len() > 63 || !label.as_bytes().iter().all(|b| b.is_ascii_alphanumeric() || *b == b'-') {
            return Err("invalid DNS hostname; use letters, digits, and hyphens");
        }
        if label.starts_with('-') || label.ends_with('-') { return Err("DNS labels cannot start or end with a hyphen"); }
        if offset + 1 + label.len() + 5 > query.len() { return Err("DNS query is too long"); }
        query[offset] = label.len() as u8;
        offset += 1;
        for byte in label.bytes() { query[offset] = byte.to_ascii_lowercase(); offset += 1; }
    }
    query[offset] = 0;
    offset += 1;
    put_be16(&mut query, offset, 1); offset += 2; // A record.
    put_be16(&mut query, offset, 1); offset += 2; // IN class.

    send_udp(DNS_IP, DNS_LOCAL_PORT, 53, &query[..offset])?;
    let start = interrupts::timer_ticks();
    let mut frame = [0u8; MAX_PACKET_SIZE];
    let mut spins = 0usize;
    while spins < MAX_POLL_SPINS {
        spins += 1;
        if let Some(length) = receive_frame(&mut frame) {
            if let Some(ip) = parse_ipv4_frame(&frame, length) {
                if ip.protocol != 17 || ip.source != DNS_IP || ip.destination != LOCAL_IP || ip.payload.len() < 8 { continue; }
                if be16(ip.payload, 0) != Some(53) || be16(ip.payload, 2) != Some(DNS_LOCAL_PORT) { continue; }
                let udp_len = be16(ip.payload, 4).unwrap_or(0) as usize;
                if udp_len < 8 || udp_len > ip.payload.len() { continue; }
                let dns = &ip.payload[8..udp_len];
                if let Some(address) = parse_dns_a_response(dns) { return Ok(address); }
            }
        }
        if interrupts::timer_ticks().wrapping_sub(start) >= POLL_TIMEOUT_TICKS {
            return Err("DNS query timed out; name resolution failed");
        }
        core::hint::spin_loop();
    }
    Err("DNS query timed out (poll limit reached)")
}

fn parse_dns_a_response(message: &[u8]) -> Option<[u8; 4]> {
    if message.len() < 12 || be16(message, 0)? != DNS_ID { return None; }
    let flags = be16(message, 2)?;
    if flags & 0x8000 == 0 || flags & 0x7800 != 0 || flags & 0x000F != 0 { return None; }
    let questions = be16(message, 4)? as usize;
    let answers = be16(message, 6)? as usize;
    if questions == 0 || answers == 0 || answers > 64 { return None; }
    let mut offset = 12usize;
    for _ in 0..questions {
        offset = skip_dns_name(message, offset)?;
        offset = offset.checked_add(4)?;
        if offset > message.len() { return None; }
    }
    for _ in 0..answers {
        offset = skip_dns_name(message, offset)?;
        if offset.checked_add(10)? > message.len() { return None; }
        let record_type = be16(message, offset)?;
        let class = be16(message, offset + 2)?;
        let data_len = be16(message, offset + 8)? as usize;
        offset += 10;
        let end = offset.checked_add(data_len)?;
        if end > message.len() { return None; }
        if record_type == 1 && class == 1 && data_len == 4 {
            return Some([message[offset], message[offset + 1], message[offset + 2], message[offset + 3]]);
        }
        offset = end;
    }
    None
}

fn skip_dns_name(message: &[u8], start: usize) -> Option<usize> {
    let mut offset = start;
    let mut jumps = 0usize;
    loop {
        let length = *message.get(offset)?;
        if length & 0xC0 == 0xC0 {
            message.get(offset + 1)?;
            return Some(offset + 2);
        }
        if length & 0xC0 != 0 { return None; }
        offset += 1;
        if length == 0 { return Some(offset); }
        if length > 63 { return None; }
        offset = offset.checked_add(length as usize)?;
        if offset > message.len() { return None; }
        jumps += 1;
        if jumps > 128 { return None; }
    }
}

fn send_udp(destination: [u8; 4], source_port: u16, destination_port: u16, payload: &[u8]) -> Result<(), &'static str> {
    if payload.len() + 8 > 1480 { return Err("UDP payload too large"); }
    let mut udp = [0u8; 512 + 8];
    if payload.len() > 512 { return Err("UDP payload exceeds DNS buffer limit"); }
    put_be16(&mut udp, 0, source_port);
    put_be16(&mut udp, 2, destination_port);
    put_be16(&mut udp, 4, (payload.len() + 8) as u16);
    put_be16(&mut udp, 6, 0); // Zero UDP checksum is legal for IPv4.
    udp[8..8 + payload.len()].copy_from_slice(payload);
    send_ipv4(destination, 17, &udp[..8 + payload.len()])
}

fn send_ipv4(destination: [u8; 4], protocol: u8, payload: &[u8]) -> Result<(), &'static str> {
    let mac = resolve_mac(next_hop(destination))?;
    send_ipv4_to_mac(destination, mac, protocol, payload)
}

fn send_ipv4_to_mac(destination: [u8; 4], destination_mac: [u8; 6], protocol: u8, payload: &[u8]) -> Result<(), &'static str> {
    if payload.len() > 1480 { return Err("IPv4 payload too large"); }
    let source_mac = unsafe { &mut *state_ptr().ok_or("E1000 driver is not ready")? }.mac;
    let mut frame = [0u8; MAX_FRAME_SIZE];
    frame[0..6].copy_from_slice(&destination_mac);
    frame[6..12].copy_from_slice(&source_mac);
    put_be16(&mut frame, 12, 0x0800);
    let ip = &mut frame[14..14 + 20 + payload.len()];
    ip[0] = 0x45;
    ip[1] = 0;
    put_be16(ip, 2, (20 + payload.len()) as u16);
    put_be16(ip, 4, (interrupts::timer_ticks() as u16) ^ 0x5841);
    put_be16(ip, 6, 0x4000); // Don't fragment.
    ip[8] = 64;
    ip[9] = protocol;
    put_be16(ip, 10, 0);
    ip[12..16].copy_from_slice(&LOCAL_IP);
    ip[16..20].copy_from_slice(&destination);
    let header_sum = checksum(&ip[..20]);
    put_be16(ip, 10, header_sum);
    ip[20..].copy_from_slice(payload);
    transmit_frame(&frame[..14 + 20 + payload.len()])
}

fn next_hop(destination: [u8; 4]) -> [u8; 4] {
    if destination[0] == LOCAL_IP[0] && destination[1] == LOCAL_IP[1] && destination[2] == LOCAL_IP[2] {
        destination
    } else {
        GATEWAY_IP
    }
}

fn transmit_frame(frame: &[u8]) -> Result<(), &'static str> {
    if frame.len() < 14 || frame.len() > MAX_FRAME_SIZE { return Err("invalid Ethernet frame size"); }
    let state_pointer = state_ptr().ok_or("E1000 driver is not ready")?;
    let state = unsafe { &mut *state_pointer };
    let index = state.tx_next;
    let desc = unsafe { state.dma_virtual.add(TX_RING_OFFSET + index * DESC_SIZE) };
    let mut status = unsafe { read_volatile(desc.add(12)) };
    let mut spins = 0usize;
    while status & DESC_STATUS_DD == 0 && spins < 1_000_000 {
        spins += 1;
        status = unsafe { read_volatile(desc.add(12)) };
        core::hint::spin_loop();
    }
    if status & DESC_STATUS_DD == 0 { return Err("E1000 transmit descriptor stayed busy"); }

    let buffer = unsafe { state.dma_virtual.add(TX_BUFFERS_OFFSET + index * RX_BUFFER_SIZE) };
    let wire_len = frame.len().max(60);
    unsafe {
        core::ptr::write_bytes(buffer, 0, wire_len);
        core::ptr::copy_nonoverlapping(frame.as_ptr(), buffer, frame.len());
        write_volatile(desc.cast::<u64>(), state.dma_physical + (TX_BUFFERS_OFFSET + index * RX_BUFFER_SIZE) as u64);
        write_volatile(desc.add(8).cast::<u16>(), wire_len as u16);
        write_volatile(desc.add(10), 0);
        write_volatile(desc.add(11), TX_CMD_EOP | TX_CMD_IFCS | TX_CMD_RS);
        write_volatile(desc.add(12), 0);
        write_volatile(desc.add(13), 0);
        write_volatile(desc.add(14).cast::<u16>(), 0);
    }
    compiler_fence(Ordering::SeqCst);
    let next = (index + 1) % TX_COUNT;
    unsafe { mmio_write(state.mmio, REG_TDT, next as u32); }
    state.tx_next = next;

    let mut spins = 0usize;
    while spins < 2_000_000 {
        if unsafe { read_volatile(desc.add(12)) } & DESC_STATUS_DD != 0 { return Ok(()); }
        spins += 1;
        core::hint::spin_loop();
    }
    Err("E1000 transmit timed out")
}

fn receive_frame(output: &mut [u8; MAX_PACKET_SIZE]) -> Option<usize> {
    let state_pointer = state_ptr()?;
    let state = unsafe { &mut *state_pointer };
    let index = state.rx_next;
    let desc = unsafe { state.dma_virtual.add(RX_RING_OFFSET + index * DESC_SIZE) };
    let status = unsafe { read_volatile(desc.add(12)) };
    if status & DESC_STATUS_DD == 0 { return None; }
    let length = unsafe { read_volatile(desc.add(8).cast::<u16>()) } as usize;
    let errors = unsafe { read_volatile(desc.add(13)) };
    let packet = unsafe { state.dma_virtual.add(RX_BUFFERS_OFFSET + index * RX_BUFFER_SIZE) };
    let result = if errors == 0 && (14..=RX_BUFFER_SIZE).contains(&length) && length <= output.len() {
        unsafe { core::ptr::copy_nonoverlapping(packet, output.as_mut_ptr(), length); }
        Some(length)
    } else {
        None
    };
    unsafe {
        write_volatile(desc.add(8).cast::<u64>(), 0);
        write_volatile(desc.add(12), 0);
    }
    compiler_fence(Ordering::SeqCst);
    unsafe { mmio_write(state.mmio, REG_RDT, index as u32); }
    state.rx_next = (index + 1) % RX_COUNT;
    result
}

struct Ipv4View<'a> {
    source: [u8; 4],
    destination: [u8; 4],
    protocol: u8,
    ttl: u8,
    payload: &'a [u8],
}

fn parse_ipv4_frame(frame: &[u8], frame_len: usize) -> Option<Ipv4View<'_>> {
    if frame_len < 14 + 20 || be16(frame, 12)? != 0x0800 { return None; }
    let packet = frame.get(14..frame_len)?;
    if packet[0] >> 4 != 4 { return None; }
    let header_len = ((packet[0] & 0x0F) as usize).checked_mul(4)?;
    if header_len < 20 || header_len > packet.len() { return None; }
    let total_len = be16(packet, 2)? as usize;
    if total_len < header_len || total_len > packet.len() || checksum(&packet[..header_len]) != 0 { return None; }
    let mut source = [0u8; 4];
    let mut destination = [0u8; 4];
    source.copy_from_slice(&packet[12..16]);
    destination.copy_from_slice(&packet[16..20]);
    Some(Ipv4View { source, destination, protocol: packet[9], ttl: packet[8], payload: &packet[header_len..total_len] })
}

fn parse_ipv4(input: &str) -> Option<[u8; 4]> {
    let mut result = [0u8; 4];
    let mut count = 0usize;
    for part in input.split('.') {
        if count >= 4 || part.is_empty() || part.len() > 3 || !part.bytes().all(|b| b.is_ascii_digit()) { return None; }
        let mut value = 0u16;
        for byte in part.bytes() { value = value * 10 + (byte - b'0') as u16; }
        if value > 255 { return None; }
        result[count] = value as u8;
        count += 1;
    }
    if count == 4 { Some(result) } else { None }
}

fn checksum(bytes: &[u8]) -> u16 {
    let mut sum = 0u32;
    let mut chunks = bytes.chunks_exact(2);
    for pair in &mut chunks { sum += u16::from_be_bytes([pair[0], pair[1]]) as u32; }
    if let Some(&last) = chunks.remainder().first() { sum += (last as u32) << 8; }
    while sum >> 16 != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    !(sum as u16)
}

fn be16(bytes: &[u8], offset: usize) -> Option<u16> {
    Some(u16::from_be_bytes([*bytes.get(offset)?, *bytes.get(offset + 1)?]))
}

fn put_be16(bytes: &mut [u8], offset: usize, value: u16) {
    if offset + 2 <= bytes.len() { bytes[offset..offset + 2].copy_from_slice(&value.to_be_bytes()); }
}

fn pci_read(bus: u8, device: u8, function: u8, offset: u8) -> u32 {
    let address = 0x8000_0000u32 | ((bus as u32) << 16) | ((device as u32) << 11)
        | ((function as u32) << 8) | ((offset as u32) & 0xFC);
    unsafe { outl(PCI_CONFIG_ADDRESS, address); inl(PCI_CONFIG_DATA) }
}

fn pci_read_word(bus: u8, device: u8, function: u8, offset: u8) -> u16 {
    let value = pci_read(bus, device, function, offset & 0xFC);
    (value >> ((offset & 2) * 8)) as u16
}

fn pci_write_word(bus: u8, device: u8, function: u8, offset: u8, value: u16) {
    let address = 0x8000_0000u32 | ((bus as u32) << 16) | ((device as u32) << 11)
        | ((function as u32) << 8) | ((offset as u32) & 0xFC);
    unsafe {
        outl(PCI_CONFIG_ADDRESS, address);
        outw(PCI_CONFIG_DATA + ((offset & 2) as u16), value);
    }
}

fn is_supported_e1000(device_id: u16) -> bool {
    matches!(device_id, 0x1000 | 0x1001 | 0x1004 | 0x100E | 0x100F | 0x1010 | 0x1011 | 0x1026 | 0x107C | 0x10D3 | 0x153A)
}

unsafe fn mmio_read(base: *mut u32, offset: usize) -> u32 {
    unsafe { read_volatile(base.add(offset / 4)) }
}

unsafe fn mmio_write(base: *mut u32, offset: usize, value: u32) {
    unsafe { write_volatile(base.add(offset / 4), value); }
}

fn state_ptr() -> Option<*mut DriverState> {
    if DRIVER_STATUS.load(Ordering::Acquire) != STATUS_READY { return None; }
    Some(addr_of_mut!(DRIVER_STATE).cast::<DriverState>())
}

unsafe fn outl(port: u16, value: u32) {
    unsafe { asm!("out dx, eax", in("dx") port, in("eax") value, options(nomem, nostack, preserves_flags)); }
}
unsafe fn inl(port: u16) -> u32 {
    let value: u32;
    unsafe { asm!("in eax, dx", in("dx") port, out("eax") value, options(nomem, nostack, preserves_flags)); }
    value
}
unsafe fn outw(port: u16, value: u16) {
    unsafe { asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)); }
}

#[cfg(test)]
mod tests {
    use super::{checksum, is_supported_e1000, parse_dns_a_response, parse_ipv4, skip_dns_name};

    #[test]
    fn recognizes_qemu_e1000_device_id() { assert!(is_supported_e1000(0x100E)); }
    #[test]
    fn rejects_non_e1000_ids() { assert!(!is_supported_e1000(0x1234)); assert!(!is_supported_e1000(0x0F00)); }
    #[test]
    fn parses_ipv4_literals_safely() {
        assert_eq!(parse_ipv4("1.1.1.1"), Some([1, 1, 1, 1]));
        assert_eq!(parse_ipv4("10.0.2.255"), Some([10, 0, 2, 255]));
        assert_eq!(parse_ipv4("999.0.0.1"), None);
        assert_eq!(parse_ipv4("1.2.3"), None);
        assert_eq!(parse_ipv4("1..2.3"), None);
    }
    #[test]
    fn ipv4_checksum_is_zero_after_inserted_checksum() {
        let mut header = [0u8; 20];
        header[0] = 0x45;
        header[2..4].copy_from_slice(&20u16.to_be_bytes());
        header[8] = 64;
        header[9] = 1;
        let value = checksum(&header);
        header[10..12].copy_from_slice(&value.to_be_bytes());
        assert_eq!(checksum(&header), 0);
    }
    #[test]
    fn dns_name_skipping_rejects_truncation() {
        assert_eq!(skip_dns_name(&[3, b'w', b'w', b'w', 0], 0), Some(5));
        assert_eq!(skip_dns_name(&[0xC0], 0), None);
    }
    #[test]
    fn dns_a_answer_is_parsed() {
        let mut message = [0u8; 12 + 17 + 4 + 2 + 10 + 4];
        message[0..2].copy_from_slice(&0x5841u16.to_be_bytes());
        message[2..4].copy_from_slice(&0x8180u16.to_be_bytes());
        message[4..6].copy_from_slice(&1u16.to_be_bytes());
        message[6..8].copy_from_slice(&1u16.to_be_bytes());
        let mut i = 12;
        message[i] = 3; i += 1; message[i..i+3].copy_from_slice(b"www"); i += 3;
        message[i] = 7; i += 1; message[i..i+7].copy_from_slice(b"example"); i += 7;
        message[i] = 3; i += 1; message[i..i+3].copy_from_slice(b"com"); i += 3;
        message[i] = 0; i += 1;
        message[i..i+2].copy_from_slice(&1u16.to_be_bytes()); i += 2;
        message[i..i+2].copy_from_slice(&1u16.to_be_bytes()); i += 2;
        message[i..i+2].copy_from_slice(&[0xC0, 12]); i += 2;
        message[i..i+2].copy_from_slice(&1u16.to_be_bytes()); i += 2;
        message[i..i+2].copy_from_slice(&1u16.to_be_bytes()); i += 2;
        i += 4;
        message[i..i+2].copy_from_slice(&4u16.to_be_bytes()); i += 2;
        message[i..i+4].copy_from_slice(&[93, 184, 216, 34]);
        assert_eq!(parse_dns_a_response(&message), Some([93, 184, 216, 34]));
    }
}
