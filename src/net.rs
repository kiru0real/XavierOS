//! Minimal polled Intel E1000 networking for XavierOS/QEMU user-mode networking.
//!
//! The driver uses explicitly mapped HHDM MMIO and physically contiguous DMA.
//! It targets QEMU's emulated E1000 and user-mode NAT: DHCPv4 with a static
//! fallback, ARP, IPv4, ICMP echo, DNS A records, and UDP-based NTP. It is
//! single-core and polling-only; one-shot HTTP/1.0 over TCP is supported. TLS, IPv6, and physical NICs are not supported.

use core::arch::asm;
use core::mem::MaybeUninit;
use core::ptr::{addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{compiler_fence, AtomicU8, AtomicU32, AtomicU64, Ordering};

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

const DEFAULT_LOCAL_IP: [u8; 4] = [10, 0, 2, 15];
const DEFAULT_GATEWAY_IP: [u8; 4] = [10, 0, 2, 2];
const DEFAULT_DNS_IP: [u8; 4] = [10, 0, 2, 3];
const DEFAULT_NETMASK: [u8; 4] = [255, 255, 255, 0];
const DNS_LOCAL_PORT: u16 = 53000;
const NTP_LOCAL_PORT: u16 = 53123;
const TCP_LOCAL_PORT_BASE: u16 = 49152;
const TCP_MAX_PAYLOAD: usize = 1400;
const HTTP_MAX_REQUEST: usize = 768;
const HTTP_TIMEOUT_TICKS: u64 = 500; // 5 seconds per synchronous request.
const DNS_ID: u16 = 0x5841;
const ICMP_ID: u16 = 0x5841;
const DHCP_CLIENT_PORT: u16 = 68;
const DHCP_SERVER_PORT: u16 = 67;
const DHCP_MAGIC_COOKIE: [u8; 4] = [99, 130, 83, 99];
const NTP_UNIX_EPOCH_DELTA: u64 = 2_208_988_800;
static CONFIG_LOCAL_IP: AtomicU32 = AtomicU32::new(u32::from_be_bytes(DEFAULT_LOCAL_IP));
static CONFIG_GATEWAY_IP: AtomicU32 = AtomicU32::new(u32::from_be_bytes(DEFAULT_GATEWAY_IP));
static CONFIG_DNS_IP: AtomicU32 = AtomicU32::new(u32::from_be_bytes(DEFAULT_DNS_IP));
static CONFIG_NETMASK: AtomicU32 = AtomicU32::new(u32::from_be_bytes(DEFAULT_NETMASK));
static CONFIG_DHCP: AtomicU8 = AtomicU8::new(0);

const STATUS_UNINITIALIZED: u8 = 0;
const STATUS_READY: u8 = 1;
const STATUS_NOT_FOUND: u8 = 2;
const STATUS_FAILED: u8 = 3;
static DRIVER_STATUS: AtomicU8 = AtomicU8::new(STATUS_UNINITIALIZED);
static TX_FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static RX_FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static TX_ERROR_COUNT: AtomicU64 = AtomicU64::new(0);
static RX_DROP_COUNT: AtomicU64 = AtomicU64::new(0);
static TCP_RETRANSMIT_COUNT: AtomicU64 = AtomicU64::new(0);
static HTTP_REQUEST_COUNT: AtomicU64 = AtomicU64::new(0);
static HTTP_SUCCESS_COUNT: AtomicU64 = AtomicU64::new(0);
static HTTP_FAILURE_COUNT: AtomicU64 = AtomicU64::new(0);
static HTTP_RESPONSE_BYTE_COUNT: AtomicU64 = AtomicU64::new(0);
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
    pub dhcp: bool,
}

#[derive(Clone, Copy, Debug, Default)]
pub struct NetworkStats {
    pub tx_frames: u64,
    pub rx_frames: u64,
    pub tx_errors: u64,
    pub rx_drops: u64,
    pub tcp_retransmits: u64,
    pub http_requests: u64,
    pub http_successes: u64,
    pub http_failures: u64,
    pub http_response_bytes: u64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct HttpResponseInfo {
    pub bytes_received: usize,
    pub body_offset: usize,
    pub status_code: u16,
    pub truncated: bool,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct HttpUrl<'a> {
    host: &'a str,
    host_header: &'a str,
    path: &'a str,
    port: u16,
}

#[derive(Clone, Copy)]
struct TcpView<'a> {
    source_port: u16,
    destination_port: u16,
    sequence: u32,
    acknowledgement: u32,
    flags: u8,
    payload: &'a [u8],
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
        local_ip: configured_ip(&CONFIG_LOCAL_IP),
        netmask: configured_ip(&CONFIG_NETMASK),
        gateway: configured_ip(&CONFIG_GATEWAY_IP),
        dns: configured_ip(&CONFIG_DNS_IP),
        dhcp: CONFIG_DHCP.load(Ordering::Acquire) != 0,
    })
}

pub fn statistics() -> NetworkStats {
    NetworkStats {
        tx_frames: TX_FRAME_COUNT.load(Ordering::Relaxed),
        rx_frames: RX_FRAME_COUNT.load(Ordering::Relaxed),
        tx_errors: TX_ERROR_COUNT.load(Ordering::Relaxed),
        rx_drops: RX_DROP_COUNT.load(Ordering::Relaxed),
        tcp_retransmits: TCP_RETRANSMIT_COUNT.load(Ordering::Relaxed),
        http_requests: HTTP_REQUEST_COUNT.load(Ordering::Relaxed),
        http_successes: HTTP_SUCCESS_COUNT.load(Ordering::Relaxed),
        http_failures: HTTP_FAILURE_COUNT.load(Ordering::Relaxed),
        http_response_bytes: HTTP_RESPONSE_BYTE_COUNT.load(Ordering::Relaxed),
    }
}

pub fn check_gateway() -> Result<(), &'static str> {
    if !is_ready() { return Err("E1000 driver is not ready"); }
    let network = info().ok_or("network information is unavailable")?;
    if !network.link_up { return Err("Ethernet link is down"); }
    resolve_mac(network.gateway).map(|_| ())
}

pub fn check_dns() -> Result<[u8; 4], &'static str> {
    resolve_target("example.com")
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
                if ip.protocol != 1 || ip.destination != configured_ip(&CONFIG_LOCAL_IP) || ip.payload.len() < 8 {
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
    if quoted[12..16] != configured_ip(&CONFIG_LOCAL_IP) || quoted[16..20] != destination { return false; }
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
    frame[28..32].copy_from_slice(&configured_ip(&CONFIG_LOCAL_IP));
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
                && response[38..42] == configured_ip(&CONFIG_LOCAL_IP)
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

    send_udp(configured_ip(&CONFIG_DNS_IP), DNS_LOCAL_PORT, 53, &query[..offset])?;
    let start = interrupts::timer_ticks();
    let mut frame = [0u8; MAX_PACKET_SIZE];
    let mut spins = 0usize;
    while spins < MAX_POLL_SPINS {
        spins += 1;
        if let Some(length) = receive_frame(&mut frame) {
            if let Some(ip) = parse_ipv4_frame(&frame, length) {
                if ip.protocol != 17 || ip.source != configured_ip(&CONFIG_DNS_IP) || ip.destination != configured_ip(&CONFIG_LOCAL_IP) || ip.payload.len() < 8 { continue; }
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
    let mac = resolve_mac(next_hop(destination))?;
    send_udp_to_mac(configured_ip(&CONFIG_LOCAL_IP), destination, mac, source_port, destination_port, payload)
}

fn send_udp_to_mac(source_ip: [u8; 4], destination: [u8; 4], destination_mac: [u8; 6], source_port: u16, destination_port: u16, payload: &[u8]) -> Result<(), &'static str> {
    if payload.len() + 8 > 1480 { return Err("UDP payload too large"); }
    let mut udp = [0u8; 520];
    if payload.len() > 512 { return Err("UDP payload exceeds 512-byte buffer limit"); }
    put_be16(&mut udp, 0, source_port);
    put_be16(&mut udp, 2, destination_port);
    put_be16(&mut udp, 4, (payload.len() + 8) as u16);
    put_be16(&mut udp, 6, 0); // Zero UDP checksum is legal for IPv4.
    udp[8..8 + payload.len()].copy_from_slice(payload);
    send_ipv4_from_to_mac(source_ip, destination, destination_mac, 17, &udp[..8 + payload.len()])
}

fn parse_udp_payload(packet: &[u8], source_port: u16, destination_port: u16) -> Option<&[u8]> {
    if packet.len() < 8 || be16(packet, 0)? != source_port || be16(packet, 2)? != destination_port { return None; }
    let length = be16(packet, 4)? as usize;
    if length < 8 || length > packet.len() { return None; }
    Some(&packet[8..length])
}

fn send_ipv4(destination: [u8; 4], protocol: u8, payload: &[u8]) -> Result<(), &'static str> {
    let mac = resolve_mac(next_hop(destination))?;
    send_ipv4_to_mac(destination, mac, protocol, payload)
}

fn send_ipv4_to_mac(destination: [u8; 4], destination_mac: [u8; 6], protocol: u8, payload: &[u8]) -> Result<(), &'static str> {
    send_ipv4_from_to_mac(configured_ip(&CONFIG_LOCAL_IP), destination, destination_mac, protocol, payload)
}

fn send_ipv4_from_to_mac(source_ip: [u8; 4], destination: [u8; 4], destination_mac: [u8; 6], protocol: u8, payload: &[u8]) -> Result<(), &'static str> {
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
    ip[12..16].copy_from_slice(&source_ip);
    ip[16..20].copy_from_slice(&destination);
    let header_sum = checksum(&ip[..20]);
    put_be16(ip, 10, header_sum);
    ip[20..].copy_from_slice(payload);
    transmit_frame(&frame[..14 + 20 + payload.len()])
}

fn next_hop(destination: [u8; 4]) -> [u8; 4] {
    let local = configured_ip(&CONFIG_LOCAL_IP);
    let mask = configured_ip(&CONFIG_NETMASK);
    let local_net = u32::from_be_bytes(local) & u32::from_be_bytes(mask);
    let destination_net = u32::from_be_bytes(destination) & u32::from_be_bytes(mask);
    if local_net == destination_net { destination } else { configured_ip(&CONFIG_GATEWAY_IP) }
}

fn configured_ip(value: &AtomicU32) -> [u8; 4] {
    value.load(Ordering::Acquire).to_be_bytes()
}

pub fn dhcp_enabled() -> bool {
    CONFIG_DHCP.load(Ordering::Acquire) != 0
}

/// Run a bounded DHCP DISCOVER/OFFER/REQUEST/ACK exchange. The existing
/// address remains active unless a complete, validated ACK is received.
pub fn acquire_dhcp() -> Result<(), &'static str> {
    if !is_ready() { return Err("E1000 driver is not ready"); }
    if info().map(|network| !network.link_up).unwrap_or(true) {
        return Err("Ethernet link is down");
    }
    let mac = unsafe { (&*state_ptr().ok_or("E1000 driver is not ready")?).mac };
    let mut xid = (interrupts::timer_ticks() as u32) ^ (read_tsc() as u32)
        ^ u32::from_be_bytes([mac[0], mac[1], mac[2], mac[3]])
        ^ u32::from_be_bytes([mac[2], mac[3], mac[4], mac[5]]);
    if xid == 0 { xid = 0x5841_4552; }

    let (discover, discover_len) = make_dhcp_message(1, xid, mac, None, None);
    send_udp_to_mac([0; 4], [255; 4], [255; 6], DHCP_CLIENT_PORT, DHCP_SERVER_PORT, &discover[..discover_len])?;
    let offer = wait_for_dhcp_reply(xid, mac, 2)?;
    if !valid_lease_ip(offer.your_ip) { return Err("DHCP offer contained an invalid IPv4 address"); }
    let server = offer.server.ok_or("DHCP offer did not include a server identifier")?;

    let (request, request_len) = make_dhcp_message(3, xid, mac, Some(offer.your_ip), Some(server));
    send_udp_to_mac([0; 4], [255; 4], [255; 6], DHCP_CLIENT_PORT, DHCP_SERVER_PORT, &request[..request_len])?;
    let ack = wait_for_dhcp_reply(xid, mac, 5)?;
    let assigned = if valid_lease_ip(ack.your_ip) { ack.your_ip } else { offer.your_ip };
    if let Some(ack_server) = ack.server {
        if ack_server != server { return Err("DHCP ACK came from a different server"); }
    }
    let mask = ack.subnet.or(offer.subnet).unwrap_or(DEFAULT_NETMASK);
    if !valid_netmask(mask) { return Err("DHCP supplied an invalid subnet mask"); }
    let gateway = ack.router.or(offer.router).unwrap_or(DEFAULT_GATEWAY_IP);
    let dns = ack.dns.or(offer.dns).unwrap_or(DEFAULT_DNS_IP);
    if gateway == [0; 4] || gateway[0] >= 224 || dns == [0; 4] || dns[0] >= 224 {
        return Err("DHCP supplied an invalid gateway or DNS address");
    }

    CONFIG_LOCAL_IP.store(u32::from_be_bytes(assigned), Ordering::Release);
    CONFIG_NETMASK.store(u32::from_be_bytes(mask), Ordering::Release);
    CONFIG_GATEWAY_IP.store(u32::from_be_bytes(gateway), Ordering::Release);
    CONFIG_DNS_IP.store(u32::from_be_bytes(dns), Ordering::Release);
    CONFIG_DHCP.store(1, Ordering::Release);
    if let Some(pointer) = state_ptr() {
        let state = unsafe { &mut *pointer };
        state.arp_valid = false;
        state.arp_ip = [0; 4];
        state.arp_mac = [0; 6];
    }
    Ok(())
}

/// Fetch a bounded plain-HTTP resource. Follows up to three HTTP redirects; HTTPS/TLS is rejected.
/// It uses bounded SYN/request retries and validates Content-Length/chunk framing. The client is synchronous and deliberately small, not a general-purpose TCP implementation.
pub fn http_get(url: &str, output: &mut [u8]) -> Result<HttpResponseInfo, &'static str> {
    HTTP_REQUEST_COUNT.fetch_add(1, Ordering::Relaxed);
    let result = http_get_follow_redirects(url, output);
    match result {
        Ok(info) => {
            HTTP_SUCCESS_COUNT.fetch_add(1, Ordering::Relaxed);
            HTTP_RESPONSE_BYTE_COUNT.fetch_add(info.bytes_received as u64, Ordering::Relaxed);
            Ok(info)
        }
        Err(error) => {
            HTTP_FAILURE_COUNT.fetch_add(1, Ordering::Relaxed);
            Err(error)
        }
    }
}

const HTTP_MAX_REDIRECTS: usize = 3;
const HTTP_URL_CAPACITY: usize = 512;
const TCP_RETRY_INTERVAL_TICKS: u64 = 100; // 1 second at 100 Hz.
const TCP_SYN_RETRIES: usize = 3;
const TCP_DATA_RETRIES: usize = 2;

fn http_get_follow_redirects(url: &str, output: &mut [u8]) -> Result<HttpResponseInfo, &'static str> {
    if url.is_empty() || url.len() >= HTTP_URL_CAPACITY { return Err("HTTP URL is empty or too long"); }
    let mut current = [0u8; HTTP_URL_CAPACITY];
    current[..url.len()].copy_from_slice(url.as_bytes());
    let mut current_len = url.len();
    for redirect_count in 0..=HTTP_MAX_REDIRECTS {
        let current_url = core::str::from_utf8(&current[..current_len]).map_err(|_| "HTTP URL is not valid UTF-8")?;
        let info = http_get_once(current_url, output)?;
        if !matches!(info.status_code, 301 | 302 | 303 | 307 | 308) {
            return Ok(info);
        }
        let header_end = info.body_offset.checked_sub(4).ok_or("HTTP header boundary is invalid")?;
        let headers = output.get(..header_end).ok_or("HTTP headers exceed response buffer")?;
        if count_http_headers(headers, b"location") > 1 {
            return Err("HTTP redirect contains multiple Location headers");
        }
        let location = match find_http_header(headers, b"location") {
            Some(value) if !value.is_empty() => value,
            _ => return Ok(info),
        };
        if redirect_count == HTTP_MAX_REDIRECTS { return Err("HTTP redirect limit exceeded"); }
        let mut next = [0u8; HTTP_URL_CAPACITY];
        let next_len = build_redirect_url(current_url, location, &mut next)?;
        current = next;
        current_len = next_len;
    }
    Err("HTTP redirect processing failed")
}

/// Single HTTP/1.0 transaction. Redirect handling is owned by `http_get` above.
fn http_get_once(url: &str, output: &mut [u8]) -> Result<HttpResponseInfo, &'static str> {
    if !is_ready() { return Err("E1000 driver is not ready"); }
    if output.len() < 128 { return Err("HTTP response buffer must be at least 128 bytes"); }
    let parsed = parse_http_url(url)?;
    let destination = resolve_target(parsed.host)?;
    let destination_mac = resolve_mac(next_hop(destination))?;

    let local_port = TCP_LOCAL_PORT_BASE + (interrupts::timer_ticks() as u16 % 16000);
    let initial_sequence = (read_tsc() as u32) ^ (interrupts::timer_ticks() as u32) ^ 0x5841_1600;
    send_tcp_segment(destination, destination_mac, local_port, parsed.port, initial_sequence, 0, 0x02, &[])?;

    let handshake_start = interrupts::timer_ticks();
    let mut last_syn_tick = handshake_start;
    let mut syn_retries = 0usize;
    let mut frame = [0u8; MAX_PACKET_SIZE];
    let mut spins = 0usize;
    let mut remote_next = 0u32;
    let mut connected = false;
    while spins < MAX_POLL_SPINS {
        spins += 1;
        if let Some(length) = receive_frame(&mut frame) {
            if let Some(ip) = parse_ipv4_frame(&frame, length) {
                if ip.source != destination || ip.destination != configured_ip(&CONFIG_LOCAL_IP) { continue; }
                if let Some(tcp) = parse_tcp_view(&ip) {
                    if tcp.source_port != parsed.port || tcp.destination_port != local_port { continue; }
                    if tcp.flags & 0x04 != 0 { return Err("HTTP TCP connection was reset during handshake"); }
                    if tcp.flags & 0x12 == 0x12 && tcp.acknowledgement == initial_sequence.wrapping_add(1) {
                        remote_next = tcp.sequence.wrapping_add(1);
                        send_tcp_segment(destination, destination_mac, local_port, parsed.port,
                            initial_sequence.wrapping_add(1), remote_next, 0x10, &[])?;
                        connected = true;
                        break;
                    }
                }
            }
        }
        let now = interrupts::timer_ticks();
        if now.wrapping_sub(handshake_start) >= HTTP_TIMEOUT_TICKS {
            return Err("HTTP TCP handshake timed out after SYN retries");
        }
        if now.wrapping_sub(last_syn_tick) >= TCP_RETRY_INTERVAL_TICKS && syn_retries < TCP_SYN_RETRIES {
            send_tcp_segment(destination, destination_mac, local_port, parsed.port, initial_sequence, 0, 0x02, &[])?;
            TCP_RETRANSMIT_COUNT.fetch_add(1, Ordering::Relaxed);
            syn_retries += 1;
            last_syn_tick = now;
        }
        core::hint::spin_loop();
    }
    if !connected { return Err("HTTP TCP handshake hit the polling limit"); }

    let mut request = [0u8; HTTP_MAX_REQUEST];
    let mut request_len = 0usize;
    append_bytes(&mut request, &mut request_len, b"GET ")?;
    append_bytes(&mut request, &mut request_len, parsed.path.as_bytes())?;
    append_bytes(&mut request, &mut request_len, b" HTTP/1.0\r\nHost: ")?;
    append_bytes(&mut request, &mut request_len, parsed.host_header.as_bytes())?;
    append_bytes(&mut request, &mut request_len, b"\r\nUser-Agent: XavierOS/0.1.7\r\nConnection: close\r\nAccept: */*\r\n\r\n")?;
    if request_len > TCP_MAX_PAYLOAD { return Err("HTTP request exceeds supported TCP payload size"); }

    let request_sequence = initial_sequence.wrapping_add(1);
    send_tcp_segment(destination, destination_mac, local_port, parsed.port,
        request_sequence, remote_next, 0x18, &request[..request_len])?;
    let local_next = request_sequence.wrapping_add(request_len as u32);

    let response_start = interrupts::timer_ticks();
    let mut last_request_tick = response_start;
    let mut request_retries = 0usize;
    let mut used = 0usize;
    let mut truncated = false;
    let mut peer_closed = false;
    spins = 0;
    while spins < MAX_POLL_SPINS {
        spins += 1;
        if let Some(length) = receive_frame(&mut frame) {
            if let Some(ip) = parse_ipv4_frame(&frame, length) {
                if ip.protocol != 6 || ip.source != destination || ip.destination != configured_ip(&CONFIG_LOCAL_IP) { continue; }
                let tcp = match parse_tcp_view(&ip) { Some(value) => value, None => continue };
                if tcp.source_port != parsed.port || tcp.destination_port != local_port { continue; }
                if tcp.flags & 0x04 != 0 { return Err("HTTP TCP connection was reset by the server"); }
                if tcp.flags & 0x10 == 0 || tcp.acknowledgement != local_next { continue; }

                let mut ack_needed = false;
                let payload_len = tcp.payload.len();
                if payload_len != 0 {
                    if tcp.sequence == remote_next {
                        let remaining = output.len().saturating_sub(used);
                        let copy_len = payload_len.min(remaining);
                        if copy_len > 0 {
                            output[used..used + copy_len].copy_from_slice(&tcp.payload[..copy_len]);
                            used += copy_len;
                        }
                        if copy_len < payload_len { truncated = true; }
                        remote_next = remote_next.wrapping_add(payload_len as u32);
                        ack_needed = true;
                    } else {
                        // Duplicate or out-of-order data: ACK the next expected byte.
                        ack_needed = true;
                    }
                }
                if tcp.flags & 0x01 != 0 {
                    let fin_sequence = tcp.sequence.wrapping_add(payload_len as u32);
                    if fin_sequence == remote_next {
                        remote_next = remote_next.wrapping_add(1);
                        peer_closed = true;
                    }
                    // A FIN outside the expected sequence is not accepted; ACK the
                    // current receive edge and keep waiting for in-order data.
                    ack_needed = true;
                }
                if ack_needed {
                    send_tcp_segment(destination, destination_mac, local_port, parsed.port,
                        local_next, remote_next, 0x10, &[])?;
                }
                if peer_closed {
                    // Close our half of the connection without waiting indefinitely for the final ACK.
                    let _ = send_tcp_segment(destination, destination_mac, local_port, parsed.port,
                        local_next, remote_next, 0x11, &[]);
                    break;
                }
            }
        }
        let now = interrupts::timer_ticks();
        if now.wrapping_sub(response_start) >= HTTP_TIMEOUT_TICKS {
            return Err(if used == 0 { "HTTP response timed out after bounded TCP retries" } else { "HTTP response timed out after partial data" });
        }
        if used == 0 && !peer_closed && request_retries < TCP_DATA_RETRIES
            && now.wrapping_sub(last_request_tick) >= TCP_RETRY_INTERVAL_TICKS {
            send_tcp_segment(destination, destination_mac, local_port, parsed.port,
                request_sequence, remote_next, 0x18, &request[..request_len])?;
            TCP_RETRANSMIT_COUNT.fetch_add(1, Ordering::Relaxed);
            request_retries += 1;
            last_request_tick = now;
        }
        core::hint::spin_loop();
    }
    if spins >= MAX_POLL_SPINS && !peer_closed {
        return Err("HTTP response hit the polling limit");
    }
    if used == 0 { return Err("HTTP server closed without a response"); }
    let header_end = find_subslice(&output[..used], b"\r\n\r\n").ok_or("HTTP response headers are incomplete or malformed")?;
    let status_code = parse_http_status(&output[..header_end]).ok_or("HTTP response status line is invalid")?;
    let body_offset = header_end + 4;
    let headers = &output[..header_end];
    if count_http_headers(headers, b"transfer-encoding") > 1 {
        return Err("duplicate HTTP Transfer-Encoding headers are rejected");
    }
    let transfer_encoding = find_http_header(headers, b"transfer-encoding");
    let content_length = parse_content_length_header(headers)?;
    if transfer_encoding.is_some() && content_length.is_some() {
        return Err("HTTP response contains both Transfer-Encoding and Content-Length");
    }
    let body_available = used.saturating_sub(body_offset);
    if let Some(encoding) = transfer_encoding {
        if !encoding.eq_ignore_ascii_case(b"chunked") {
            return Err("unsupported HTTP Transfer-Encoding");
        }
        let decoded = decode_chunked_body(&mut output[body_offset..used])?;
        used = body_offset + decoded;
        truncated = false;
    } else if let Some(expected) = content_length {
        if expected <= body_available {
            used = body_offset + expected;
            truncated = false;
        } else if used == output.len() {
            truncated = true;
        } else if !truncated {
            return Err("HTTP body ended before Content-Length bytes arrived");
        }
    }
    Ok(HttpResponseInfo { bytes_received: used, body_offset, status_code, truncated })
}

fn find_http_header<'a>(headers: &'a [u8], name: &[u8]) -> Option<&'a [u8]> {
    for raw_line in headers.split(|byte| *byte == b'\n').skip(1) {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        let Some(colon) = line.iter().position(|byte| *byte == b':') else { continue; };
        if line[..colon].eq_ignore_ascii_case(name) {
            let value = &line[colon + 1..];
            let start = value.iter().position(|byte| *byte != b' ' && *byte != b'\t').unwrap_or(value.len());
            let end = value.iter().rposition(|byte| *byte != b' ' && *byte != b'\t').map(|index| index + 1).unwrap_or(start);
            return Some(&value[start..end]);
        }
    }
    None
}

fn count_http_headers(headers: &[u8], name: &[u8]) -> usize {
    headers.split(|byte| *byte == b'\n').skip(1).filter(|raw_line| {
        let line = raw_line.strip_suffix(b"\r").unwrap_or(raw_line);
        line.iter().position(|byte| *byte == b':')
            .map(|colon| line[..colon].eq_ignore_ascii_case(name)).unwrap_or(false)
    }).count()
}

fn parse_content_length_header(headers: &[u8]) -> Result<Option<usize>, &'static str> {
    if count_http_headers(headers, b"content-length") > 1 {
        return Err("duplicate HTTP Content-Length headers are rejected");
    }
    let Some(value) = find_http_header(headers, b"content-length") else { return Ok(None); };
    if value.is_empty() || !value.iter().all(|byte| byte.is_ascii_digit()) {
        return Err("invalid HTTP Content-Length header");
    }
    let mut length = 0usize;
    for byte in value {
        length = length.checked_mul(10).and_then(|n| n.checked_add((*byte - b'0') as usize))
            .ok_or("HTTP Content-Length overflow")?;
    }
    Ok(Some(length))
}

fn decode_chunked_body(body: &mut [u8]) -> Result<usize, &'static str> {
    let mut read = 0usize;
    let mut written = 0usize;
    loop {
        let remaining = body.get(read..).ok_or("invalid chunked body cursor")?;
        let line_length = find_subslice(remaining, b"\r\n").ok_or("incomplete HTTP chunk-size line")?;
        let line = &remaining[..line_length];
        let token_end = line.iter().position(|byte| *byte == b';').unwrap_or(line.len());
        let token = &line[..token_end];
        if token.is_empty() || token.len() > 8 || !token.iter().all(|byte| byte.is_ascii_hexdigit()) {
            return Err("invalid HTTP chunk size");
        }
        let mut chunk_size = 0usize;
        for byte in token {
            let digit = match *byte {
                b'0'..=b'9' => (*byte - b'0') as usize,
                b'a'..=b'f' => (*byte - b'a' + 10) as usize,
                b'A'..=b'F' => (*byte - b'A' + 10) as usize,
                _ => return Err("invalid HTTP chunk size"),
            };
            chunk_size = chunk_size.checked_mul(16).and_then(|n| n.checked_add(digit))
                .ok_or("HTTP chunk size overflow")?;
        }
        read = read.checked_add(line_length + 2).ok_or("HTTP chunk offset overflow")?;
        if chunk_size == 0 {
            // Consume the terminating empty trailer line, or bounded trailer fields ending in CRLFCRLF.
            loop {
                let tail = body.get(read..).ok_or("invalid HTTP chunk trailer cursor")?;
                let trailer_length = find_subslice(tail, b"\r\n").ok_or("incomplete HTTP chunk trailer")?;
                read = read.checked_add(trailer_length + 2).ok_or("HTTP trailer offset overflow")?;
                if trailer_length == 0 { return Ok(written); }
            }
        }
        let chunk_end = read.checked_add(chunk_size).ok_or("HTTP chunk offset overflow")?;
        let terminator_end = chunk_end.checked_add(2).ok_or("HTTP chunk offset overflow")?;
        let data = body.get(read..chunk_end).ok_or("truncated HTTP chunk data")?;
        if body.get(chunk_end..terminator_end) != Some(&b"\r\n"[..]) {
            return Err("HTTP chunk data is not followed by CRLF");
        }
        let write_end = written.checked_add(data.len()).ok_or("HTTP decoded body overflow")?;
        if write_end > body.len() { return Err("HTTP decoded body exceeds response buffer"); }
        body.copy_within(read..chunk_end, written);
        written = write_end;
        read = terminator_end;
    }
}

fn build_redirect_url(current_url: &str, location: &[u8], output: &mut [u8; HTTP_URL_CAPACITY]) -> Result<usize, &'static str> {
    if location.is_empty() || location.len() >= output.len()
        || !location.iter().all(|byte| (0x21..=0x7E).contains(byte))
        || location.contains(&b'#') {
        return Err("invalid or unsupported HTTP redirect Location");
    }
    let is_absolute_http = location.get(..7).map(|scheme| scheme.eq_ignore_ascii_case(b"http://")).unwrap_or(false);
    if find_subslice(location, b"://").is_some() && !is_absolute_http {
        return Err("redirects to HTTPS or unsupported URL schemes are rejected");
    }
    let mut used = 0usize;
    if is_absolute_http {
        output[..location.len()].copy_from_slice(location);
        used = location.len();
    } else {
        let current = parse_http_url(current_url)?;
        append_bytes(output, &mut used, b"http://")?;
        append_bytes(output, &mut used, current.host_header.as_bytes())?;
        if location.starts_with(b"/") {
            append_bytes(output, &mut used, location)?;
        } else if location.starts_with(b"?") {
            let path = current.path.split('?').next().unwrap_or("/");
            append_bytes(output, &mut used, path.as_bytes())?;
            append_bytes(output, &mut used, location)?;
        } else {
            let path = current.path.split('?').next().unwrap_or("/");
            let directory_end = path.rfind('/').map(|index| index + 1).unwrap_or(1);
            append_bytes(output, &mut used, path[..directory_end].as_bytes())?;
            append_bytes(output, &mut used, location)?;
        }
    }
    let text = core::str::from_utf8(&output[..used]).map_err(|_| "redirect URL is not valid UTF-8")?;
    let _ = parse_http_url(text)?;
    Ok(used)
}

fn parse_http_url(url: &str) -> Result<HttpUrl<'_>, &'static str> {
    let scheme_end = url.find("://").ok_or("URL must start with http:// (HTTPS is not implemented)")?;
    if !url[..scheme_end].eq_ignore_ascii_case("http") {
        return Err("only plain HTTP is supported; HTTPS/TLS is not implemented");
    }
    let rest = &url[scheme_end + 3..];
    let authority_end = rest.find('/').unwrap_or(rest.len());
    let authority = &rest[..authority_end];
    let path = if authority_end == rest.len() { "/" } else { &rest[authority_end..] };
    if authority.is_empty() || authority.contains('@') || authority.len() > 260 {
        return Err("invalid HTTP URL authority");
    }
    if path.len() > 384 || !path.starts_with('/') || path.bytes().any(|byte| byte < 0x20 || byte == b' ' || byte == b'#') {
        return Err("invalid HTTP path or path exceeds 384 bytes");
    }
    let (host, port) = if let Some((host, port_text)) = authority.rsplit_once(':') {
        if host.is_empty() || host.contains(':') || port_text.is_empty() || !port_text.bytes().all(|b| b.is_ascii_digit()) {
            return Err("invalid HTTP host or port");
        }
        let mut port = 0u32;
        for byte in port_text.bytes() { port = port.saturating_mul(10).saturating_add((byte - b'0') as u32); }
        if port == 0 || port > u16::MAX as u32 { return Err("HTTP port must be between 1 and 65535"); }
        (host, port as u16)
    } else { (authority, 80u16) };
    if host.is_empty() || host.len() > 253 || !host.bytes().all(|b| b.is_ascii_alphanumeric() || b == b'.' || b == b'-') {
        return Err("invalid HTTP hostname; IPv6 literals are not supported");
    }
    Ok(HttpUrl { host, host_header: authority, path, port })
}

fn append_bytes(target: &mut [u8], used: &mut usize, bytes: &[u8]) -> Result<(), &'static str> {
    let end = used.checked_add(bytes.len()).ok_or("buffer length overflow")?;
    if end > target.len() { return Err("HTTP request is too long"); }
    target[*used..end].copy_from_slice(bytes);
    *used = end;
    Ok(())
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || needle.len() > haystack.len() { return None; }
    haystack.windows(needle.len()).position(|window| window == needle)
}

fn parse_http_status(headers: &[u8]) -> Option<u16> {
    let end = headers.iter().position(|b| *b == b'\n').unwrap_or(headers.len());
    let line = headers.get(..end)?.strip_suffix(b"\r").unwrap_or(&headers[..end]);
    if !line.starts_with(b"HTTP/1.") || line.len() < 12 || line[8] != b' ' { return None; }
    let digits = line.get(9..12)?;
    if !digits.iter().all(|b| b.is_ascii_digit()) { return None; }
    Some(((digits[0] - b'0') as u16) * 100 + ((digits[1] - b'0') as u16) * 10 + (digits[2] - b'0') as u16)
}

fn parse_tcp_view<'a>(ip: &Ipv4View<'a>) -> Option<TcpView<'a>> {
    if ip.protocol != 6 || ip.payload.len() < 20 { return None; }
    let segment = ip.payload;
    let header_len = ((segment[12] >> 4) as usize).checked_mul(4)?;
    if header_len < 20 || header_len > segment.len() { return None; }
    if tcp_checksum_ipv4(ip.source, ip.destination, segment) != 0 { return None; }
    Some(TcpView {
        source_port: be16(segment, 0)?,
        destination_port: be16(segment, 2)?,
        sequence: u32::from_be_bytes([segment[4], segment[5], segment[6], segment[7]]),
        acknowledgement: u32::from_be_bytes([segment[8], segment[9], segment[10], segment[11]]),
        flags: segment[13],
        payload: &segment[header_len..],
    })
}

fn tcp_checksum_ipv4(source: [u8; 4], destination: [u8; 4], segment: &[u8]) -> u16 {
    if segment.len() > u16::MAX as usize { return 0xFFFF; }
    let mut sum = 0u32;
    sum += u16::from_be_bytes([source[0], source[1]]) as u32;
    sum += u16::from_be_bytes([source[2], source[3]]) as u32;
    sum += u16::from_be_bytes([destination[0], destination[1]]) as u32;
    sum += u16::from_be_bytes([destination[2], destination[3]]) as u32;
    sum += 6; // zero + IP protocol TCP
    sum += segment.len() as u32;
    let mut chunks = segment.chunks_exact(2);
    for pair in &mut chunks { sum += u16::from_be_bytes([pair[0], pair[1]]) as u32; }
    if let Some(last) = chunks.remainder().first() { sum += (*last as u32) << 8; }
    while sum >> 16 != 0 { sum = (sum & 0xFFFF) + (sum >> 16); }
    !(sum as u16)
}

fn send_tcp_segment(destination: [u8; 4], destination_mac: [u8; 6], source_port: u16, destination_port: u16,
                    sequence: u32, acknowledgement: u32, flags: u8, payload: &[u8]) -> Result<(), &'static str> {
    if payload.len() > TCP_MAX_PAYLOAD { return Err("TCP payload exceeds supported frame size"); }
    let mut segment = [0u8; 20 + TCP_MAX_PAYLOAD];
    put_be16(&mut segment, 0, source_port);
    put_be16(&mut segment, 2, destination_port);
    segment[4..8].copy_from_slice(&sequence.to_be_bytes());
    segment[8..12].copy_from_slice(&acknowledgement.to_be_bytes());
    segment[12] = 5 << 4;
    segment[13] = flags;
    put_be16(&mut segment, 14, 64240);
    put_be16(&mut segment, 16, 0);
    put_be16(&mut segment, 18, 0);
    segment[20..20 + payload.len()].copy_from_slice(payload);
    let checksum_value = tcp_checksum_ipv4(configured_ip(&CONFIG_LOCAL_IP), destination, &segment[..20 + payload.len()]);
    put_be16(&mut segment, 16, checksum_value);
    send_ipv4_to_mac(destination, destination_mac, 6, &segment[..20 + payload.len()])
}

/// Query an NTPv3/v4 server and return validated Unix UTC seconds.
pub fn ntp_query(server_name: &str) -> Result<u64, &'static str> {
    let server = resolve_target(server_name)?;
    let server_mac = resolve_mac(next_hop(server))?;
    let mut request = [0u8; 48];
    request[0] = 0x23; // LI=0, VN=4, client mode.
    let nonce_hi = (interrupts::timer_ticks() as u32) ^ 0x5841_4E54;
    let nonce_lo = read_tsc() as u32;
    request[40..44].copy_from_slice(&nonce_hi.to_be_bytes());
    request[44..48].copy_from_slice(&nonce_lo.to_be_bytes());
    send_udp_to_mac(configured_ip(&CONFIG_LOCAL_IP), server, server_mac, NTP_LOCAL_PORT, 123, &request)?;

    let start = interrupts::timer_ticks();
    let mut frame = [0u8; MAX_PACKET_SIZE];
    let mut spins = 0usize;
    while spins < MAX_POLL_SPINS {
        spins += 1;
        if let Some(length) = receive_frame(&mut frame) {
            if let Some(ip) = parse_ipv4_frame(&frame, length) {
                if ip.protocol != 17 || ip.source != server || ip.destination != configured_ip(&CONFIG_LOCAL_IP) { continue; }
                if let Some(payload) = parse_udp_payload(ip.payload, 123, NTP_LOCAL_PORT) {
                    if payload.len() < 48 { continue; }
                    let mode = payload[0] & 0x07;
                    let version = (payload[0] >> 3) & 0x07;
                    let stratum = payload[1];
                    if mode != 4 || !(3..=4).contains(&version) || payload[0] >> 6 == 3 || stratum == 0 || stratum > 15 { continue; }
                    if payload[24..32] != request[40..48] { continue; }
                    // Reject unsynchronized or obviously malformed timestamps before changing the clock.
                    let receive_seconds = u32::from_be_bytes([payload[32], payload[33], payload[34], payload[35]]) as u64;
                    let transmit_seconds = u32::from_be_bytes([payload[40], payload[41], payload[42], payload[43]]) as u64;
                    if receive_seconds == 0 || transmit_seconds == 0 || transmit_seconds <= NTP_UNIX_EPOCH_DELTA {
                        return Err("NTP server returned an invalid timestamp");
                    }
                    if transmit_seconds < receive_seconds {
                        return Err("NTP server returned an invalid timestamp");
                    }
                    return Ok(transmit_seconds - NTP_UNIX_EPOCH_DELTA);
                }
            }
        }
        if interrupts::timer_ticks().wrapping_sub(start) >= POLL_TIMEOUT_TICKS {
            return Err("NTP query timed out after 3 seconds");
        }
        core::hint::spin_loop();
    }
    Err("NTP query timed out (poll limit reached)")
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DhcpReply {
    message_type: u8,
    your_ip: [u8; 4],
    subnet: Option<[u8; 4]>,
    router: Option<[u8; 4]>,
    dns: Option<[u8; 4]>,
    server: Option<[u8; 4]>,
}

fn valid_lease_ip(ip: [u8; 4]) -> bool {
    ip != [0; 4] && ip != [255; 4] && ip[0] < 224 && ip[0] != 127
}

fn valid_netmask(mask: [u8; 4]) -> bool {
    let bits = u32::from_be_bytes(mask);
    if bits == 0 { return false; }
    let inverted = !bits;
    (inverted & inverted.wrapping_add(1)) == 0
}

fn push_dhcp_option(buffer: &mut [u8], position: &mut usize, code: u8, data: &[u8]) -> bool {
    if data.len() > 255 || (*position).saturating_add(2 + data.len()) > buffer.len() { return false; }
    buffer[*position] = code;
    buffer[*position + 1] = data.len() as u8;
    buffer[*position + 2..*position + 2 + data.len()].copy_from_slice(data);
    *position += 2 + data.len();
    true
}

fn make_dhcp_message(message_type: u8, xid: u32, mac: [u8; 6], requested_ip: Option<[u8; 4]>, server: Option<[u8; 4]>) -> ([u8; 320], usize) {
    let mut packet = [0u8; 320];
    packet[0] = 1; // BOOTREQUEST.
    packet[1] = 1; // Ethernet.
    packet[2] = 6;
    packet[4..8].copy_from_slice(&xid.to_be_bytes());
    packet[10..12].copy_from_slice(&0x8000u16.to_be_bytes()); // Broadcast reply.
    packet[28..34].copy_from_slice(&mac);
    packet[236..240].copy_from_slice(&DHCP_MAGIC_COOKIE);
    let mut position = 240usize;
    let _ = push_dhcp_option(&mut packet, &mut position, 53, &[message_type]);
    if let Some(ip) = requested_ip { let _ = push_dhcp_option(&mut packet, &mut position, 50, &ip); }
    if let Some(server_ip) = server { let _ = push_dhcp_option(&mut packet, &mut position, 54, &server_ip); }
    let _ = push_dhcp_option(&mut packet, &mut position, 55, &[1, 3, 6, 51, 54]);
    let mut client_id = [0u8; 7];
    client_id[0] = 1;
    client_id[1..].copy_from_slice(&mac);
    let _ = push_dhcp_option(&mut packet, &mut position, 61, &client_id);
    let _ = push_dhcp_option(&mut packet, &mut position, 57, &576u16.to_be_bytes());
    let _ = push_dhcp_option(&mut packet, &mut position, 12, b"xavieros");
    packet[position] = 255;
    position += 1;
    (packet, position)
}

fn parse_dhcp_reply(packet: &[u8], xid: u32, mac: [u8; 6]) -> Option<DhcpReply> {
    if packet.len() < 240 || packet[0] != 2 || packet[1] != 1 || packet[2] != 6 { return None; }
    if u32::from_be_bytes([packet[4], packet[5], packet[6], packet[7]]) != xid { return None; }
    if packet[28..34] != mac || packet[236..240] != DHCP_MAGIC_COOKIE { return None; }
    let your_ip = [packet[16], packet[17], packet[18], packet[19]];
    let mut message_type = 0u8;
    let mut subnet = None;
    let mut router = None;
    let mut dns = None;
    let mut server = None;
    let mut cursor = 240usize;
    while cursor < packet.len() {
        let code = packet[cursor];
        cursor += 1;
        if code == 255 { break; }
        if code == 0 { continue; }
        let length = *packet.get(cursor)? as usize;
        cursor += 1;
        let end = cursor.checked_add(length)?;
        let value = packet.get(cursor..end)?;
        match code {
            53 if length == 1 => message_type = value[0],
            1 if length == 4 => subnet = Some([value[0], value[1], value[2], value[3]]),
            3 if length >= 4 => router = Some([value[0], value[1], value[2], value[3]]),
            6 if length >= 4 => dns = Some([value[0], value[1], value[2], value[3]]),
            54 if length == 4 => server = Some([value[0], value[1], value[2], value[3]]),
            _ => {}
        }
        cursor = end;
    }
    if message_type == 0 { return None; }
    Some(DhcpReply { message_type, your_ip, subnet, router, dns, server })
}

fn wait_for_dhcp_reply(xid: u32, mac: [u8; 6], expected_type: u8) -> Result<DhcpReply, &'static str> {
    let start = interrupts::timer_ticks();
    let mut frame = [0u8; MAX_PACKET_SIZE];
    let mut spins = 0usize;
    while spins < MAX_POLL_SPINS {
        spins += 1;
        if let Some(length) = receive_frame(&mut frame) {
            if let Some(ip) = parse_ipv4_frame(&frame, length) {
                if ip.protocol == 17 {
                    if let Some(payload) = parse_udp_payload(ip.payload, DHCP_SERVER_PORT, DHCP_CLIENT_PORT) {
                        if let Some(reply) = parse_dhcp_reply(payload, xid, mac) {
                            if reply.message_type == expected_type { return Ok(reply); }
                        }
                    }
                }
            }
        }
        if interrupts::timer_ticks().wrapping_sub(start) >= POLL_TIMEOUT_TICKS {
            return Err(if expected_type == 2 { "DHCP offer timed out" } else { "DHCP acknowledgement timed out" });
        }
        core::hint::spin_loop();
    }
    Err("DHCP timed out (poll limit reached)")
}

fn transmit_frame(frame: &[u8]) -> Result<(), &'static str> {
    if frame.len() < 14 || frame.len() > MAX_FRAME_SIZE { TX_ERROR_COUNT.fetch_add(1, Ordering::Relaxed); return Err("invalid Ethernet frame size"); }
    let state_pointer = match state_ptr() { Some(value) => value, None => { TX_ERROR_COUNT.fetch_add(1, Ordering::Relaxed); return Err("E1000 driver is not ready"); } };
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
    if status & DESC_STATUS_DD == 0 { TX_ERROR_COUNT.fetch_add(1, Ordering::Relaxed); return Err("E1000 transmit descriptor stayed busy"); }

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
        if unsafe { read_volatile(desc.add(12)) } & DESC_STATUS_DD != 0 {
            TX_FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
            return Ok(());
        }
        spins += 1;
        core::hint::spin_loop();
    }
    TX_ERROR_COUNT.fetch_add(1, Ordering::Relaxed);
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
        RX_FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
        Some(length)
    } else {
        RX_DROP_COUNT.fetch_add(1, Ordering::Relaxed);
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
    let fragment_field = be16(packet, 6)?;
    // We do not reassemble fragmented IPv4 datagrams; reject both nonzero offsets and MF.
    if fragment_field & 0x3FFF != 0 { return None; }
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
    use super::{build_redirect_url, checksum, decode_chunked_body, find_http_header, is_supported_e1000, make_dhcp_message, parse_content_length_header, parse_dhcp_reply, parse_dns_a_response, parse_http_status, parse_http_url, parse_ipv4, skip_dns_name, tcp_checksum_ipv4, valid_netmask, DHCP_MAGIC_COOKIE};

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
    fn builds_and_parses_dhcp_discover_message_safely() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let (discover, len) = make_dhcp_message(1, 0x1234_5678, mac, None, None);
        assert_eq!(discover[0], 1);
        assert_eq!(u32::from_be_bytes([discover[4], discover[5], discover[6], discover[7]]), 0x1234_5678);
        assert_eq!(&discover[28..34], &mac);
        assert_eq!(&discover[236..240], &DHCP_MAGIC_COOKIE);
        assert_eq!(discover[len - 1], 255);
        assert!(parse_dhcp_reply(&discover[..len], 0x1234_5678, mac).is_none());
    }

    #[test]
    fn parses_dhcp_offer_identity_and_basic_options() {
        let mac = [0x52, 0x54, 0x00, 0x12, 0x34, 0x56];
        let xid = 0x1020_3040;
        let (mut packet, mut len) = make_dhcp_message(1, xid, mac, None, None);
        packet[0] = 2;
        packet[16..20].copy_from_slice(&[10, 0, 2, 15]);
        let mut cursor = 240usize;
        while cursor < len && packet[cursor] != 255 {
            if packet[cursor] == 0 { cursor += 1; continue; }
            let option_len = packet[cursor + 1] as usize;
            if packet[cursor] == 53 { packet[cursor + 2] = 2; }
            cursor += 2 + option_len;
        }
        assert_eq!(packet[cursor], 255);
        let options = [
            (1u8, [255u8, 255, 255, 0]),
            (3u8, [10u8, 0, 2, 2]),
            (6u8, [10u8, 0, 2, 3]),
            (54u8, [10u8, 0, 2, 2]),
        ];
        for (code, value) in options {
            packet[cursor] = code;
            packet[cursor + 1] = 4;
            packet[cursor + 2..cursor + 6].copy_from_slice(&value);
            cursor += 6;
        }
        packet[cursor] = 255;
        len = cursor + 1;
        let parsed = parse_dhcp_reply(&packet[..len], xid, mac).unwrap();
        assert_eq!(parsed.message_type, 2);
        assert_eq!(parsed.your_ip, [10, 0, 2, 15]);
        assert_eq!(parsed.subnet, Some([255, 255, 255, 0]));
        assert_eq!(parsed.router, Some([10, 0, 2, 2]));
        assert_eq!(parsed.dns, Some([10, 0, 2, 3]));
        assert_eq!(parsed.server, Some([10, 0, 2, 2]));
    }

    #[test]
    fn validates_subnet_masks() {
        assert!(valid_netmask([255, 255, 255, 0]));
        assert!(valid_netmask([255, 255, 0, 0]));
        assert!(!valid_netmask([255, 0, 255, 0]));
        assert!(!valid_netmask([0, 0, 0, 0]));
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
    #[test]
    fn parses_only_supported_safe_http_urls() {
        let parsed = parse_http_url("http://example.com/path?q=1").unwrap();
        assert_eq!(parsed.host, "example.com");
        assert_eq!(parsed.path, "/path?q=1");
        assert_eq!(parsed.port, 80);
        let custom = parse_http_url("http://example.com:8080/").unwrap();
        assert_eq!(custom.port, 8080);
        assert_eq!(parse_http_url("https://example.com/"), Err("only plain HTTP is supported; HTTPS/TLS is not implemented"));
        assert!(parse_http_url("http://user@example.com/").is_err());
        assert!(parse_http_url("http://example.com:0/").is_err());
    }

    #[test]
    fn parses_http_status_line_and_rejects_malformed_status() {
        assert_eq!(parse_http_status(b"HTTP/1.0 200 OK\r\nContent-Type: text/plain"), Some(200));
        assert_eq!(parse_http_status(b"HTTP/1.1 404 Not Found\r\n"), Some(404));
        assert_eq!(parse_http_status(b"NOT HTTP\r\n"), None);
        assert_eq!(parse_http_status(b"HTTP/1.1 ABC Nope"), None);
    }

    #[test]
    fn tcp_checksum_is_zero_when_inserted() {
        let source = [10, 0, 2, 15];
        let destination = [93, 184, 216, 34];
        let mut segment = [0u8; 21];
        segment[0..2].copy_from_slice(&49152u16.to_be_bytes());
        segment[2..4].copy_from_slice(&80u16.to_be_bytes());
        segment[12] = 0x50;
        segment[13] = 0x10;
        segment[20] = b'x';
        let value = tcp_checksum_ipv4(source, destination, &segment);
        segment[16..18].copy_from_slice(&value.to_be_bytes());
        assert_eq!(tcp_checksum_ipv4(source, destination, &segment), 0);
    }

    #[test]
    fn parses_content_length_without_overflow() {
        assert_eq!(parse_content_length_header(b"HTTP/1.0 200 OK\r\nContent-Length: 12\r\n").unwrap(), Some(12));
        assert!(parse_content_length_header(b"HTTP/1.0 200 OK\r\nContent-Length: 184467440737095516160\r\n").is_err());
        assert!(parse_content_length_header(b"HTTP/1.0 200 OK\r\nContent-Length: +1\r\n").is_err());
    }

    #[test]
    fn decodes_chunked_body_in_place_and_rejects_truncation() {
        let mut body = *b"4\r\ntest\r\n0\r\n\r\nxxxxxxxx";
        let length = decode_chunked_body(&mut body).unwrap();
        assert_eq!(&body[..length], b"test");
        let mut truncated = *b"5\r\nno\r\n";
        let mut invalid_size = *b"Z\r\n";
        assert!(decode_chunked_body(&mut truncated).is_err());
        assert!(decode_chunked_body(&mut invalid_size).is_err());
    }

    #[test]
    fn follows_only_bounded_http_redirect_targets() {
        let mut output = [0u8; super::HTTP_URL_CAPACITY];
        let length = build_redirect_url("http://example.com/a/old", b"/next", &mut output).unwrap();
        assert_eq!(&output[..length], b"http://example.com/next");
        assert!(parse_http_url(core::str::from_utf8(&output[..length]).unwrap()).is_ok());
        assert!(build_redirect_url("http://example.com/", b"https://example.com/", &mut output).is_err());
    }

    #[test]
    fn finds_headers_case_insensitively_and_trims_whitespace() {
        assert_eq!(find_http_header(b"HTTP/1.0 302 Found\r\nLocation:  /next \t\r\n", b"location"), Some(&b"/next"[..]));
    }

    #[test]
    fn rejects_ambiguous_duplicate_content_length() {
        assert!(parse_content_length_header(b"HTTP/1.0 200 OK\r\nContent-Length: 4\r\ncontent-length: 5\r\n").is_err());
    }

    #[test]
    fn decodes_chunk_extensions_and_trailers() {
        let mut body = *b"4;foo=bar\r\ntest\r\n0\r\nX-Test: yes\r\n\r\n";
        let length = decode_chunked_body(&mut body).unwrap();
        assert_eq!(&body[..length], b"test");
    }

}
