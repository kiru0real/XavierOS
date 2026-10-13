//! Bounded polling ATA PIO driver for the primary IDE master (LBA28).
//!
//! This targets QEMU's legacy PIIX IDE controller. Operations are synchronous and
//! must only be called from normal kernel context, never from an IRQ handler.

use core::sync::atomic::{AtomicBool, AtomicU32, Ordering};

const DATA: u16 = 0x1F0;
const ERROR_FEATURES: u16 = 0x1F1;
const SECTOR_COUNT: u16 = 0x1F2;
const LBA_LOW: u16 = 0x1F3;
const LBA_MID: u16 = 0x1F4;
const LBA_HIGH: u16 = 0x1F5;
const DRIVE_HEAD: u16 = 0x1F6;
const STATUS_COMMAND: u16 = 0x1F7;
const ALT_STATUS_CONTROL: u16 = 0x3F6;

const STATUS_ERR: u8 = 0x01;
const STATUS_DRQ: u8 = 0x08;
const STATUS_DF: u8 = 0x20;
const STATUS_BSY: u8 = 0x80;
const POLL_LIMIT: usize = 2_000_000;
const LBA28_LIMIT: u32 = 0x1000_0000;

static READY: AtomicBool = AtomicBool::new(false);
static SECTORS: AtomicU32 = AtomicU32::new(0);

pub const SECTOR_SIZE: usize = 512;

#[derive(Clone, Copy)]
pub struct DriveInfo {
    pub sectors: u32,
}

pub fn initialize() -> Result<DriveInfo, &'static str> {
    READY.store(false, Ordering::Release);
    SECTORS.store(0, Ordering::Release);

    unsafe {
        outb(DRIVE_HEAD, 0xA0); // Primary master, legacy ATA device.
        delay_400ns();
        outb(SECTOR_COUNT, 0);
        outb(LBA_LOW, 0);
        outb(LBA_MID, 0);
        outb(LBA_HIGH, 0);
        outb(STATUS_COMMAND, 0xEC); // IDENTIFY DEVICE
    }

    let first_status = unsafe { inb(STATUS_COMMAND) };
    if first_status == 0 || first_status == 0xFF {
        return Err("NO ATA DEVICE FOUND ON PRIMARY MASTER");
    }

    let status = wait_not_busy()?;
    if status & STATUS_DRQ == 0 {
        return Err("ATA IDENTIFY DID NOT SET DRQ");
    }

    let mut words = [0u16; 256];
    for word in words.iter_mut() {
        *word = unsafe { inw(DATA) };
    }

    let sectors = (words[60] as u32) | ((words[61] as u32) << 16);
    if sectors == 0 {
        return Err("ATA DEVICE REPORTED ZERO LBA28 SECTORS");
    }
    if words[49] & (1 << 9) == 0 {
        return Err("ATA DEVICE DOES NOT REPORT LBA SUPPORT");
    }

    let sectors = sectors.min(LBA28_LIMIT);
    SECTORS.store(sectors, Ordering::Release);
    READY.store(true, Ordering::Release);
    Ok(DriveInfo { sectors })
}

pub fn is_ready() -> bool {
    READY.load(Ordering::Acquire)
}

pub fn sector_count() -> u32 {
    SECTORS.load(Ordering::Acquire)
}

pub fn read_sector(lba: u32, out: &mut [u8; SECTOR_SIZE]) -> Result<(), &'static str> {
    check_lba(lba)?;
    unsafe {
        select_lba(lba);
        outb(SECTOR_COUNT, 1);
        outb(LBA_LOW, lba as u8);
        outb(LBA_MID, (lba >> 8) as u8);
        outb(LBA_HIGH, (lba >> 16) as u8);
        outb(STATUS_COMMAND, 0x20); // READ SECTORS
    }
    wait_for_drq()?;
    for pair in out.chunks_exact_mut(2) {
        let word = unsafe { inw(DATA) }.to_le_bytes();
        pair[0] = word[0];
        pair[1] = word[1];
    }
    // Do not report success merely because one data phase was available. Confirm
    // the device completed the command and surfaced no late error afterwards.
    let status = wait_not_busy()?;
    if status & STATUS_DRQ != 0 {
        return Err("ATA READ DID NOT COMPLETE THE DATA PHASE");
    }
    Ok(())
}

pub fn write_sector(lba: u32, data: &[u8; SECTOR_SIZE]) -> Result<(), &'static str> {
    check_lba(lba)?;
    unsafe {
        select_lba(lba);
        outb(SECTOR_COUNT, 1);
        outb(LBA_LOW, lba as u8);
        outb(LBA_MID, (lba >> 8) as u8);
        outb(LBA_HIGH, (lba >> 16) as u8);
        outb(STATUS_COMMAND, 0x30); // WRITE SECTORS
    }
    wait_for_drq()?;
    for pair in data.chunks_exact(2) {
        unsafe { outw(DATA, u16::from_le_bytes([pair[0], pair[1]])); }
    }
    let status = wait_not_busy()?;
    if status & STATUS_DRQ != 0 {
        return Err("ATA WRITE DID NOT COMPLETE THE DATA PHASE");
    }
    unsafe { outb(STATUS_COMMAND, 0xE7); } // FLUSH CACHE
    let _ = wait_not_busy()?;
    Ok(())
}

fn check_lba(lba: u32) -> Result<(), &'static str> {
    if !is_ready() {
        return Err("ATA DEVICE IS NOT INITIALIZED");
    }
    if lba >= sector_count() || lba >= LBA28_LIMIT {
        return Err("LBA IS OUT OF RANGE");
    }
    Ok(())
}

fn select_lba(lba: u32) {
    unsafe {
        outb(DRIVE_HEAD, 0xE0 | ((lba >> 24) as u8 & 0x0F));
        delay_400ns();
    }
}

fn wait_for_drq() -> Result<(), &'static str> {
    for _ in 0..POLL_LIMIT {
        let status = unsafe { inb(STATUS_COMMAND) };
        if status == 0 || status == 0xFF {
            return Err("ATA DEVICE DISAPPEARED DURING I/O");
        }
        if status & STATUS_BSY != 0 {
            core::hint::spin_loop();
            continue;
        }
        if status & (STATUS_ERR | STATUS_DF) != 0 {
            let _error = unsafe { inb(ERROR_FEATURES) };
            return Err("ATA DEVICE REPORTED AN I/O ERROR");
        }
        if status & STATUS_DRQ != 0 {
            return Ok(());
        }
        core::hint::spin_loop();
    }
    Err("ATA DRQ WAIT TIMED OUT")
}

fn wait_not_busy() -> Result<u8, &'static str> {
    for _ in 0..POLL_LIMIT {
        let status = unsafe { inb(STATUS_COMMAND) };
        if status == 0 || status == 0xFF {
            return Err("NO ATA DEVICE RESPONSE");
        }
        if status & STATUS_BSY != 0 {
            core::hint::spin_loop();
            continue;
        }
        if status & (STATUS_ERR | STATUS_DF) != 0 {
            let _error = unsafe { inb(ERROR_FEATURES) };
            return Err("ATA DEVICE REPORTED AN ERROR");
        }
        return Ok(status);
    }
    Err("ATA BUSY WAIT TIMED OUT")
}

unsafe fn delay_400ns() {
    unsafe {
        let _ = inb(ALT_STATUS_CONTROL);
        let _ = inb(ALT_STATUS_CONTROL);
        let _ = inb(ALT_STATUS_CONTROL);
        let _ = inb(ALT_STATUS_CONTROL);
    }
}

unsafe fn outb(port: u16, value: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)); }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)); }
    value
}

unsafe fn outw(port: u16, value: u16) {
    unsafe { core::arch::asm!("out dx, ax", in("dx") port, in("ax") value, options(nomem, nostack, preserves_flags)); }
}

unsafe fn inw(port: u16) -> u16 {
    let value: u16;
    unsafe { core::arch::asm!("in ax, dx", in("dx") port, out("ax") value, options(nomem, nostack, preserves_flags)); }
    value
}
