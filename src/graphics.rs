//! Renderer V2: software backbuffer plus tile-based dirty-region presentation.
//!
//! Drawing remains CPU-rendered, but UI code paints into a RAM backbuffer. Dirty
//! 32x32 tiles are copied to the Limine framebuffer only after a logical update.
//! There is no hardware page-flip/vblank synchronization or GPU acceleration.

use core::ptr::{addr_of, addr_of_mut, read_volatile, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use crate::Framebuffer;

pub const MAX_WIDTH: usize = 1366;
pub const MAX_HEIGHT: usize = 768;
pub const TILE_SIZE: usize = 32;
const MAX_TILES_X: usize = MAX_WIDTH.div_ceil(TILE_SIZE);
const MAX_TILES_Y: usize = MAX_HEIGHT.div_ceil(TILE_SIZE);
const TILE_COUNT: usize = MAX_TILES_X * MAX_TILES_Y;
const DAMAGE_WORDS: usize = TILE_COUNT.div_ceil(64);
const PIXELS: usize = MAX_WIDTH * MAX_HEIGHT;

static mut BACKBUFFER: [u32; PIXELS] = [0; PIXELS];
static mut FRONT: Option<Framebuffer> = None;
static mut ACTIVE_WIDTH: usize = 0;
static mut ACTIVE_HEIGHT: usize = 0;
static mut BACK_PITCH: usize = 0;
static mut DAMAGE: [u64; DAMAGE_WORDS] = [0; DAMAGE_WORDS];
static ACTIVE: AtomicBool = AtomicBool::new(false);
static FRAME_COUNT: AtomicU64 = AtomicU64::new(0);
static PRESENTED_TILES: AtomicU64 = AtomicU64::new(0);
static PRESENTED_PIXELS: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy)]
pub struct GraphicsStats {
    pub active: bool,
    pub width: usize,
    pub height: usize,
    pub frames: u64,
    pub tiles_presented: u64,
    pub pixels_presented: u64,
}

/// Initialize the backbuffer after Login Manager has finished drawing directly
/// to the boot framebuffer. Returns a framebuffer descriptor targeting RAM.
pub fn initialize(front: &Framebuffer) -> Option<Framebuffer> {
    if front.bpp != 32 || front.width == 0 || front.height == 0 { return None; }
    let width = front.width as usize;
    let height = front.height as usize;
    if width > MAX_WIDTH || height > MAX_HEIGHT || front.pitch < (width * 4) as u64 { return None; }

    unsafe {
        ACTIVE.store(false, Ordering::Release);
        ACTIVE_WIDTH = width;
        ACTIVE_HEIGHT = height;
        BACK_PITCH = width * 4;
        FRONT = Some(*front);
        let back = addr_of_mut!(BACKBUFFER).cast::<u32>();
        for y in 0..height {
            let source = front.address.add(y * front.pitch as usize).cast::<u32>();
            let target = back.add(y * width);
            for x in 0..width {
                write_volatile(target.add(x), read_volatile(source.add(x)));
            }
        }
        DAMAGE = [0; DAMAGE_WORDS];
        ACTIVE.store(true, Ordering::Release);
    }
    mark_full();
    Some(backbuffer_descriptor(*front))
}

/// Get the current CPU backbuffer as a Limine-compatible framebuffer descriptor.
pub fn backbuffer_descriptor(mut descriptor: Framebuffer) -> Framebuffer {
    if !is_active() { return descriptor; }
    unsafe {
        descriptor.address = addr_of_mut!(BACKBUFFER).cast::<u8>();
        descriptor.pitch = BACK_PITCH as u64;
        descriptor
    }
}

pub fn is_active() -> bool { ACTIVE.load(Ordering::Acquire) }

/// True when a framebuffer view points into the RAM backbuffer. Drawing to this
/// region does not need volatile stores; the volatile copy happens in `present()`.
pub fn is_backbuffer_view(fb: &Framebuffer) -> bool {
    if !is_active() || fb.address.is_null() { return false; }
    let (base, length, pitch) = unsafe {
        (addr_of!(BACKBUFFER) as usize, PIXELS * 4, BACK_PITCH)
    };
    let address = fb.address as usize;
    address >= base
        && address < base.saturating_add(length)
        && fb.pitch as usize == pitch
}

pub fn frontbuffer_address() -> Option<*mut u8> {
    if !is_active() { return None; }
    unsafe { FRONT.map(|front| front.address) }
}

/// Apply a successful live VBE mode change to both front and backbuffer metadata.
pub fn reconfigure_frontbuffer(address: *mut u8, width: usize, height: usize, pitch: usize) -> Option<Framebuffer> {
    if !is_active() || address.is_null() || width == 0 || height == 0
        || width > MAX_WIDTH || height > MAX_HEIGHT || pitch < width * 4
    { return None; }
    unsafe {
        let mut front = FRONT?;
        front.address = address;
        front.width = width as u64;
        front.height = height as u64;
        front.pitch = pitch as u64;
        FRONT = Some(front);
        ACTIVE_WIDTH = width;
        ACTIVE_HEIGHT = height;
        BACK_PITCH = width * 4;
        let back = addr_of_mut!(BACKBUFFER).cast::<u32>();
        for y in 0..height {
            let source = address.add(y * pitch).cast::<u32>();
            let target = back.add(y * width);
            for x in 0..width {
                write_volatile(target.add(x), read_volatile(source.add(x)));
            }
        }
        DAMAGE = [0; DAMAGE_WORDS];
        let mut shadow = front;
        shadow.address = back.cast::<u8>();
        shadow.pitch = BACK_PITCH as u64;
        mark_full();
        Some(shadow)
    }
}

/// Mark a rectangle relative to any framebuffer view. This also supports the
/// Terminal sub-framebuffer whose base pointer begins inside the desktop backbuffer.
pub fn mark_rect(fb: &Framebuffer, x: usize, y: usize, width: usize, height: usize) {
    if !is_active() || width == 0 || height == 0 { return; }
    let (screen_width, screen_height, back_pitch, base) = unsafe {
        (ACTIVE_WIDTH, ACTIVE_HEIGHT, BACK_PITCH, addr_of!(BACKBUFFER) as usize)
    };
    let address = fb.address as usize;
    let Some(offset) = address.checked_sub(base) else { return; };
    if offset >= PIXELS * 4 || fb.pitch == 0 || fb.pitch as usize != back_pitch { return; }
    let origin_y = offset / back_pitch;
    let origin_x = (offset % back_pitch) / 4;
    if origin_x >= screen_width || origin_y >= screen_height { return; }
    // Clip against the view before translating it into desktop coordinates.
    // Using min(width, view_width) without subtracting x/y can dirty tiles
    // outside a sub-framebuffer such as the Terminal window.
    let local_left = x.min(fb.width as usize);
    let local_top = y.min(fb.height as usize);
    let local_right = x.saturating_add(width).min(fb.width as usize);
    let local_bottom = y.saturating_add(height).min(fb.height as usize);
    let left = origin_x.saturating_add(local_left).min(screen_width);
    let top = origin_y.saturating_add(local_top).min(screen_height);
    let right = origin_x.saturating_add(local_right).min(screen_width);
    let bottom = origin_y.saturating_add(local_bottom).min(screen_height);
    if left >= right || top >= bottom { return; }
    mark_absolute(left, top, right - left, bottom - top, screen_width, screen_height);
}

pub fn mark_absolute(x: usize, y: usize, width: usize, height: usize, screen_width: usize, screen_height: usize) {
    if !is_active() || width == 0 || height == 0 { return; }
    let active_width = unsafe { ACTIVE_WIDTH };
    let active_height = unsafe { ACTIVE_HEIGHT };
    if screen_width != active_width || screen_height != active_height { return; }
    let left = x.min(active_width);
    let top = y.min(active_height);
    let right = x.saturating_add(width).min(active_width);
    let bottom = y.saturating_add(height).min(active_height);
    if left >= right || top >= bottom { return; }
    let first_x = left / TILE_SIZE;
    let last_x = (right - 1) / TILE_SIZE;
    let first_y = top / TILE_SIZE;
    let last_y = (bottom - 1) / TILE_SIZE;
    unsafe {
        let damage = addr_of_mut!(DAMAGE).cast::<u64>();
        for tile_y in first_y..=last_y {
            for tile_x in first_x..=last_x {
                let index = tile_y * MAX_TILES_X + tile_x;
                let word = index / 64;
                let mask = 1u64 << (index % 64);
                write_volatile(damage.add(word), read_volatile(damage.add(word)) | mask);
            }
        }
    }
}

pub fn mark_full() {
    if !is_active() { return; }
    let words = unsafe { addr_of_mut!(DAMAGE).cast::<u64>() };
    unsafe {
        for index in 0..DAMAGE_WORDS { write_volatile(words.add(index), u64::MAX); }
    }
}

/// Copy only dirty tiles from RAM into the physical framebuffer.
pub fn present() {
    if !is_active() { return; }
    unsafe {
        let Some(front) = FRONT else { return; };
        let damage_ptr = addr_of_mut!(DAMAGE).cast::<u64>();
        let back = addr_of!(BACKBUFFER).cast::<u32>();
        let width = ACTIVE_WIDTH;
        let height = ACTIVE_HEIGHT;
        let pitch = BACK_PITCH;
        let tiles_x = width.div_ceil(TILE_SIZE);
        let tiles_y = height.div_ceil(TILE_SIZE);
        let mut frame_tiles = 0u64;
        let mut frame_pixels = 0u64;
        for tile_y in 0..tiles_y {
            for tile_x in 0..tiles_x {
                let index = tile_y * MAX_TILES_X + tile_x;
                let word = index / 64;
                let mask = 1u64 << (index % 64);
                let bits = read_volatile(damage_ptr.add(word));
                if bits & mask == 0 { continue; }
                write_volatile(damage_ptr.add(word), bits & !mask);
                let x0 = tile_x * TILE_SIZE;
                let y0 = tile_y * TILE_SIZE;
                let x1 = (x0 + TILE_SIZE).min(width);
                let y1 = (y0 + TILE_SIZE).min(height);
                for y in y0..y1 {
                    let source = back.add(y * (pitch / 4) + x0);
                    let destination = front.address.add(y * front.pitch as usize).cast::<u32>().add(x0);
                    for x in 0..(x1 - x0) {
                        // Backbuffer is ordinary cacheable RAM; only the physical
                        // framebuffer destination requires a volatile store.
                        let pixel = *source.add(x);
                        write_volatile(destination.add(x), pixel);
                    }
                }
                frame_tiles += 1;
                frame_pixels += ((x1 - x0) * (y1 - y0)) as u64;
            }
        }
        if frame_tiles != 0 {
            FRAME_COUNT.fetch_add(1, Ordering::Relaxed);
            PRESENTED_TILES.fetch_add(frame_tiles, Ordering::Relaxed);
            PRESENTED_PIXELS.fetch_add(frame_pixels, Ordering::Relaxed);
        }
    }
}

pub fn stats() -> GraphicsStats {
    let (width, height) = unsafe { (ACTIVE_WIDTH, ACTIVE_HEIGHT) };
    GraphicsStats {
        active: is_active(), width, height,
        frames: FRAME_COUNT.load(Ordering::Relaxed),
        tiles_presented: PRESENTED_TILES.load(Ordering::Relaxed),
        pixels_presented: PRESENTED_PIXELS.load(Ordering::Relaxed),
    }
}

pub fn self_test() -> Result<(), &'static str> {
    if !is_active() { return Err("RENDERER BACKBUFFER IS NOT ACTIVE"); }
    let (width, height, pitch, front) = unsafe { (ACTIVE_WIDTH, ACTIVE_HEIGHT, BACK_PITCH, FRONT) };
    if width == 0 || height == 0 || width > MAX_WIDTH || height > MAX_HEIGHT {
        return Err("RENDERER DIMENSIONS ARE OUT OF RANGE");
    }
    if pitch != width * 4 { return Err("BACKBUFFER PITCH DOES NOT MATCH 32-BIT WIDTH"); }
    if front.map(|value| value.address.is_null() || value.pitch < (width * 4) as u64).unwrap_or(true) {
        return Err("FRONTBUFFER METADATA IS INVALID");
    }
    let pointer = unsafe { addr_of!(BACKBUFFER) as usize };
    if pointer % 4 != 0 { return Err("BACKBUFFER IS NOT U32 ALIGNED"); }
    Ok(())
}
