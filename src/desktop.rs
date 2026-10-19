//! XavierOS Desktop Preview for v0.2.0-beta.2.
//!
//! A small framebuffer compositor with a macOS-inspired dock. This is an in-kernel
//! preview: non-terminal apps are information windows, while Terminal opens the
//! existing shell. Mouse input is polled from the legacy PS/2 auxiliary port;
//! keyboard navigation remains available when no mouse is detected.

use crate::{clock, config, interrupts, net, users, Framebuffer, KeyboardKey, MemoryStats};

const APP_COUNT: usize = 6;
const CURSOR_WIDTH: usize = 64;
const CURSOR_HEIGHT: usize = 64;
const CURSOR_SAVE_WIDTH: usize = CURSOR_WIDTH;
const CURSOR_SAVE_HEIGHT: usize = CURSOR_HEIGHT;
const CURSOR_HOTSPOT_X: i32 = 16;
const CURSOR_HOTSPOT_Y: i32 = 6;
const CURSOR_RGBA: &[u8] = include_bytes!("../assets/cursor.rgba");
const DOCK_LABEL_SAVE_WIDTH: usize = 64;
const DOCK_LABEL_SAVE_HEIGHT: usize = 21;
const APP_FILES: usize = 0;
const APP_TERMINAL: usize = 1;
const APP_NETWORK: usize = 2;
const APP_CLOCK: usize = 3;
const APP_SETTINGS: usize = 4;
const APP_ABOUT: usize = 5;
const BUILD_LABEL: &str = "XAVIEROS 0.2.0-BETA.2";
const WALLPAPER_WIDTH: usize = 735;
const WALLPAPER_HEIGHT: usize = 452;
// RGB24 pixels generated from the user-supplied wallpaper. Keeping the image as
// raw RGB avoids needing a JPEG decoder inside this no_std kernel.
const WALLPAPER_RGB: &[u8] = include_bytes!("../assets/wallpaper.rgb");
const APP_NAMES: [&str; APP_COUNT] = ["FILES", "TERMINAL", "NETWORK", "CLOCK", "SETTINGS", "ABOUT"];

struct DesktopState {
    selected: usize,
    open_window: Option<usize>,
    mouse_x: i32,
    mouse_y: i32,
    mouse_buttons: u8,
    mouse_ready: bool,
}

// Save only the pixels underneath the small pointer. Redrawing the full desktop
// for every PS/2 packet caused visible flicker and wasted a large amount of CPU.
struct CursorBackground {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    pixels: [u32; CURSOR_SAVE_WIDTH * CURSOR_SAVE_HEIGHT],
    valid: bool,
}

impl CursorBackground {
    const fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            pixels: [0; CURSOR_SAVE_WIDTH * CURSOR_SAVE_HEIGHT],
            valid: false,
        }
    }
}

struct DockLabelBackground {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    pixels: [u32; DOCK_LABEL_SAVE_WIDTH * DOCK_LABEL_SAVE_HEIGHT],
    valid: bool,
}

impl DockLabelBackground {
    const fn new() -> Self {
        Self {
            x: 0,
            y: 0,
            width: 0,
            height: 0,
            pixels: [0; DOCK_LABEL_SAVE_WIDTH * DOCK_LABEL_SAVE_HEIGHT],
            valid: false,
        }
    }
}

pub(super) fn run(framebuffer: &Framebuffer, memory_summary: Option<(MemoryStats, bool)>, mouse_ready: bool) -> ! {
    interrupts::enable_interrupts();
    let width = framebuffer.width as usize;
    let height = framebuffer.height as usize;
    let mut state = DesktopState {
        selected: APP_TERMINAL,
        open_window: None,
        mouse_x: (width / 2) as i32,
        mouse_y: (height.saturating_sub(150)) as i32,
        mouse_buttons: 0,
        mouse_ready,
    };
    let mut cursor_background = CursorBackground::new();
    let mut dock_label_background = DockLabelBackground::new();

    crate::serial_write("XavierOS Desktop Preview entered. Press Enter on Terminal or type T.\r\n");
    render_desktop(framebuffer, &state, memory_summary, &mut dock_label_background);
    if state.mouse_ready {
        draw_mouse_cursor_saved(framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
    }

    loop {
        let mut repaint = false;
        let mut dock_repaint = false;
        let mut cursor_moved = false;
        let mut launch_terminal = false;

        if let Some(packet) = interrupts::poll_mouse_packet() {
            let old_x = state.mouse_x;
            let old_y = state.mouse_y;
            state.mouse_x = (state.mouse_x + packet.dx as i32).clamp(1, width.saturating_sub(2) as i32);
            // PS/2 Y motion is positive upward, while framebuffer Y is positive downward.
            state.mouse_y = (state.mouse_y - packet.dy as i32).clamp(1, height.saturating_sub(2) as i32);
            cursor_moved = old_x != state.mouse_x || old_y != state.mouse_y;

            // Update only the Dock when the pointer moves over a different icon.
            // The wallpaper and open windows must not be redrawn for mouse motion.
            if let Some(index) = dock_hit_test(width, height, state.mouse_x, state.mouse_y) {
                if index != state.selected {
                    state.selected = index;
                    dock_repaint = true;
                }
            }
            let left_now = packet.buttons & 0x01 != 0;
            let left_before = state.mouse_buttons & 0x01 != 0;
            if left_now && !left_before {
                if let Some(index) = dock_hit_test(width, height, state.mouse_x, state.mouse_y) {
                    state.selected = index;
                    if index == APP_TERMINAL {
                        launch_terminal = true;
                    } else {
                        state.open_window = Some(index);
                        repaint = true;
                    }
                } else if state.open_window.is_some() && close_button_hit(framebuffer, state.mouse_x, state.mouse_y) {
                    state.open_window = None;
                    repaint = true;
                }
            }
            state.mouse_buttons = packet.buttons;
        }

        if let Some(key) = crate::keyboard_poll_key() {
            match key {
                KeyboardKey::CursorLeft | KeyboardKey::HistoryUp => {
                    state.selected = (state.selected + APP_COUNT - 1) % APP_COUNT;
                    dock_repaint = true;
                }
                KeyboardKey::CursorRight | KeyboardKey::HistoryDown | KeyboardKey::Tab => {
                    state.selected = (state.selected + 1) % APP_COUNT;
                    dock_repaint = true;
                }
                KeyboardKey::Enter => {
                    if state.selected == APP_TERMINAL {
                        launch_terminal = true;
                    } else {
                        state.open_window = Some(state.selected);
                        repaint = true;
                    }
                }
                KeyboardKey::Escape => {
                    if state.open_window.is_some() {
                        state.open_window = None;
                        repaint = true;
                    }
                }
                KeyboardKey::Character(byte) => match byte.to_ascii_uppercase() {
                    b'T' => launch_terminal = true,
                    b'F' => { state.selected = APP_FILES; state.open_window = Some(APP_FILES); repaint = true; }
                    b'N' => { state.selected = APP_NETWORK; state.open_window = Some(APP_NETWORK); repaint = true; }
                    b'C' => { state.selected = APP_CLOCK; state.open_window = Some(APP_CLOCK); repaint = true; }
                    b'S' => { state.selected = APP_SETTINGS; state.open_window = Some(APP_SETTINGS); repaint = true; }
                    b'A' => { state.selected = APP_ABOUT; state.open_window = Some(APP_ABOUT); repaint = true; }
                    _ => {}
                },
                _ => {}
            }
        }

        if launch_terminal {
            // The console owns the framebuffer while it runs, so the previous
            // cursor snapshot is discarded by the full desktop redraw below.
            crate::run_console(framebuffer, memory_summary);
            state.open_window = None;
            repaint = true;
        }

        if repaint {
            render_desktop(framebuffer, &state, memory_summary, &mut dock_label_background);
            if state.mouse_ready {
                draw_mouse_cursor_saved(framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
            } else {
                cursor_background.valid = false;
            }
        } else if dock_repaint || cursor_moved {
            if state.mouse_ready {
                restore_cursor_background(framebuffer, &mut cursor_background);
            }
            if dock_repaint {
                restore_dock_label_background(framebuffer, &mut dock_label_background);
                save_dock_label_background(framebuffer, state.selected, &mut dock_label_background);
                draw_dock(framebuffer, state.selected);
            }
            if state.mouse_ready {
                draw_mouse_cursor_saved(framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
            }
        }

        interrupts::wait_for_interrupt();
    }
}

fn render_desktop(
    fb: &Framebuffer,
    state: &DesktopState,
    memory_summary: Option<(MemoryStats, bool)>,
    dock_label_background: &mut DockLabelBackground,
) {
    draw_wallpaper(fb);
    if let Some(app) = state.open_window {
        draw_app_window(fb, app, memory_summary);
    }
    draw_menu_bar(fb, state.mouse_ready);
    save_dock_label_background(fb, state.selected, dock_label_background);
    draw_dock(fb, state.selected);
}

fn draw_wallpaper(fb: &Framebuffer) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    if width == 0 || height == 0 || WALLPAPER_RGB.len() < WALLPAPER_WIDTH * WALLPAPER_HEIGHT * 3 {
        return;
    }

    // "Cover" scaling: preserve the photo's aspect ratio and crop the centered
    // excess so every framebuffer pixel is covered (no stretching or black bars).
    let width_u = width as u64;
    let height_u = height as u64;
    let source_w = WALLPAPER_WIDTH as u64;
    let source_h = WALLPAPER_HEIGHT as u64;
    let target_is_wider = width_u * source_h >= height_u * source_w;
    let step_fp: u64;
    let start_x_fp: u64;
    let start_y_fp: u64;
    if target_is_wider {
        step_fp = (source_w << 16) / width_u;
        start_x_fp = 0;
        start_y_fp = ((width_u * source_h - height_u * source_w) << 15) / width_u;
    } else {
        step_fp = (source_h << 16) / height_u;
        start_x_fp = ((height_u * source_w - width_u * source_h) << 15) / height_u;
        start_y_fp = 0;
    }

    for y in 0..height {
        let source_y_fp = start_y_fp + y as u64 * step_fp;
        let source_y = ((source_y_fp >> 16) as usize).min(WALLPAPER_HEIGHT - 1);
        let row = unsafe { fb.address.add(y * fb.pitch as usize) as *mut u32 };
        for x in 0..width {
            let source_x_fp = start_x_fp + x as u64 * step_fp;
            let source_x = ((source_x_fp >> 16) as usize).min(WALLPAPER_WIDTH - 1);
            let source_index = (source_y * WALLPAPER_WIDTH + source_x) * 3;
            let pixel = color(
                fb,
                WALLPAPER_RGB[source_index],
                WALLPAPER_RGB[source_index + 1],
                WALLPAPER_RGB[source_index + 2],
            );
            unsafe { core::ptr::write_volatile(row.add(x), pixel); }
        }
    }
}


fn draw_menu_bar(fb: &Framebuffer, mouse_ready: bool) {
    let width = fb.width as usize;
    let scale = if width >= 800 { 2 } else { 1 };
    crate::fill_rect(fb, 0, 0, width, 42, color(fb, 15, 22, 43));
    crate::fill_rect(fb, 0, 40, width, 2, color(fb, 76, 194, 240));
    crate::draw_text(fb, 20, 13, "X", 2, color(fb, 82, 213, 255));
    crate::draw_text(fb, 43, 14, "XAVIEROS", 1, color(fb, 230, 239, 255));
    let menu_scale = if scale == 2 { 1 } else { 1 };
    crate::draw_text(fb, 138, 16, "SYSTEM   DESKTOP   VIEW   HELP", menu_scale, color(fb, 197, 212, 238));

    let mut time_buf = [0u8; 24];
    let time = format_clock(&mut time_buf);
    let time_width = time.len() * 6;
    let right_x = width.saturating_sub(time_width + if mouse_ready { 124 } else { 102 });
    crate::draw_text(fb, right_x, 16, time, 1, color(fb, 228, 237, 250));
    crate::draw_text(fb, width.saturating_sub(if mouse_ready { 96 } else { 80 }), 16, if mouse_ready { "MOUSE" } else { "KEYS" }, 1, color(fb, 107, 229, 181));
}

fn format_clock(buffer: &mut [u8; 24]) -> &str {
    let Ok(now) = clock::now_local() else { return "CLOCK N/A"; };
    let mut len = 0usize;
    push_num(buffer, &mut len, now.year as u64, 4);
    push_byte(buffer, &mut len, b'-');
    push_num(buffer, &mut len, now.month as u64, 2);
    push_byte(buffer, &mut len, b'-');
    push_num(buffer, &mut len, now.day as u64, 2);
    push_byte(buffer, &mut len, b' ');
    push_num(buffer, &mut len, now.hour as u64, 2);
    push_byte(buffer, &mut len, b':');
    push_num(buffer, &mut len, now.minute as u64, 2);
    core::str::from_utf8(&buffer[..len]).unwrap_or("CLOCK N/A")
}

fn dock_label_rect(fb: &Framebuffer, selected: usize) -> (usize, usize, usize, usize) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    let compact = width < 640;
    let slot_w = if compact { 44 } else { 68 };
    let padding = if compact { 8 } else { 16 };
    let dock_h = if compact { 62 } else { 78 };
    let dock_w = APP_COUNT * slot_w + padding * 2;
    let dock_x = width.saturating_sub(dock_w) / 2;
    let dock_y = height.saturating_sub(dock_h + 18);
    let center_x = dock_x + padding + selected * slot_w + slot_w / 2;
    let label_w = APP_NAMES[selected].len() * 6;
    let label_x = center_x.saturating_sub(label_w / 2);
    (label_x.saturating_sub(8), dock_y.saturating_sub(28), label_w + 16, 21)
}

fn save_dock_label_background(fb: &Framebuffer, selected: usize, background: &mut DockLabelBackground) {
    background.valid = false;
    if selected >= APP_COUNT { return; }

    let (x, y, width, height) = dock_label_rect(fb, selected);
    let fb_width = fb.width as usize;
    let fb_height = fb.height as usize;
    if x >= fb_width || y >= fb_height { return; }

    background.x = x;
    background.y = y;
    background.width = width.min(DOCK_LABEL_SAVE_WIDTH).min(fb_width - x);
    background.height = height.min(DOCK_LABEL_SAVE_HEIGHT).min(fb_height - y);
    for row_index in 0..background.height {
        let row = unsafe { fb.address.add((y + row_index) * fb.pitch as usize) as *const u32 };
        for col in 0..background.width {
            background.pixels[row_index * DOCK_LABEL_SAVE_WIDTH + col] = unsafe {
                core::ptr::read_volatile(row.add(x + col))
            };
        }
    }
    background.valid = true;
}

fn restore_dock_label_background(fb: &Framebuffer, background: &mut DockLabelBackground) {
    if !background.valid { return; }

    for row_index in 0..background.height {
        let row = unsafe {
            fb.address.add((background.y + row_index) * fb.pitch as usize) as *mut u32
        };
        for col in 0..background.width {
            unsafe {
                core::ptr::write_volatile(
                    row.add(background.x + col),
                    background.pixels[row_index * DOCK_LABEL_SAVE_WIDTH + col],
                );
            }
        }
    }
    background.valid = false;
}

fn draw_dock(fb: &Framebuffer, selected: usize) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    let compact = width < 640;
    let slot_w = if compact { 44 } else { 68 };
    let padding = if compact { 8 } else { 16 };
    let dock_h = if compact { 62 } else { 78 };
    let dock_w = APP_COUNT * slot_w + padding * 2;
    let dock_x = width.saturating_sub(dock_w) / 2;
    let dock_y = height.saturating_sub(dock_h + 18);

    // Layered dark glass, a subtle cool rim, and a fine top-edge reflection.
    rounded_rect(fb, dock_x + 3, dock_y + 6, dock_w, dock_h, 19, color(fb, 2, 5, 13));
    rounded_rect(fb, dock_x, dock_y, dock_w, dock_h, 18, color(fb, 12, 19, 34));
    rounded_rect(fb, dock_x + 2, dock_y + 2, dock_w.saturating_sub(4), dock_h.saturating_sub(4), 16, color(fb, 25, 35, 53));
    crate::fill_rect(fb, dock_x + 22, dock_y + 3, dock_w.saturating_sub(44), 1, color(fb, 101, 137, 169));
    crate::fill_rect(fb, dock_x + 18, dock_y + dock_h - 3, dock_w.saturating_sub(36), 1, color(fb, 40, 62, 85));

    for index in 0..APP_COUNT {
        let slot_x = dock_x + padding + index * slot_w;
        let icon_size = if !compact && index == selected { 48 } else if compact && index == selected { 32 } else if compact { 27 } else { 42 };
        let center_x = slot_x + slot_w / 2;
        let icon_x = center_x.saturating_sub(icon_size / 2);
        let icon_y = dock_y + (dock_h.saturating_sub(icon_size)) / 2 - 2;
        if index == selected {
            rounded_rect(fb, center_x.saturating_sub((slot_w - 6) / 2), dock_y + 7, slot_w - 6, dock_h - 14, 12, color(fb, 43, 62, 86));
            crate::fill_rect(fb, center_x.saturating_sub(10), dock_y + dock_h - 7, 20, 2, color(fb, 98, 221, 255));
        }
        draw_app_icon(fb, index, icon_x, icon_y, icon_size);
        if index == selected {
            let label = APP_NAMES[index];
            let label_w = label.len() * 6;
            let label_x = center_x.saturating_sub(label_w / 2);
            rounded_rect(fb, label_x.saturating_sub(8), dock_y.saturating_sub(28), label_w + 16, 21, 7, color(fb, 10, 17, 30));
            crate::fill_rect(fb, label_x.saturating_sub(3), dock_y.saturating_sub(28), label_w + 6, 1, color(fb, 63, 111, 143));
            crate::draw_text(fb, label_x, dock_y.saturating_sub(22), label, 1, color(fb, 240, 247, 255));
        }
        // Cyan status dot marks the Terminal as available in the dock.
        if index == APP_TERMINAL {
            fill_circle(fb, center_x as i32, (dock_y + dock_h - 4) as i32, 2, color(fb, 99, 227, 204));
        }
    }
}

fn draw_app_icon(fb: &Framebuffer, app: usize, x: usize, y: usize, size: usize) {
    let r = (size / 5).max(4);
    match app {
        APP_FILES => {
            // Blue folder with a raised tab and a bright front edge.
            rounded_rect(fb, x + 1, y + size / 4, size.saturating_sub(2), size * 2 / 3, r, color(fb, 41, 117, 180));
            rounded_rect(fb, x + 4, y + size / 6, size * 3 / 5, size / 3, r / 2, color(fb, 119, 213, 245));
            rounded_rect(fb, x + 3, y + size / 3, size.saturating_sub(6), size / 2, r / 2, color(fb, 54, 171, 224));
            crate::fill_rect(fb, x + size / 5, y + size / 2, size * 3 / 5, 2, color(fb, 194, 242, 255));
        }
        APP_TERMINAL => {
            rounded_rect(fb, x, y, size, size, r, color(fb, 7, 13, 25));
            rounded_rect(fb, x + 2, y + 2, size.saturating_sub(4), size.saturating_sub(4), r.saturating_sub(2), color(fb, 25, 41, 58));
            crate::fill_rect(fb, x + 4, y + 6, size.saturating_sub(8), 1, color(fb, 64, 88, 109));
            for (i, shade) in [color(fb, 255, 112, 120), color(fb, 255, 202, 102), color(fb, 113, 231, 170)].iter().enumerate() {
                fill_circle(fb, (x + 7 + i * 6) as i32, (y + 7) as i32, 1, *shade);
            }
            let text_scale = if size >= 44 { 2 } else { 1 };
            crate::draw_text(fb, x + size / 4, y + size / 2 - 3, ">", text_scale, color(fb, 111, 238, 202));
            crate::fill_rect(fb, x + size / 2, y + size * 2 / 3, size / 4, 2, color(fb, 111, 238, 202));
        }
        APP_NETWORK => {
            let cx = (x + size / 2) as i32;
            let cy = (y + size / 2) as i32;
            fill_circle(fb, cx, cy, (size / 2) as i32, color(fb, 17, 109, 124));
            fill_circle(fb, cx, cy, (size / 2).saturating_sub(3) as i32, color(fb, 21, 62, 83));
            let points = [
                (cx, y as i32 + size as i32 / 4),
                (x as i32 + size as i32 / 4, y as i32 + size as i32 * 3 / 4),
                (x as i32 + size as i32 * 3 / 4, y as i32 + size as i32 * 3 / 4),
            ];
            draw_thick_line(fb, points[0].0, points[0].1, points[1].0, points[1].1, 2, color(fb, 109, 246, 222));
            draw_thick_line(fb, points[0].0, points[0].1, points[2].0, points[2].1, 2, color(fb, 109, 246, 222));
            draw_thick_line(fb, points[1].0, points[1].1, points[2].0, points[2].1, 2, color(fb, 109, 246, 222));
            for (px, py) in points { fill_circle(fb, px, py, (size / 10).max(2) as i32, color(fb, 171, 255, 239)); }
        }
        APP_CLOCK => {
            let cx = (x + size / 2) as i32;
            let cy = (y + size / 2) as i32;
            fill_circle(fb, cx, cy, (size / 2) as i32, color(fb, 8, 23, 48));
            fill_circle(fb, cx, cy, (size / 2).saturating_sub(3) as i32, color(fb, 170, 219, 246));
            fill_circle(fb, cx, cy, (size / 2).saturating_sub(6) as i32, color(fb, 234, 246, 255));
            for (dx, dy) in [(0, -1), (1, 0), (0, 1), (-1, 0)] {
                crate::fill_rect(fb, (cx + dx * (size as i32 / 3)).max(0) as usize, (cy + dy * (size as i32 / 3)).max(0) as usize, 2, 2, color(fb, 49, 93, 127));
            }
            draw_thick_line(fb, cx, cy, cx, cy - (size as i32 / 4), 3, color(fb, 27, 68, 105));
            draw_thick_line(fb, cx, cy, cx + (size as i32 / 5), cy + (size as i32 / 7), 3, color(fb, 24, 143, 177));
            fill_circle(fb, cx, cy, (size / 12).max(2) as i32, color(fb, 37, 173, 203));
        }
        APP_SETTINGS => {
            let cx = (x + size / 2) as i32;
            let cy = (y + size / 2) as i32;
            rounded_rect(fb, x, y, size, size, r, color(fb, 48, 65, 91));
            // Eight teeth around the hub produce a clear gear silhouette.
            for (dx, dy) in [(0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1)] {
                let tooth_size = (size / 6).max(3);
                let tx = (cx + dx * (size as i32 / 3) - tooth_size as i32 / 2).max(0) as usize;
                let ty = (cy + dy * (size as i32 / 3) - tooth_size as i32 / 2).max(0) as usize;
                rounded_rect(fb, tx, ty, tooth_size, tooth_size, 2, color(fb, 185, 208, 230));
            }
            fill_circle(fb, cx, cy, (size * 3 / 10) as i32, color(fb, 207, 226, 243));
            fill_circle(fb, cx, cy, (size / 8).max(2) as i32, color(fb, 46, 66, 95));
        }
        _ => {
            rounded_rect(fb, x, y, size, size, r, color(fb, 12, 52, 89));
            rounded_rect(fb, x + 2, y + 2, size.saturating_sub(4), size.saturating_sub(4), r.saturating_sub(1), color(fb, 27, 125, 167));
            let span = (size * 23 / 100) as i32;
            let cx = x as i32 + size as i32 / 2;
            let cy = y as i32 + size as i32 / 2;
            draw_thick_line(fb, cx - span, cy - span, cx + span, cy + span, (size / 12).max(3) as i32, color(fb, 206, 250, 255));
            draw_thick_line(fb, cx - span, cy + span, cx + span, cy - span, (size / 12).max(3) as i32, color(fb, 111, 227, 255));
        }
    }
}

fn draw_app_window(fb: &Framebuffer, app: usize, memory_summary: Option<(MemoryStats, bool)>) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    let win_w = width.saturating_sub(54).min(700).max(250);
    let win_h = height.saturating_sub(230).min(420).max(180);
    let x = width.saturating_sub(win_w) / 2;
    let y = 74 + height.saturating_sub(74 + win_h + 104) / 2;
    rounded_rect(fb, x + 5, y + 8, win_w, win_h, 13, color(fb, 5, 8, 19));
    rounded_rect(fb, x, y, win_w, win_h, 13, color(fb, 23, 31, 52));
    rounded_rect(fb, x + 1, y + 1, win_w.saturating_sub(2), win_h.saturating_sub(2), 12, color(fb, 16, 24, 43));
    rounded_rect(fb, x, y, win_w, 38, 12, color(fb, 36, 46, 68));
    crate::fill_rect(fb, x, y + 26, win_w, 12, color(fb, 36, 46, 68));
    fill_circle(fb, (x + 18) as i32, (y + 19) as i32, 5, color(fb, 255, 97, 111));
    fill_circle(fb, (x + 35) as i32, (y + 19) as i32, 5, color(fb, 255, 192, 79));
    fill_circle(fb, (x + 52) as i32, (y + 19) as i32, 5, color(fb, 91, 224, 147));
    crate::draw_text(fb, x + 72, y + 15, APP_NAMES[app], 1, color(fb, 225, 234, 249));

    let left = x + 24;
    let mut row_y = y + 58;
    let gap = 23;
    match app {
        APP_FILES => {
            crate::draw_text(fb, left, row_y, "LOCATIONS", 1, color(fb, 104, 220, 255)); row_y += gap;
            crate::draw_text(fb, left, row_y, "/", 1, color(fb, 226, 237, 252)); row_y += gap;
            let mut count = 0usize;
            let _ = crate::vfs::list_directory("/", |name, is_dir| {
                if count < 7 {
                    let prefix = if is_dir { "[DIR]" } else { "[FILE]" };
                    crate::draw_text(fb, left + 12, row_y + count * gap, prefix, 1, color(fb, 105, 224, 181));
                    crate::draw_text(fb, left + 58, row_y + count * gap, name, 1, color(fb, 220, 232, 250));
                }
                count += 1;
            });
            let info_y = y + win_h.saturating_sub(38);
            crate::draw_text(fb, left, info_y, "OPEN TERMINAL FOR CAT / WRITE / MKDIR", 1, color(fb, 155, 176, 207));
        }
        APP_NETWORK => {
            crate::draw_text(fb, left, row_y, "NETWORK STATUS", 1, color(fb, 104, 220, 255)); row_y += gap + 3;
            crate::draw_text(fb, left, row_y, net::status_text(), 2, color(fb, if net::is_ready() { 105 } else { 255 }, if net::is_ready() { 231 } else { 139 }, if net::is_ready() { 183 } else { 139 })); row_y += gap + 9;
            if let Some(info) = net::info() {
                let mac = format_mac(&info.mac);
                let local_ip = format_ipv4(info.local_ip);
                let gateway = format_ipv4(info.gateway);
                let dns = format_ipv4(info.dns);
                draw_label_value(fb, left, row_y, "MAC", bytes_text(&mac), color(fb, 223, 234, 251)); row_y += gap;
                draw_label_value(fb, left, row_y, "IPV4", bytes_text(&local_ip), color(fb, 223, 234, 251)); row_y += gap;
                draw_label_value(fb, left, row_y, "GATEWAY", bytes_text(&gateway), color(fb, 223, 234, 251)); row_y += gap;
                draw_label_value(fb, left, row_y, "DNS", bytes_text(&dns), color(fb, 223, 234, 251)); row_y += gap;
                crate::draw_text(fb, left, row_y, if info.link_up { "LINK UP" } else { "LINK DOWN" }, 1, color(fb, 105, 224, 181));
            } else {
                crate::draw_text(fb, left, row_y, "NO E1000 NETWORK DEVICE WAS DETECTED", 1, color(fb, 255, 166, 151));
            }
            crate::draw_text(fb, left, y + win_h.saturating_sub(38), "COMMANDS: NETSTAT  NETCHECK  PING", 1, color(fb, 155, 176, 207));
        }
        APP_CLOCK => {
            crate::draw_text(fb, left, row_y, "WALL CLOCK", 1, color(fb, 104, 220, 255)); row_y += gap + 14;
            if let Ok(now) = clock::now_local() {
                let mut buffer = [0u8; 24];
                let text = format_datetime(&mut buffer, now);
                crate::draw_text(fb, left, row_y, text, if width >= 640 { 3 } else { 2 }, color(fb, 245, 249, 255)); row_y += 42;
            } else {
                crate::draw_text(fb, left, row_y, "CLOCK UNAVAILABLE", 2, color(fb, 255, 177, 156)); row_y += gap + 12;
            }
            let offset = clock::timezone_offset_minutes();
            let timezone = format_timezone(offset);
            crate::draw_text(fb, left, row_y, bytes_text(&timezone), 1, color(fb, 180, 202, 232)); row_y += gap;
            crate::draw_text(fb, left, row_y, if clock::is_ntp_synchronized() { "SOURCE: NTP SYNCED" } else { "SOURCE: CMOS RTC" }, 1, color(fb, 105, 224, 181));
        }
        APP_SETTINGS => {
            let settings = config::current();
            crate::draw_text(fb, left, row_y, "SYSTEM PREFERENCES", 1, color(fb, 104, 220, 255)); row_y += gap + 4;
            draw_label_value(fb, left, row_y, "NETWORK", if settings.use_dhcp { "DHCP" } else { "STATIC" }, color(fb, 226, 237, 252)); row_y += gap;
            let timezone = format_timezone(settings.timezone_minutes);
            let timeout = format_seconds(settings.timeout_seconds);
            draw_label_value(fb, left, row_y, "TIMEZONE", bytes_text(&timezone), color(fb, 226, 237, 252)); row_y += gap;
            draw_label_value(fb, left, row_y, "NTP SERVER", settings.ntp_server_str(), color(fb, 226, 237, 252)); row_y += gap;
            draw_label_value(fb, left, row_y, "TIMEOUT", bytes_text(&timeout), color(fb, 226, 237, 252)); row_y += gap + 8;
            crate::draw_text(fb, left, row_y, "DISPLAY", 1, color(fb, 104, 220, 255)); row_y += gap;
            let mut resolution_buffer = [0u8; 24];
            let resolution = format_resolution(&mut resolution_buffer, fb.width, fb.height);
            draw_label_value(fb, left, row_y, "CURRENT MODE", resolution, color(fb, 226, 237, 252)); row_y += gap;
            crate::draw_text(fb, left, row_y, "AVAILABLE: 1024x768 / 1280x720 / 1366x768", 1, color(fb, 226, 237, 252)); row_y += gap;
            crate::draw_text(fb, left, row_y, "CHOOSE RESOLUTION IN LIMINE STARTUP MENU", 1, color(fb, 155, 176, 207)); row_y += gap;
            crate::draw_text(fb, left, row_y, "LIVE SWITCHING IS NOT SUPPORTED YET", 1, color(fb, 155, 176, 207));
        }
        _ => {
            crate::draw_text(fb, left, row_y, "XAVIEROS DESKTOP PREVIEW", 2, color(fb, 112, 221, 255)); row_y += gap + 17;
            crate::draw_text(fb, left, row_y, BUILD_LABEL, 1, color(fb, 224, 235, 252)); row_y += gap;
            crate::draw_text(fb, left, row_y, "RUST NO_STD KERNEL  /  LIMINE  /  X86_64", 1, color(fb, 180, 200, 231)); row_y += gap;
            crate::draw_text(fb, left, row_y, "THE DESKTOP IS AN EARLY IN-KERNEL UI PROTOTYPE.", 1, color(fb, 180, 200, 231)); row_y += gap;
            crate::draw_text(fb, left, row_y, "FILES, NETWORK, CLOCK AND SETTINGS ARE PREVIEW WINDOWS.", 1, color(fb, 180, 200, 231));
        }
    }
    let hint = "PRESS ESC TO CLOSE  /  CLICK DOCK TO SWITCH APPS";
    crate::draw_text(fb, left, y + win_h.saturating_sub(18), hint, 1, color(fb, 123, 150, 190));
    let _ = memory_summary;
}

fn draw_label_value(fb: &Framebuffer, x: usize, y: usize, label: &str, value: &str, color_value: u32) {
    crate::draw_text(fb, x, y, label, 1, color(fb, 115, 139, 178));
    let value_x = x + label.len() * 6 + 18;
    crate::draw_text(fb, value_x, y, value, 1, color_value);
}

fn dock_hit_test(width: usize, height: usize, x: i32, y: i32) -> Option<usize> {
    if x < 0 || y < 0 { return None; }
    let compact = width < 640;
    let slot_w = if compact { 44 } else { 68 };
    let padding = if compact { 8 } else { 16 };
    let dock_h = if compact { 62 } else { 78 };
    let dock_w = APP_COUNT * slot_w + padding * 2;
    let dock_x = width.saturating_sub(dock_w) / 2;
    let dock_y = height.saturating_sub(dock_h + 18);
    let x = x as usize;
    let y = y as usize;
    if x < dock_x + padding || x >= dock_x + dock_w - padding || y < dock_y || y >= dock_y + dock_h {
        return None;
    }
    let index = (x - (dock_x + padding)) / slot_w;
    if index < APP_COUNT { Some(index) } else { None }
}

fn close_button_hit(fb: &Framebuffer, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 { return false; }
    let width = fb.width as usize;
    let height = fb.height as usize;
    let win_w = width.saturating_sub(54).min(700).max(250);
    let win_h = height.saturating_sub(230).min(420).max(180);
    let left = width.saturating_sub(win_w) / 2;
    let top = 74 + height.saturating_sub(74 + win_h + 104) / 2;
    let x = x as usize;
    let y = y as usize;
    x >= left + 7 && x <= left + 29 && y >= top + 7 && y <= top + 31
}

fn draw_mouse_cursor_saved(fb: &Framebuffer, x: i32, y: i32, background: &mut CursorBackground) {
    save_cursor_background(fb, x, y, background);
    draw_mouse_cursor(fb, x, y);
}

fn save_cursor_background(fb: &Framebuffer, x: i32, y: i32, background: &mut CursorBackground) {
    background.valid = false;
    let origin_x = x - CURSOR_HOTSPOT_X;
    let origin_y = y - CURSOR_HOTSPOT_Y;
    let fb_width = fb.width as i32;
    let fb_height = fb.height as i32;
    let left = origin_x.max(0) as usize;
    let top = origin_y.max(0) as usize;
    let right = (origin_x + CURSOR_WIDTH as i32).min(fb_width).max(0) as usize;
    let bottom = (origin_y + CURSOR_HEIGHT as i32).min(fb_height).max(0) as usize;
    if left >= right || top >= bottom { return; }

    background.x = left;
    background.y = top;
    background.width = right - left;
    background.height = bottom - top;
    for row_index in 0..background.height {
        let row = unsafe { fb.address.add((top + row_index) * fb.pitch as usize) as *const u32 };
        for col in 0..background.width {
            background.pixels[row_index * CURSOR_SAVE_WIDTH + col] = unsafe {
                core::ptr::read_volatile(row.add(left + col))
            };
        }
    }
    background.valid = true;
}

fn restore_cursor_background(fb: &Framebuffer, background: &mut CursorBackground) {
    if !background.valid { return; }

    for row_index in 0..background.height {
        let row = unsafe {
            fb.address.add((background.y + row_index) * fb.pitch as usize) as *mut u32
        };
        for col in 0..background.width {
            unsafe {
                core::ptr::write_volatile(
                    row.add(background.x + col),
                    background.pixels[row_index * CURSOR_SAVE_WIDTH + col],
                );
            }
        }
    }
    background.valid = false;
}

fn draw_mouse_cursor(fb: &Framebuffer, x: i32, y: i32) {
    if CURSOR_RGBA.len() < CURSOR_WIDTH * CURSOR_HEIGHT * 4 { return; }
    let origin_x = x - CURSOR_HOTSPOT_X;
    let origin_y = y - CURSOR_HOTSPOT_Y;
    let fb_width = fb.width as i32;
    let fb_height = fb.height as i32;

    // Paint a one-pixel near-white halo around the submitted black pointer so it
    // stays visible over both the dark wallpaper and bright UI panels.
    for cy in 0..CURSOR_HEIGHT {
        let py = origin_y + cy as i32;
        if py < 0 || py >= fb_height { continue; }
        let row = unsafe { fb.address.add(py as usize * fb.pitch as usize) as *mut u32 };
        for cx in 0..CURSOR_WIDTH {
            let px = origin_x + cx as i32;
            if px < 0 || px >= fb_width { continue; }
            let index = (cy * CURSOR_WIDTH + cx) * 4;
            let alpha = CURSOR_RGBA[index + 3];
            if alpha == 0 {
                if cursor_has_neighbor(cx, cy) {
                    blend_cursor_pixel(fb, row, px as usize, 242, 247, 255, 225);
                }
            } else {
                blend_cursor_pixel(
                    fb,
                    row,
                    px as usize,
                    CURSOR_RGBA[index],
                    CURSOR_RGBA[index + 1],
                    CURSOR_RGBA[index + 2],
                    alpha,
                );
            }
        }
    }
}

fn cursor_has_neighbor(x: usize, y: usize) -> bool {
    let top = y.saturating_sub(1);
    let bottom = (y + 1).min(CURSOR_HEIGHT - 1);
    let left = x.saturating_sub(1);
    let right = (x + 1).min(CURSOR_WIDTH - 1);
    for ny in top..=bottom {
        for nx in left..=right {
            if nx == x && ny == y { continue; }
            if CURSOR_RGBA[(ny * CURSOR_WIDTH + nx) * 4 + 3] >= 64 {
                return true;
            }
        }
    }
    false
}

fn blend_cursor_pixel(
    fb: &Framebuffer,
    row: *mut u32,
    x: usize,
    red: u8,
    green: u8,
    blue: u8,
    alpha: u8,
) {
    if alpha == 0 { return; }
    let destination = unsafe { core::ptr::read_volatile(row.add(x)) };
    let dest_red = unpack_channel(destination, fb.red_mask_size, fb.red_mask_shift);
    let dest_green = unpack_channel(destination, fb.green_mask_size, fb.green_mask_shift);
    let dest_blue = unpack_channel(destination, fb.blue_mask_size, fb.blue_mask_shift);
    let inverse = 255 - alpha as u16;
    let blend = |source: u8, dest: u8| {
        ((source as u16 * alpha as u16 + dest as u16 * inverse + 127) / 255) as u8
    };
    let output = crate::rgb(
        fb,
        blend(red, dest_red),
        blend(green, dest_green),
        blend(blue, dest_blue),
    );
    unsafe { core::ptr::write_volatile(row.add(x), output); }
}

fn unpack_channel(pixel: u32, size: u8, shift: u8) -> u8 {
    if size == 0 || size > 8 || shift >= 32 { return 0; }
    let max = (1u32 << size) - 1;
    let value = (pixel >> shift) & max;
    ((value * 255 + max / 2) / max) as u8
}

fn rounded_rect(fb: &Framebuffer, x: usize, y: usize, w: usize, h: usize, radius: usize, shade: u32) {
    if w == 0 || h == 0 { return; }
    let r = radius.min(w / 2).min(h / 2);
    if r == 0 { crate::fill_rect(fb, x, y, w, h, shade); return; }
    crate::fill_rect(fb, x + r, y, w.saturating_sub(2 * r), h, shade);
    crate::fill_rect(fb, x, y + r, w, h.saturating_sub(2 * r), shade);
    fill_circle(fb, (x + r) as i32, (y + r) as i32, r as i32, shade);
    fill_circle(fb, (x + w - r - 1) as i32, (y + r) as i32, r as i32, shade);
    fill_circle(fb, (x + r) as i32, (y + h - r - 1) as i32, r as i32, shade);
    fill_circle(fb, (x + w - r - 1) as i32, (y + h - r - 1) as i32, r as i32, shade);
}

fn fill_circle(fb: &Framebuffer, cx: i32, cy: i32, radius: i32, shade: u32) {
    if radius <= 0 { return; }
    let squared = radius * radius;
    for dy in -radius..=radius {
        let remaining = squared - dy * dy;
        if remaining < 0 { continue; }
        let dx = integer_sqrt(remaining as u32) as i32;
        let y = cy + dy;
        if y < 0 { continue; }
        crate::fill_rect(fb, (cx - dx).max(0) as usize, y as usize, (dx * 2 + 1) as usize, 1, shade);
    }
}

fn draw_thick_line(fb: &Framebuffer, mut x0: i32, mut y0: i32, x1: i32, y1: i32, thickness: i32, shade: u32) {
    let dx = (x1 - x0).abs();
    let sx = if x0 < x1 { 1 } else { -1 };
    let dy = -(y1 - y0).abs();
    let sy = if y0 < y1 { 1 } else { -1 };
    let mut error = dx + dy;
    loop {
        let half = thickness.max(1) / 2;
        if x0 >= 0 && y0 >= 0 {
            crate::fill_rect(fb, (x0 - half).max(0) as usize, (y0 - half).max(0) as usize,
                thickness.max(1) as usize, thickness.max(1) as usize, shade);
        }
        if x0 == x1 && y0 == y1 { break; }
        let twice = 2 * error;
        if twice >= dy { error += dy; x0 += sx; }
        if twice <= dx { error += dx; y0 += sy; }
    }
}

fn integer_sqrt(value: u32) -> u32 {
    let mut op = value;
    let mut result = 0u32;
    let mut one = 1u32 << 30;
    while one > op { one >>= 2; }
    while one != 0 {
        if op >= result + one {
            op -= result + one;
            result = (result >> 1) + one;
        } else {
            result >>= 1;
        }
        one >>= 2;
    }
    result
}

fn format_ipv4(address: [u8; 4]) -> [u8; 15] {
    let mut buffer = [b' '; 15];
    let mut used = 0usize;
    for (index, part) in address.iter().enumerate() {
        if index > 0 { push_byte(&mut buffer, &mut used, b'.'); }
        push_num(&mut buffer, &mut used, *part as u64, 1);
    }
    buffer
}

fn format_mac(mac: &[u8; 6]) -> [u8; 17] {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    let mut buffer = [b'0'; 17];
    let mut used = 0usize;
    for (index, value) in mac.iter().enumerate() {
        if index != 0 { push_byte(&mut buffer, &mut used, b':'); }
        push_byte(&mut buffer, &mut used, HEX[(value >> 4) as usize]);
        push_byte(&mut buffer, &mut used, HEX[(value & 0x0F) as usize]);
    }
    buffer
}

fn format_resolution(buffer: &mut [u8; 24], width: u64, height: u64) -> &str {
    let mut used = 0usize;
    push_num(buffer, &mut used, width, 1);
    push_byte(buffer, &mut used, b'x');
    push_num(buffer, &mut used, height, 1);
    core::str::from_utf8(&buffer[..used]).unwrap_or("UNKNOWN")
}

fn format_datetime(buffer: &mut [u8; 24], now: clock::DateTime) -> &str {
    let mut used = 0usize;
    push_num(buffer, &mut used, now.year as u64, 4);
    push_byte(buffer, &mut used, b'-');
    push_num(buffer, &mut used, now.month as u64, 2);
    push_byte(buffer, &mut used, b'-');
    push_num(buffer, &mut used, now.day as u64, 2);
    push_byte(buffer, &mut used, b' ');
    push_num(buffer, &mut used, now.hour as u64, 2);
    push_byte(buffer, &mut used, b':');
    push_num(buffer, &mut used, now.minute as u64, 2);
    core::str::from_utf8(&buffer[..used]).unwrap_or("DATE N/A")
}

fn format_timezone(minutes: i32) -> [u8; 6] {
    let mut result = [b' '; 6];
    result[0] = if minutes < 0 { b'-' } else { b'+' };
    let magnitude = minutes.unsigned_abs();
    result[1] = b'0' + ((magnitude / 60) / 10) as u8;
    result[2] = b'0' + ((magnitude / 60) % 10) as u8;
    result[3] = b':';
    result[4] = b'0' + ((magnitude % 60) / 10) as u8;
    result[5] = b'0' + ((magnitude % 60) % 10) as u8;
    result
}

fn format_seconds(seconds: u16) -> [u8; 11] {
    let mut result = [b' '; 11];
    let mut used = 0;
    push_num(&mut result, &mut used, seconds as u64, 1);
    for b in b" SECONDS" { push_byte(&mut result, &mut used, *b); }
    result
}

fn bytes_text(bytes: &[u8]) -> &str {
    let end = bytes.iter().rposition(|byte| *byte != b' ').map_or(0, |index| index + 1);
    core::str::from_utf8(&bytes[..end]).unwrap_or("")
}

fn push_num(buffer: &mut [u8], used: &mut usize, mut value: u64, min_digits: usize) {
    let mut temp = [0u8; 20];
    let mut count = 0usize;
    loop {
        temp[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 { break; }
    }
    while count < min_digits && count < temp.len() { temp[count] = b'0'; count += 1; }
    for byte in temp[..count].iter().rev() { push_byte(buffer, used, *byte); }
}

fn push_byte(buffer: &mut [u8], used: &mut usize, byte: u8) {
    if *used < buffer.len() { buffer[*used] = byte; *used += 1; }
}

fn lerp(start: u8, end: u8, amount: u8) -> u8 {
    let a = start as i32;
    let delta = end as i32 - a;
    (a + delta * amount as i32 / 255).clamp(0, 255) as u8
}

fn color(fb: &Framebuffer, red: u8, green: u8, blue: u8) -> u32 {
    crate::rgb(fb, red, green, blue)
}
