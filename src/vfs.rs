//! Tiny mount dispatcher: the embedded initramfs is mounted at `/`, while the writable ATA disk
//! filesystem is exposed at `/DISK`. The two backing filesystems remain independently tested.

use crate::{diskfs, fs};

#[derive(Clone, Copy)]
pub struct EntryInfo {
    pub is_dir: bool,
    pub size: usize,
    pub on_disk: bool,
}

pub fn list_directory<F>(path: &str, mut visit: F) -> Result<usize, &'static str>
where F: FnMut(&str, bool) {
    if let Some(disk_path) = disk_path(path) {
        return diskfs::list_directory(disk_path, |name, is_dir| visit(name, is_dir));
    }
    let mut count = fs::list_directory(path, |name, is_dir| visit(name, is_dir))?;
    // Show the writable mount point as a synthetic root entry, without altering the embedded image.
    if path.trim_matches('/').is_empty() && diskfs::mount_status() && fs::find("/DISK").is_err() {
        visit("DISK", true);
        count += 1;
    }
    Ok(count)
}

pub fn read_file(path: &str, output: &mut [u8]) -> Result<usize, &'static str> {
    if let Some(disk_path) = disk_path(path) {
        return diskfs::read_file(disk_path, output);
    }
    let bytes = fs::read_file(path)?;
    if output.len() < bytes.len() { return Err("OUTPUT BUFFER IS TOO SMALL FOR FILE"); }
    output[..bytes.len()].copy_from_slice(bytes);
    Ok(bytes.len())
}

pub fn stat(path: &str) -> Result<EntryInfo, &'static str> {
    if let Some(disk_path) = disk_path(path) {
        let entry = diskfs::stat(disk_path)?;
        return Ok(EntryInfo { is_dir: entry.is_dir, size: entry.size as usize, on_disk: true });
    }
    let entry = fs::find(path)?;
    Ok(EntryInfo { is_dir: entry.is_dir, size: entry.size, on_disk: false })
}

pub fn write_file(path: &str, contents: &[u8]) -> Result<(), &'static str> {
    let disk_path = disk_path(path).ok_or("WRITE IS ONLY SUPPORTED UNDER /DISK")?;
    diskfs::write_file(disk_path, contents)
}

pub fn touch(path: &str) -> Result<(), &'static str> {
    let disk_path = disk_path(path).ok_or("TOUCH IS ONLY SUPPORTED UNDER /DISK")?;
    diskfs::touch(disk_path)
}

pub fn mkdir(path: &str) -> Result<(), &'static str> {
    let disk_path = disk_path(path).ok_or("MKDIR IS ONLY SUPPORTED UNDER /DISK")?;
    diskfs::mkdir(disk_path)
}

pub fn remove(path: &str) -> Result<(), &'static str> {
    let disk_path = disk_path(path).ok_or("RM IS ONLY SUPPORTED UNDER /DISK")?;
    diskfs::remove(disk_path)
}

pub fn disk_path(path: &str) -> Option<&str> {
    let clean = path.trim_matches('/');
    if clean.eq_ignore_ascii_case("DISK") { return Some(""); }
    let (mount, rest) = clean.split_once('/')?;
    if mount.eq_ignore_ascii_case("DISK") { Some(rest) } else { None }
}

/// Validate the root mount and the synthetic writable mount when its filesystem is available.
pub fn self_test() -> Result<(), &'static str> {
    let root = stat("/")?;
    if !root.is_dir || root.on_disk { return Err("EMBEDDED ROOT MOUNT IS INVALID"); }
    if diskfs::mount_status() {
        let disk = stat("/DISK")?;
        if !disk.is_dir || !disk.on_disk { return Err("WRITABLE MOUNT POINT IS INVALID"); }
        let mut found_disk = false;
        list_directory("/", |name, is_dir| {
            if name.eq_ignore_ascii_case("DISK") && is_dir { found_disk = true; }
        })?;
        if !found_disk { return Err("WRITABLE MOUNT IS MISSING FROM ROOT LISTING"); }
    }
    Ok(())
}
