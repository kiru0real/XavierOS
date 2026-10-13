//! Read-only XavierOS initramfs (XVRFS v1).
//!
//! The image is generated from `rootfs/` by `mkinitramfs.py` and embedded in
//! the kernel ELF. This first version is intentionally read-only: it provides
//! a useful VFS-like API without introducing a disk driver yet.

const IMAGE: &[u8] = include_bytes!("../initramfs.xfs");
const MAGIC: &[u8; 8] = b"XVRFS001";
const HEADER_SIZE: usize = 16;
const RECORD_SIZE: usize = 20;
const KIND_DIRECTORY: u8 = 1;
const KIND_FILE: u8 = 2;

#[derive(Clone, Copy)]
pub struct Entry {
    pub path: &'static str,
    pub is_dir: bool,
    pub size: usize,
    pub data: &'static [u8],
}

#[derive(Clone, Copy)]
pub struct FsInfo {
    pub entries: usize,
    pub image_bytes: usize,
    pub files: usize,
    pub directories: usize,
    pub file_bytes: usize,
}

fn read_u16(offset: usize) -> Option<u16> {
    let bytes = IMAGE.get(offset..offset.checked_add(2)?)?;
    Some(u16::from_le_bytes([bytes[0], bytes[1]]))
}

fn read_u32(offset: usize) -> Option<u32> {
    let bytes = IMAGE.get(offset..offset.checked_add(4)?)?;
    Some(u32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]))
}

fn header() -> Result<(usize, usize), &'static str> {
    if IMAGE.len() < HEADER_SIZE {
        return Err("INITRAMFS HEADER IS TRUNCATED");
    }
    if IMAGE.get(0..8) != Some(MAGIC.as_slice()) {
        return Err("INITRAMFS MAGIC IS INVALID");
    }
    let count = read_u32(8).ok_or("INITRAMFS ENTRY COUNT IS INVALID")? as usize;
    let table_offset = read_u32(12).ok_or("INITRAMFS TABLE OFFSET IS INVALID")? as usize;
    if table_offset < HEADER_SIZE {
        return Err("INITRAMFS TABLE OVERLAPS HEADER");
    }
    let table_bytes = count.checked_mul(RECORD_SIZE).ok_or("INITRAMFS TABLE SIZE OVERFLOW")?;
    let table_end = table_offset.checked_add(table_bytes).ok_or("INITRAMFS TABLE END OVERFLOW")?;
    if table_end > IMAGE.len() {
        return Err("INITRAMFS TABLE IS TRUNCATED");
    }
    Ok((count, table_offset))
}

fn entry_at(index: usize, count: usize, table_offset: usize) -> Result<Entry, &'static str> {
    if index >= count {
        return Err("INITRAMFS ENTRY INDEX OUT OF RANGE");
    }
    let record = table_offset
        .checked_add(index.checked_mul(RECORD_SIZE).ok_or("INITRAMFS RECORD OFFSET OVERFLOW")?)
        .ok_or("INITRAMFS RECORD OFFSET OVERFLOW")?;
    let kind = *IMAGE.get(record).ok_or("INITRAMFS RECORD IS TRUNCATED")?;
    if kind != KIND_DIRECTORY && kind != KIND_FILE {
        return Err("INITRAMFS ENTRY TYPE IS INVALID");
    }
    let path_offset = read_u32(record + 4).ok_or("INITRAMFS PATH OFFSET IS INVALID")? as usize;
    let path_len = read_u16(record + 8).ok_or("INITRAMFS PATH LENGTH IS INVALID")? as usize;
    let data_offset = read_u32(record + 12).ok_or("INITRAMFS DATA OFFSET IS INVALID")? as usize;
    let data_len = read_u32(record + 16).ok_or("INITRAMFS DATA LENGTH IS INVALID")? as usize;
    if path_len == 0 || path_len > 1024 {
        return Err("INITRAMFS PATH LENGTH IS INVALID");
    }
    let path_end = path_offset.checked_add(path_len).ok_or("INITRAMFS PATH END OVERFLOW")?;
    let path_bytes = IMAGE.get(path_offset..path_end).ok_or("INITRAMFS PATH IS OUT OF BOUNDS")?;
    let path = core::str::from_utf8(path_bytes).map_err(|_| "INITRAMFS PATH IS NOT UTF-8")?;
    if path.starts_with('/') || path.contains("//") || path.split('/').any(|part| part == ".." || part == ".") {
        return Err("INITRAMFS PATH IS NOT CANONICAL");
    }
    let data = if kind == KIND_FILE {
        let data_end = data_offset.checked_add(data_len).ok_or("INITRAMFS DATA END OVERFLOW")?;
        IMAGE.get(data_offset..data_end).ok_or("INITRAMFS FILE DATA IS OUT OF BOUNDS")?
    } else {
        if data_len != 0 {
            return Err("DIRECTORY ENTRY CONTAINS FILE DATA");
        }
        &IMAGE[0..0]
    };
    Ok(Entry { path, is_dir: kind == KIND_DIRECTORY, size: data_len, data })
}

fn clean_path(path: &str) -> Result<&str, &'static str> {
    let clean = path.trim_matches('/');
    if clean.len() > 1024 {
        return Err("PATH IS TOO LONG");
    }
    if clean.split('/').any(|part| part == ".." || part == ".") {
        return Err("DOT PATH COMPONENTS ARE NOT SUPPORTED");
    }
    if clean.contains("//") {
        return Err("EMPTY PATH COMPONENT IS NOT SUPPORTED");
    }
    Ok(clean)
}

pub fn find(path: &str) -> Result<Entry, &'static str> {
    let clean = clean_path(path)?;
    if clean.is_empty() {
        return Ok(Entry { path: "/", is_dir: true, size: 0, data: &IMAGE[0..0] });
    }
    let (count, table_offset) = header()?;
    for index in 0..count {
        let entry = entry_at(index, count, table_offset)?;
        if entry.path.eq_ignore_ascii_case(clean) {
            return Ok(entry);
        }
    }
    Err("PATH NOT FOUND")
}

pub fn read_file(path: &str) -> Result<&'static [u8], &'static str> {
    let entry = find(path)?;
    if entry.is_dir {
        return Err("PATH IS A DIRECTORY");
    }
    Ok(entry.data)
}

fn suffix_in_directory<'a>(entry_path: &'a str, directory: &str) -> Option<&'a str> {
    if directory.is_empty() {
        return Some(entry_path);
    }
    let prefix = entry_path.get(..directory.len())?;
    if !prefix.eq_ignore_ascii_case(directory) {
        return None;
    }
    entry_path.get(directory.len()..)?.strip_prefix('/')
}

fn seen_before(parent: &str, child: &str, before: usize, count: usize, table_offset: usize) -> Result<bool, &'static str> {
    for index in 0..before {
        let previous = entry_at(index, count, table_offset)?;
        if let Some(suffix) = suffix_in_directory(previous.path, parent) {
            if suffix.is_empty() {
                continue;
            }
            let previous_child = suffix.split('/').next().unwrap_or(suffix);
            if previous_child.eq_ignore_ascii_case(child) {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

/// Visit each immediate child exactly once. The callback receives child name and directory flag.
pub fn list_directory<F>(path: &str, mut visit: F) -> Result<usize, &'static str>
where
    F: FnMut(&str, bool),
{
    let parent = clean_path(path)?;
    if !parent.is_empty() && !find(parent)?.is_dir {
        return Err("PATH IS NOT A DIRECTORY");
    }
    let (count, table_offset) = header()?;
    let mut listed = 0usize;
    for index in 0..count {
        let entry = entry_at(index, count, table_offset)?;
        let Some(suffix) = suffix_in_directory(entry.path, parent) else { continue };
        if suffix.is_empty() {
            continue;
        }
        let child = suffix.split('/').next().unwrap_or(suffix);
        if child.is_empty() || seen_before(parent, child, index, count, table_offset)? {
            continue;
        }
        let is_dir = suffix.contains('/') || entry.is_dir;
        visit(child, is_dir);
        listed += 1;
    }
    Ok(listed)
}

pub fn info() -> Result<FsInfo, &'static str> {
    let (count, table_offset) = header()?;
    let mut files = 0usize;
    let mut directories = 0usize;
    let mut file_bytes = 0usize;
    for index in 0..count {
        let entry = entry_at(index, count, table_offset)?;
        if entry.is_dir {
            directories += 1;
        } else {
            files += 1;
            file_bytes = file_bytes.checked_add(entry.size).ok_or("INITRAMFS SIZE OVERFLOW")?;
        }
    }
    Ok(FsInfo { entries: count, image_bytes: IMAGE.len(), files, directories, file_bytes })
}

pub fn self_test() -> Result<(), &'static str> {
    let info = info()?;
    if info.entries == 0 {
        return Err("INITRAMFS HAS NO ENTRIES");
    }
    if info.files == 0 || info.directories == 0 {
        return Err("INITRAMFS NEEDS FILES AND DIRECTORIES");
    }
    if read_file("/README.TXT").is_err() {
        return Err("README.TXT SELF-TEST FAILED");
    }
    if read_file("/ETC/ISSUE").is_err() {
        return Err("ETC/ISSUE SELF-TEST FAILED");
    }
    let mut root_items = 0usize;
    list_directory("/", |_, _| root_items += 1)?;
    if root_items == 0 {
        return Err("ROOT DIRECTORY IS EMPTY");
    }
    Ok(())
}
