//! Persistent, checksummed XavierOS configuration with alternating on-disk slots.
//! The previous valid slot is kept until a newer slot has been written successfully.

use core::cell::UnsafeCell;

use crate::{clock, diskfs, net};

const CONFIG_SIZE: usize = 96;
const MAGIC: &[u8; 8] = b"XVCFG018";
const NTP_CAPACITY: usize = 64;
const SLOT_A: &str = "SYSTEM/CONFIG.A";
const SLOT_B: &str = "SYSTEM/CONFIG.B";

#[derive(Clone, Copy)]
pub struct Settings {
    pub timezone_minutes: i32,
    pub use_dhcp: bool,
    pub timeout_seconds: u16,
    pub ntp_server: [u8; NTP_CAPACITY],
    pub ntp_len: usize,
    pub sequence: u64,
    pub loaded_from_disk: bool,
    pub dirty: bool,
    active_slot: u8,
}

impl Settings {
    const fn defaults() -> Self {
        Self {
            timezone_minutes: 0,
            use_dhcp: true,
            timeout_seconds: 5,
            ntp_server: ntp_default(),
            ntp_len: 19,
            sequence: 0,
            loaded_from_disk: false,
            dirty: false,
            active_slot: 1, // first save writes slot A
        }
    }

    pub fn ntp_server_str(&self) -> &str {
        core::str::from_utf8(&self.ntp_server[..self.ntp_len]).unwrap_or("time.cloudflare.com")
    }
}

struct KernelCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for KernelCell<T> {}
impl<T> KernelCell<T> {
    const fn new(value: T) -> Self { Self(UnsafeCell::new(value)) }
    fn get(&self) -> *mut T { self.0.get() }
}

const fn ntp_default() -> [u8; NTP_CAPACITY] {
    let mut value = [0u8; NTP_CAPACITY];
    let bytes = *b"time.cloudflare.com";
    let mut index = 0;
    while index < bytes.len() { value[index] = bytes[index]; index += 1; }
    value
}

static STATE: KernelCell<Settings> = KernelCell::new(Settings::defaults());

/// Load the newest valid configuration slot. A missing config is not an error;
/// malformed slots are ignored and defaults are used. No file is written here.
pub fn initialize() -> Result<bool, &'static str> {
    if !diskfs::mount_status() { return Err("WRITABLE DISK IS NOT MOUNTED"); }
    let a = read_slot(SLOT_A, 0);
    let b = read_slot(SLOT_B, 1);
    let mut settings = match (a, b) {
        (Some(left), Some(right)) => if sequence_newer(right.sequence, left.sequence) { right } else { left },
        (Some(value), None) | (None, Some(value)) => value,
        (None, None) => Settings::defaults(),
    };
    settings.loaded_from_disk = settings.sequence != 0;
    clock::set_timezone_minutes(settings.timezone_minutes)?;
    net::set_timeout_seconds(settings.timeout_seconds)?;
    unsafe { *STATE.get() = settings; }
    Ok(settings.loaded_from_disk)
}

pub fn current() -> Settings { unsafe { *STATE.get() } }

pub fn set_timezone_minutes(minutes: i32) -> Result<(), &'static str> {
    if !(-840..=840).contains(&minutes) { return Err("TIMEZONE OFFSET MUST BE BETWEEN -14:00 AND +14:00"); }
    clock::set_timezone_minutes(minutes)?;
    unsafe { (*STATE.get()).timezone_minutes = minutes; (*STATE.get()).dirty = true; }
    Ok(())
}

pub fn set_ntp_server(server: &str) -> Result<(), &'static str> {
    validate_hostname(server)?;
    let state = unsafe { &mut *STATE.get() };
    state.ntp_server.fill(0);
    state.ntp_server[..server.len()].copy_from_slice(server.as_bytes());
    state.ntp_len = server.len();
    state.dirty = true;
    Ok(())
}

pub fn set_dhcp(enabled: bool) { unsafe { (*STATE.get()).use_dhcp = enabled; (*STATE.get()).dirty = true; } }

pub fn set_timeout(seconds: u16) -> Result<(), &'static str> {
    if !(1..=120).contains(&seconds) { return Err("TIMEOUT MUST BE BETWEEN 1 AND 120 SECONDS"); }
    net::set_timeout_seconds(seconds)?;
    unsafe { (*STATE.get()).timeout_seconds = seconds; (*STATE.get()).dirty = true; }
    Ok(())
}

pub fn reset_to_defaults() -> Result<(), &'static str> {
    let active = unsafe { (*STATE.get()).active_slot };
    let sequence = unsafe { (*STATE.get()).sequence };
    let mut defaults = Settings::defaults();
    defaults.active_slot = active;
    defaults.sequence = sequence;
    defaults.dirty = true;
    clock::set_timezone_minutes(0)?;
    net::set_timeout_seconds(defaults.timeout_seconds)?;
    unsafe { *STATE.get() = defaults; }
    save()
}

/// Save into the inactive slot. XVDISKFS writes data before switching metadata,
/// so a failed write leaves the prior active config slot intact.
pub fn self_test() -> Result<(), &'static str> {
    let state = current();
    if !(-840..=840).contains(&state.timezone_minutes) { return Err("TIMEZONE OFFSET IS OUT OF RANGE"); }
    if !(1..=120).contains(&state.timeout_seconds) { return Err("NETWORK TIMEOUT IS OUT OF RANGE"); }
    validate_hostname(state.ntp_server_str())?;
    Ok(())
}

pub fn save() -> Result<(), &'static str> {
    if !diskfs::mount_status() { return Err("WRITABLE DISK IS NOT MOUNTED"); }
    ensure_system_directory()?;
    let snapshot = current();
    let sequence = snapshot.sequence.checked_add(1).ok_or("CONFIGURATION SEQUENCE OVERFLOW")?;
    let target_slot = if snapshot.active_slot == 0 { 1 } else { 0 };
    let path = if target_slot == 0 { SLOT_A } else { SLOT_B };
    let mut bytes = [0u8; CONFIG_SIZE];
    encode(&snapshot, sequence, &mut bytes)?;
    diskfs::write_file(path, &bytes)?;
    let state = unsafe { &mut *STATE.get() };
    state.sequence = sequence;
    state.active_slot = target_slot;
    state.loaded_from_disk = true;
    state.dirty = false;
    Ok(())
}

fn ensure_system_directory() -> Result<(), &'static str> {
    match diskfs::stat("SYSTEM") {
        Ok(info) if info.is_dir => Ok(()),
        Ok(_) => Err("SYSTEM CONFIG PATH EXISTS BUT IS NOT A DIRECTORY"),
        Err(_) => diskfs::mkdir("SYSTEM"),
    }
}

fn read_slot(path: &str, active_slot: u8) -> Option<Settings> {
    let mut bytes = [0u8; CONFIG_SIZE];
    let length = diskfs::read_file(path, &mut bytes).ok()?;
    if length != CONFIG_SIZE { return None; }
    decode(&bytes, active_slot).ok()
}

fn encode(settings: &Settings, sequence: u64, out: &mut [u8; CONFIG_SIZE]) -> Result<(), &'static str> {
    if settings.ntp_len == 0 || settings.ntp_len >= NTP_CAPACITY { return Err("NTP SERVER NAME LENGTH IS INVALID"); }
    validate_hostname(settings.ntp_server_str())?;
    out.fill(0);
    out[..8].copy_from_slice(MAGIC);
    out[8..16].copy_from_slice(&sequence.to_le_bytes());
    out[16..20].copy_from_slice(&settings.timezone_minutes.to_le_bytes());
    out[20] = if settings.use_dhcp { 1 } else { 0 };
    out[21..23].copy_from_slice(&settings.timeout_seconds.to_le_bytes());
    out[23] = settings.ntp_len as u8;
    out[24..24 + settings.ntp_len].copy_from_slice(&settings.ntp_server[..settings.ntp_len]);
    let checksum = checksum32(&out[..88]);
    out[88..92].copy_from_slice(&checksum.to_le_bytes());
    Ok(())
}

fn decode(bytes: &[u8; CONFIG_SIZE], active_slot: u8) -> Result<Settings, &'static str> {
    if &bytes[..8] != MAGIC { return Err("CONFIG MAGIC IS INVALID"); }
    if bytes[92..].iter().any(|byte| *byte != 0) { return Err("CONFIG RESERVED BYTES ARE NOT ZERO"); }
    let expected = u32::from_le_bytes(bytes[88..92].try_into().map_err(|_| "CONFIG CHECKSUM IS INVALID")?);
    if checksum32(&bytes[..88]) != expected { return Err("CONFIG CHECKSUM MISMATCH"); }
    let sequence = u64::from_le_bytes(bytes[8..16].try_into().map_err(|_| "CONFIG SEQUENCE IS INVALID")?);
    if sequence == 0 { return Err("CONFIG SEQUENCE IS ZERO"); }
    let timezone_minutes = i32::from_le_bytes(bytes[16..20].try_into().map_err(|_| "CONFIG TIMEZONE IS INVALID")?);
    if !(-840..=840).contains(&timezone_minutes) { return Err("CONFIG TIMEZONE IS OUT OF RANGE"); }
    if bytes[20] > 1 { return Err("CONFIG NETWORK MODE IS INVALID"); }
    let timeout_seconds = u16::from_le_bytes([bytes[21], bytes[22]]);
    if !(1..=120).contains(&timeout_seconds) { return Err("CONFIG TIMEOUT IS OUT OF RANGE"); }
    let ntp_len = bytes[23] as usize;
    if ntp_len == 0 || ntp_len >= NTP_CAPACITY { return Err("CONFIG NTP NAME LENGTH IS INVALID"); }
    if bytes[24 + ntp_len..88].iter().any(|byte| *byte != 0) { return Err("CONFIG NTP PADDING IS INVALID"); }
    let server = core::str::from_utf8(&bytes[24..24 + ntp_len]).map_err(|_| "CONFIG NTP NAME IS NOT ASCII")?;
    validate_hostname(server)?;
    let mut ntp_server = [0u8; NTP_CAPACITY];
    ntp_server[..ntp_len].copy_from_slice(server.as_bytes());
    Ok(Settings {
        timezone_minutes, use_dhcp: bytes[20] == 1, timeout_seconds, ntp_server,
        ntp_len, sequence, loaded_from_disk: true, dirty: false, active_slot,
    })
}

fn validate_hostname(server: &str) -> Result<(), &'static str> {
    let bytes = server.as_bytes();
    if bytes.is_empty() || bytes.len() >= NTP_CAPACITY { return Err("NTP SERVER MUST BE 1-63 ASCII HOSTNAME CHARACTERS"); }
    if bytes[0] == b'.' || bytes[bytes.len() - 1] == b'.' || server.contains("..") {
        return Err("NTP SERVER HOSTNAME HAS INVALID DOT PLACEMENT");
    }
    if !bytes.iter().all(|b| b.is_ascii_alphanumeric() || *b == b'.' || *b == b'-') {
        return Err("NTP SERVER MUST BE A HOSTNAME, NOT A URL OR IP-LITERAL");
    }
    Ok(())
}

fn sequence_newer(candidate: u64, current: u64) -> bool {
    candidate != current && candidate.wrapping_sub(current) < (1u64 << 63)
}

fn checksum32(bytes: &[u8]) -> u32 {
    let mut value = 0x811C_9DC5u32;
    for byte in bytes { value ^= *byte as u32; value = value.wrapping_mul(0x0100_0193); }
    value
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_round_trip_and_checksum() {
        let mut source = Settings::defaults();
        source.timezone_minutes = 180;
        source.use_dhcp = true;
        source.timeout_seconds = 7;
        let mut encoded = [0u8; CONFIG_SIZE];
        encode(&source, 9, &mut encoded).unwrap();
        let decoded = decode(&encoded, 0).unwrap();
        assert_eq!(decoded.sequence, 9);
        assert_eq!(decoded.timezone_minutes, 180);
        assert!(decoded.use_dhcp);
        assert_eq!(decoded.timeout_seconds, 7);
        assert_eq!(decoded.ntp_server_str(), "time.cloudflare.com");
        encoded[30] ^= 0x40;
        assert!(decode(&encoded, 0).is_err());
    }

    #[test]
    fn invalid_hostname_is_rejected() {
        let too_long = "x".repeat(64);
        for value in ["", "bad host", "https://example.com", ".bad", "bad.", "bad..host", too_long.as_str()] {
            assert!(validate_hostname(value).is_err(), "accepted {value}");
        }
        assert!(validate_hostname("time.cloudflare.com").is_ok());
    }

    #[test]
    fn wraps_sequence_numbers_safely() {
        assert!(sequence_newer(2, 1));
        assert!(!sequence_newer(1, 2));
        assert!(sequence_newer(1, u64::MAX));
    }
}
