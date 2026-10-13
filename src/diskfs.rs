//! Writable XVDISKFS v1 filesystem stored on the primary ATA disk.
//! The filesystem uses one superblock sector, a fixed metadata table, and contiguous file extents.
//! It is intentionally small and single-core; it is not crash-safe and should only be used on a
//! disposable QEMU disk image during development.

use core::cell::UnsafeCell;

use crate::ata::{self, SECTOR_SIZE};

const MAGIC: &[u8; 8] = b"XVDISK01";
const VERSION: u32 = 1;
const META_LBA: u32 = 1;
const META_SECTORS: u32 = 16;
const DATA_START: u32 = 32;
const MAX_ENTRIES: usize = 128;
const ENTRIES_PER_SECTOR: usize = SECTOR_SIZE / 64;
const NAME_CAPACITY: usize = 48;
const MAX_PATH_DEPTH: usize = 16;
const KIND_FREE: u8 = 0;
const KIND_DIR: u8 = 1;
const KIND_FILE: u8 = 2;

#[derive(Clone, Copy)]
struct Entry {
    kind: u8,
    name_len: u8,
    parent: u16,
    size: u32,
    start_lba: u32,
    sectors: u32,
    name: [u8; NAME_CAPACITY],
}

impl Entry {
    const fn empty() -> Self {
        Self { kind: KIND_FREE, name_len: 0, parent: 0, size: 0, start_lba: 0, sectors: 0, name: [0; NAME_CAPACITY] }
    }

    fn name_str(&self) -> &str {
        core::str::from_utf8(&self.name[..self.name_len as usize]).unwrap_or("")
    }
}

#[derive(Clone, Copy)]
struct State {
    mounted: bool,
    total_sectors: u32,
    last_error: &'static str,
    entries: [Entry; MAX_ENTRIES],
}

impl State {
    const fn new() -> Self {
        Self { mounted: false, total_sectors: 0, last_error: "NOT INITIALIZED", entries: [Entry::empty(); MAX_ENTRIES] }
    }
}

struct KernelCell<T>(UnsafeCell<T>);
unsafe impl<T> Sync for KernelCell<T> {}
impl<T> KernelCell<T> {
    const fn new(value: T) -> Self { Self(UnsafeCell::new(value)) }
    fn get(&self) -> *mut T { self.0.get() }
}

static STATE: KernelCell<State> = KernelCell::new(State::new());

#[derive(Clone, Copy)]
pub struct DiskInfo {
    pub available: bool,
    pub mounted: bool,
    pub total_sectors: u32,
    pub files: usize,
    pub directories: usize,
    pub free_entries: usize,
    pub used_bytes: u64,
}

#[derive(Clone, Copy)]
pub struct EntryInfo {
    pub is_dir: bool,
    pub size: u32,
}

pub fn initialize() -> Result<DiskInfo, &'static str> {
    let drive = match ata::initialize() {
        Ok(drive) => drive,
        Err(error) => {
            unsafe { (*STATE.get()).last_error = error; }
            return Err(error);
        }
    };
    if drive.sectors < DATA_START + 32 {
        unsafe { (*STATE.get()).last_error = "DISK IS TOO SMALL FOR XVDISKFS"; }
        return Err("DISK IS TOO SMALL FOR XVDISKFS");
    }
    let mut superblock = [0u8; SECTOR_SIZE];
    ata::read_sector(0, &mut superblock)?;
    if is_blank(&superblock) {
        if let Err(error) = format_with_capacity(drive.sectors) {
            unsafe { (*STATE.get()).last_error = error; }
            return Err(error);
        }
    } else if superblock.get(0..8) != Some(MAGIC.as_slice()) {
        unsafe { (*STATE.get()).last_error = "INVALID OR UNFORMATTED DISK"; }
        return Err("DISK HAS NO XVDISKFS SIGNATURE; USE FORMAT DISK TO INITIALIZE IT");
    }
    if let Err(error) = mount(drive.sectors) {
        unsafe { (*STATE.get()).last_error = error; }
        return Err(error);
    }
    info().ok_or("DISK INFO IS UNAVAILABLE")
}

pub fn mount_status() -> bool {
    unsafe { (*STATE.get()).mounted }
}

pub fn last_error() -> &'static str {
    unsafe { (*STATE.get()).last_error }
}

pub fn info() -> Option<DiskInfo> {
    let state = unsafe { &*STATE.get() };
    if !state.mounted {
        return Some(DiskInfo { available: ata::is_ready(), mounted: false, total_sectors: ata::sector_count(), files: 0, directories: 0, free_entries: MAX_ENTRIES, used_bytes: 0 });
    }
    let mut files = 0usize;
    let mut directories = 0usize;
    let mut free_entries = 0usize;
    let mut used_bytes = 0u64;
    for entry in &state.entries {
        match entry.kind {
            KIND_FREE => free_entries += 1,
            KIND_DIR => directories += 1,
            KIND_FILE => { files += 1; used_bytes = used_bytes.saturating_add(entry.size as u64); },
            _ => return None,
        }
    }
    Some(DiskInfo { available: ata::is_ready(), mounted: true, total_sectors: state.total_sectors, files, directories, free_entries, used_bytes })
}

pub fn format() -> Result<(), &'static str> {
    unsafe { (*STATE.get()).mounted = false; (*STATE.get()).last_error = "FORMATTING"; }
    if !ata::is_ready() {
        if let Err(error) = ata::initialize() {
            unsafe { (*STATE.get()).last_error = error; }
            return Err(error);
        }
    }
    if let Err(error) = format_with_capacity(ata::sector_count()) {
        unsafe { (*STATE.get()).last_error = error; }
        return Err(error);
    }
    if let Err(error) = mount(ata::sector_count()) {
        unsafe { (*STATE.get()).last_error = error; }
        return Err(error);
    }
    Ok(())
}

fn format_with_capacity(total_sectors: u32) -> Result<(), &'static str> {
    if total_sectors < DATA_START + 32 {
        return Err("DISK IS TOO SMALL TO FORMAT");
    }
    let mut superblock = [0u8; SECTOR_SIZE];
    superblock[0..8].copy_from_slice(MAGIC);
    put_u32(&mut superblock, 8, VERSION);
    put_u32(&mut superblock, 12, total_sectors);
    put_u32(&mut superblock, 16, META_LBA);
    put_u32(&mut superblock, 20, META_SECTORS);
    put_u32(&mut superblock, 24, DATA_START);
    put_u32(&mut superblock, 28, MAX_ENTRIES as u32);
    // A compact checksum detects accidental corruption of the fixed superblock fields.
    let checksum = checksum32(&superblock[0..32]);
    put_u32(&mut superblock, 32, checksum);

    let zero = [0u8; SECTOR_SIZE];
    for sector in 0..META_SECTORS {
        ata::write_sector(META_LBA + sector, &zero)?;
    }

    let mut root = Entry::empty();
    root.kind = KIND_DIR;
    root.name_len = 1;
    root.name[0] = b'/';
    write_entry_to_disk(0, &root)?;
    // Commit the superblock last so a blank disk remains visibly unformatted if
    // initialization of its metadata table fails part-way through.
    ata::write_sector(0, &superblock)?;
    Ok(())
}

fn mount(device_sectors: u32) -> Result<(), &'static str> {
    let mut superblock = [0u8; SECTOR_SIZE];
    ata::read_sector(0, &mut superblock)?;
    if superblock.get(0..8) != Some(MAGIC.as_slice()) {
        return Err("XVDISKFS SIGNATURE MISMATCH");
    }
    if get_u32(&superblock, 8) != Some(VERSION) {
        return Err("UNSUPPORTED XVDISKFS VERSION");
    }
    if get_u32(&superblock, 16) != Some(META_LBA)
        || get_u32(&superblock, 20) != Some(META_SECTORS)
        || get_u32(&superblock, 24) != Some(DATA_START)
        || get_u32(&superblock, 28) != Some(MAX_ENTRIES as u32)
        || get_u32(&superblock, 32) != Some(checksum32(&superblock[0..32]))
    {
        return Err("XVDISKFS SUPERBLOCK VALIDATION FAILED");
    }
    let declared_sectors = get_u32(&superblock, 12).ok_or("INVALID DISK SIZE FIELD")?;
    if declared_sectors > device_sectors || declared_sectors < DATA_START + 1 {
        return Err("XVDISKFS DISK SIZE DOES NOT MATCH DEVICE");
    }

    let state = unsafe { &mut *STATE.get() };
    state.mounted = false;
    state.total_sectors = declared_sectors;
    state.last_error = "NONE";
    for sector_index in 0..META_SECTORS {
        let mut sector = [0u8; SECTOR_SIZE];
        ata::read_sector(META_LBA + sector_index, &mut sector)?;
        for item in 0..ENTRIES_PER_SECTOR {
            let index = sector_index as usize * ENTRIES_PER_SECTOR + item;
            if index >= MAX_ENTRIES { break; }
            state.entries[index] = decode_entry(&sector[item * 64..item * 64 + 64])?;
        }
    }
    validate_entries(state)?;
    if state.entries[0].kind != KIND_DIR || state.entries[0].name_str() != "/" {
        return Err("XVDISKFS ROOT DIRECTORY IS INVALID");
    }
    state.mounted = true;
    state.last_error = "NONE";
    Ok(())
}

fn validate_entries(state: &State) -> Result<(), &'static str> {
    for (index, entry) in state.entries.iter().enumerate() {
        match entry.kind {
            KIND_FREE => continue,
            KIND_DIR | KIND_FILE => {},
            _ => return Err("INVALID DISK ENTRY TYPE"),
        }
        if index == 0 { continue; }
        if entry.name_len == 0 || entry.name_len as usize > NAME_CAPACITY {
            return Err("INVALID DISK ENTRY NAME LENGTH");
        }
        if entry.name[..entry.name_len as usize].iter().any(|b| *b < 0x20 || *b > 0x7E || *b == b'/') {
            return Err("INVALID DISK ENTRY NAME");
        }
        let parent = entry.parent as usize;
        if parent >= MAX_ENTRIES || state.entries[parent].kind != KIND_DIR {
            return Err("DISK ENTRY REFERENCES AN INVALID PARENT");
        }
        if entry.kind == KIND_FILE {
            let end = entry.start_lba.checked_add(entry.sectors).ok_or("FILE EXTENT OVERFLOW")?;
            if entry.sectors == 0 {
                if entry.size != 0 { return Err("EMPTY FILE HAS NONZERO SIZE"); }
            } else if entry.start_lba < DATA_START || end > state.total_sectors || entry.size > entry.sectors.saturating_mul(SECTOR_SIZE as u32) {
                return Err("FILE EXTENT IS OUT OF RANGE");
            }
        } else if entry.size != 0 || entry.sectors != 0 {
            return Err("DIRECTORY HAS FILE DATA");
        }
        for other in index + 1..MAX_ENTRIES {
            let sibling = &state.entries[other];
            if sibling.kind != KIND_FREE && sibling.parent == entry.parent && sibling.name_str().eq_ignore_ascii_case(entry.name_str()) {
                return Err("DUPLICATE SIBLING NAME IN DISK DIRECTORY");
            }
        }
    }
    // Extents belonging to distinct files must not overlap.
    for i in 0..MAX_ENTRIES {
        let a = &state.entries[i];
        if a.kind != KIND_FILE || a.sectors == 0 { continue; }
        let a_end = a.start_lba.saturating_add(a.sectors);
        for j in i + 1..MAX_ENTRIES {
            let b = &state.entries[j];
            if b.kind != KIND_FILE || b.sectors == 0 { continue; }
            let b_end = b.start_lba.saturating_add(b.sectors);
            if a.start_lba < b_end && b.start_lba < a_end {
                return Err("OVERLAPPING FILE EXTENTS ON DISK");
            }
        }
    }
    Ok(())
}

pub fn list_directory<F>(path: &str, mut visit: F) -> Result<usize, &'static str>
where F: FnMut(&str, bool) {
    let state = state_ref()?;
    let index = resolve(state, path)?;
    let directory = &state.entries[index];
    if directory.kind != KIND_DIR { return Err("PATH IS NOT A DIRECTORY"); }
    let mut count = 0usize;
    for entry in &state.entries {
        if entry.kind == KIND_FREE || entry.parent as usize != index || (index == 0 && entry.name_str() == "/") { continue; }
        visit(entry.name_str(), entry.kind == KIND_DIR);
        count += 1;
    }
    Ok(count)
}

pub fn stat(path: &str) -> Result<EntryInfo, &'static str> {
    let state = state_ref()?;
    let index = resolve(state, path)?;
    let entry = &state.entries[index];
    Ok(EntryInfo { is_dir: entry.kind == KIND_DIR, size: entry.size })
}

pub fn read_file(path: &str, output: &mut [u8]) -> Result<usize, &'static str> {
    let state = state_ref()?;
    let index = resolve(state, path)?;
    let entry = state.entries[index];
    if entry.kind != KIND_FILE { return Err("PATH IS NOT A FILE"); }
    let size = entry.size as usize;
    if output.len() < size { return Err("OUTPUT BUFFER IS TOO SMALL FOR FILE"); }
    let mut copied = 0usize;
    let mut sector = [0u8; SECTOR_SIZE];
    for relative in 0..entry.sectors {
        if copied >= size { break; }
        ata::read_sector(entry.start_lba + relative, &mut sector)?;
        let amount = (size - copied).min(SECTOR_SIZE);
        output[copied..copied + amount].copy_from_slice(&sector[..amount]);
        copied += amount;
    }
    Ok(copied)
}

pub fn write_file(path: &str, data: &[u8]) -> Result<(), &'static str> {
    if data.len() > u32::MAX as usize { return Err("FILE IS TOO LARGE"); }
    let clean = clean_path(path)?;
    if clean.is_empty() { return Err("ROOT IS NOT A FILE"); }
    let (parent_path, leaf) = split_parent(clean)?;
    validate_leaf(leaf)?;
    let state_ptr = STATE.get();
    let state = unsafe { &mut *state_ptr };
    ensure_mounted(state)?;
    let parent = resolve(state, parent_path)?;
    if state.entries[parent].kind != KIND_DIR { return Err("PARENT PATH IS NOT A DIRECTORY"); }
    let existing = find_child(state, parent, leaf);
    let index = if let Some(index) = existing {
        if state.entries[index].kind != KIND_FILE { return Err("PATH ALREADY EXISTS AS A DIRECTORY"); }
        index
    } else {
        free_entry(state).ok_or("DISK DIRECTORY TABLE IS FULL")?
    };
    let sectors_needed = ((data.len() + SECTOR_SIZE - 1) / SECTOR_SIZE) as u32;
    // When replacing an existing file, keep its old extent reserved until all new data
    // and metadata have been committed. If space is insufficient, fail without damaging it.
    let start_lba = if sectors_needed == 0 { 0 } else { find_extent(state, sectors_needed, existing)? };
    let mut source_offset = 0usize;
    for sector_index in 0..sectors_needed {
        let mut sector = [0u8; SECTOR_SIZE];
        let amount = (data.len() - source_offset).min(SECTOR_SIZE);
        sector[..amount].copy_from_slice(&data[source_offset..source_offset + amount]);
        ata::write_sector(start_lba + sector_index, &sector)?;
        source_offset += amount;
    }

    let mut new_entry = Entry::empty();
    new_entry.kind = KIND_FILE;
    new_entry.name_len = leaf.len() as u8;
    new_entry.parent = parent as u16;
    new_entry.size = data.len() as u32;
    new_entry.start_lba = start_lba;
    new_entry.sectors = sectors_needed;
    new_entry.name[..leaf.len()].copy_from_slice(leaf.as_bytes());
    let previous_entry = state.entries[index];
    state.entries[index] = new_entry;
    if let Err(error) = write_metadata_sector(index / ENTRIES_PER_SECTOR, state) {
        state.entries[index] = previous_entry;
        return Err(error);
    }
    Ok(())
}

pub fn touch(path: &str) -> Result<(), &'static str> {
    let exists = {
        let state = state_ref()?;
        resolve(state, path).is_ok()
    };
    if exists { return Err("PATH ALREADY EXISTS; TOUCH DOES NOT TRUNCATE FILES"); }
    write_file(path, &[])
}

pub fn mkdir(path: &str) -> Result<(), &'static str> {
    let clean = clean_path(path)?;
    if clean.is_empty() { return Err("ROOT DIRECTORY ALREADY EXISTS"); }
    let (parent_path, leaf) = split_parent(clean)?;
    validate_leaf(leaf)?;
    let state_ptr = STATE.get();
    let state = unsafe { &mut *state_ptr };
    ensure_mounted(state)?;
    let parent = resolve(state, parent_path)?;
    if state.entries[parent].kind != KIND_DIR { return Err("PARENT PATH IS NOT A DIRECTORY"); }
    if find_child(state, parent, leaf).is_some() { return Err("PATH ALREADY EXISTS"); }
    let index = free_entry(state).ok_or("DISK DIRECTORY TABLE IS FULL")?;
    let mut entry = Entry::empty();
    entry.kind = KIND_DIR;
    entry.name_len = leaf.len() as u8;
    entry.parent = parent as u16;
    entry.name[..leaf.len()].copy_from_slice(leaf.as_bytes());
    state.entries[index] = entry;
    if let Err(error) = write_metadata_sector(index / ENTRIES_PER_SECTOR, state) {
        state.entries[index] = Entry::empty();
        return Err(error);
    }
    Ok(())
}

pub fn remove(path: &str) -> Result<(), &'static str> {
    let clean = clean_path(path)?;
    if clean.is_empty() { return Err("CANNOT REMOVE ROOT DIRECTORY"); }
    let state_ptr = STATE.get();
    let state = unsafe { &mut *state_ptr };
    ensure_mounted(state)?;
    let index = resolve(state, clean)?;
    if index == 0 { return Err("CANNOT REMOVE ROOT DIRECTORY"); }
    let entry = state.entries[index];
    if entry.kind == KIND_DIR && state.entries.iter().any(|candidate| candidate.kind != KIND_FREE && candidate.parent as usize == index) {
        return Err("DIRECTORY IS NOT EMPTY");
    }
    state.entries[index] = Entry::empty();
    if let Err(error) = write_metadata_sector(index / ENTRIES_PER_SECTOR, state) {
        state.entries[index] = entry;
        return Err(error);
    }
    Ok(())
}

pub fn self_test() -> Result<(), &'static str> {
    let state = state_ref()?;
    if state.total_sectors < DATA_START + 1 { return Err("DISK IS TOO SMALL"); }
    if state.entries[0].kind != KIND_DIR || state.entries[0].name_str() != "/" { return Err("ROOT ENTRY INVALID"); }
    validate_entries(state)?;
    Ok(())
}

fn state_ref() -> Result<&'static State, &'static str> {
    let state = unsafe { &*STATE.get() };
    if !state.mounted { return Err("WRITABLE DISK FILESYSTEM IS NOT MOUNTED"); }
    Ok(state)
}

fn ensure_mounted(state: &State) -> Result<(), &'static str> {
    if !state.mounted || !ata::is_ready() { return Err("WRITABLE DISK FILESYSTEM IS NOT READY"); }
    Ok(())
}

fn resolve(state: &State, path: &str) -> Result<usize, &'static str> {
    let clean = clean_path(path)?;
    if clean.is_empty() { return Ok(0); }
    let mut parent = 0usize;
    let mut depth = 0usize;
    for component in clean.split('/') {
        depth += 1;
        if depth > MAX_PATH_DEPTH { return Err("PATH IS TOO DEEP"); }
        validate_leaf(component)?;
        parent = find_child(state, parent, component).ok_or("PATH NOT FOUND")?;
    }
    Ok(parent)
}

fn find_child(state: &State, parent: usize, name: &str) -> Option<usize> {
    state.entries.iter().enumerate().find_map(|(index, entry)| {
        if entry.kind != KIND_FREE && index != 0 && entry.parent as usize == parent && entry.name_str().eq_ignore_ascii_case(name) { Some(index) } else { None }
    })
}

fn free_entry(state: &State) -> Option<usize> {
    (1..MAX_ENTRIES).find(|index| state.entries[*index].kind == KIND_FREE)
}

fn find_extent(state: &State, needed: u32, replacing: Option<usize>) -> Result<u32, &'static str> {
    if needed == 0 { return Ok(0); }
    let mut candidate = DATA_START;
    loop {
        let mut next_start = state.total_sectors;
        let mut next_end = state.total_sectors;
        for (index, entry) in state.entries.iter().enumerate() {
            if Some(index) == replacing || entry.kind != KIND_FILE || entry.sectors == 0 { continue; }
            let end = entry.start_lba.checked_add(entry.sectors).ok_or("FILE EXTENT OVERFLOW")?;
            if entry.start_lba < candidate && end > candidate { candidate = end; }
            if entry.start_lba >= candidate && entry.start_lba < next_start {
                next_start = entry.start_lba;
                next_end = end;
            }
        }
        if candidate.checked_add(needed).ok_or("FILE SIZE OVERFLOW")? <= next_start {
            return Ok(candidate);
        }
        if next_start >= state.total_sectors { return Err("DISK IS FULL OR TOO FRAGMENTED"); }
        candidate = next_end;
        if candidate.checked_add(needed).ok_or("FILE SIZE OVERFLOW")? > state.total_sectors {
            return Err("DISK IS FULL OR TOO FRAGMENTED");
        }
    }
}

fn clean_path(path: &str) -> Result<&str, &'static str> {
    let clean = path.trim_matches('/');
    if clean.len() > 768 { return Err("PATH IS TOO LONG"); }
    if clean.contains("//") || clean.split('/').any(|part| part == "." || part == "..") { return Err("INVALID PATH COMPONENT"); }
    Ok(clean)
}

fn split_parent(path: &str) -> Result<(&str, &str), &'static str> {
    match path.rsplit_once('/') {
        Some((parent, leaf)) if !leaf.is_empty() => Ok((parent, leaf)),
        Some(_) => Err("EMPTY FILE OR DIRECTORY NAME"),
        None => Ok(("", path)),
    }
}

fn validate_leaf(name: &str) -> Result<(), &'static str> {
    if name.is_empty() || name.len() > NAME_CAPACITY { return Err("NAME MUST BE 1-48 BYTES"); }
    if name == "." || name == ".." || name.bytes().any(|b| b == b'/' || b == 0 || b < 0x20 || b > 0x7E) {
        return Err("NAME MUST CONTAIN PRINTABLE ASCII WITHOUT SLASHES");
    }
    Ok(())
}

fn write_metadata_sector(sector_index: usize, state: &State) -> Result<(), &'static str> {
    if sector_index >= META_SECTORS as usize { return Err("METADATA INDEX OUT OF RANGE"); }
    let mut sector = [0u8; SECTOR_SIZE];
    for item in 0..ENTRIES_PER_SECTOR {
        let index = sector_index * ENTRIES_PER_SECTOR + item;
        encode_entry(&state.entries[index], &mut sector[item * 64..item * 64 + 64]);
    }
    ata::write_sector(META_LBA + sector_index as u32, &sector)
}

fn write_entry_to_disk(index: usize, entry: &Entry) -> Result<(), &'static str> {
    let mut sector = [0u8; SECTOR_SIZE];
    let sector_index = index / ENTRIES_PER_SECTOR;
    ata::read_sector(META_LBA + sector_index as u32, &mut sector)?;
    encode_entry(entry, &mut sector[(index % ENTRIES_PER_SECTOR) * 64..(index % ENTRIES_PER_SECTOR + 1) * 64]);
    ata::write_sector(META_LBA + sector_index as u32, &sector)
}

fn encode_entry(entry: &Entry, bytes: &mut [u8]) {
    bytes.fill(0);
    bytes[0] = entry.kind;
    bytes[1] = entry.name_len;
    bytes[2..4].copy_from_slice(&entry.parent.to_le_bytes());
    bytes[4..8].copy_from_slice(&entry.size.to_le_bytes());
    bytes[8..12].copy_from_slice(&entry.start_lba.to_le_bytes());
    bytes[12..16].copy_from_slice(&entry.sectors.to_le_bytes());
    bytes[16..64].copy_from_slice(&entry.name);
}

fn decode_entry(bytes: &[u8]) -> Result<Entry, &'static str> {
    if bytes.len() != 64 { return Err("INVALID METADATA RECORD SIZE"); }
    let mut entry = Entry::empty();
    entry.kind = bytes[0];
    entry.name_len = bytes[1];
    entry.parent = u16::from_le_bytes([bytes[2], bytes[3]]);
    entry.size = u32::from_le_bytes([bytes[4], bytes[5], bytes[6], bytes[7]]);
    entry.start_lba = u32::from_le_bytes([bytes[8], bytes[9], bytes[10], bytes[11]]);
    entry.sectors = u32::from_le_bytes([bytes[12], bytes[13], bytes[14], bytes[15]]);
    entry.name.copy_from_slice(&bytes[16..64]);
    if entry.kind == KIND_FREE { return Ok(Entry::empty()); }
    if entry.name_len as usize > NAME_CAPACITY { return Err("INVALID STORED NAME LENGTH"); }
    Ok(entry)
}

fn is_blank(data: &[u8; SECTOR_SIZE]) -> bool { data.iter().all(|byte| *byte == 0) }

fn put_u32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}

fn get_u32(bytes: &[u8], offset: usize) -> Option<u32> {
    let slice = bytes.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([slice[0], slice[1], slice[2], slice[3]]))
}

fn checksum32(bytes: &[u8]) -> u32 {
    bytes.iter().fold(0xA5A5_5A5A, |acc, byte| acc.rotate_left(3) ^ (*byte as u32))
}
