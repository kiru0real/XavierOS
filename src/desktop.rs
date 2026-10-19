//! XavierOS Desktop Preview for v0.2.0-beta.1.
//!
//! A small framebuffer compositor with a macOS-inspired dock. This is an in-kernel
//! preview: non-terminal apps are information windows, while Terminal opens the
//! existing shell. Mouse input is polled from the legacy PS/2 auxiliary port;
//! keyboard navigation remains available when no mouse is detected.

use crate::{clock, config, interrupts, net, users, Framebuffer, KeyboardKey, MemoryStats};

const APP_COUNT: usize = 6;
const CURSOR_SAVE_WIDTH: usize = 16;
const CURSOR_SAVE_HEIGHT: usize = 22;
const DOCK_LABEL_SAVE_WIDTH: usize = 64;
const DOCK_LABEL_SAVE_HEIGHT: usize = 21;
const APP_FILES: usize = 0;
const APP_TERMINAL: usize = 1;
const APP_NETWORK: usize = 2;
const APP_CLOCK: usize = 3;
const APP_SETTINGS: usize = 4;
const APP_ABOUT: usize = 5;
const BUILD_LABEL: &str = "XAVIEROS 0.2.0-BETA.1";
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
    draw_welcome(fb);
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
    let safe_height = height.max(1);
    // Deep midnight gradient.
    for y in 0..height {
        let t = (y * 255 / safe_height) as u8;
        let red = lerp(10, 24, t);
        let green = lerp(16, 22, t);
        let blue = lerp(39, 65, t);
        crate::fill_rect(fb, 0, y, width, 1, color(fb, red, green, blue));
    }

    let short = width.min(height);
    let radius = (short * 38 / 100).max(36);
    let cx = (width * 73 / 100) as i32;
    let cy = (height * 53 / 100) as i32;
    fill_circle(fb, cx, cy, radius as i32, color(fb, 47, 37, 112));
    fill_circle(fb, cx + (radius / 4) as i32, cy - (radius / 7) as i32, (radius * 3 / 4) as i32, color(fb, 31, 35, 88));
    fill_circle(fb, (width * 36 / 100) as i32, (height * 67 / 100) as i32, (radius * 2 / 3) as i32, color(fb, 22, 104, 139));
    fill_circle(fb, (width * 36 / 100 + short / 22) as i32, (height * 67 / 100 - short / 36) as i32, (radius / 2) as i32, color(fb, 28, 49, 110));

    // Muted geometric X ribbons, deliberately behind foreground content.
    let x_center = (width * 70 / 100) as i32;
    let y_center = (height * 48 / 100) as i32;
    let span = (short * 24 / 100) as i32;
    draw_thick_line(fb, x_center - span, y_center - span, x_center + span, y_center + span, 5, color(fb, 66, 75, 147));
    draw_thick_line(fb, x_center - span, y_center + span, x_center + span, y_center - span, 5, color(fb, 42, 115, 159));

    let stars: [(usize, usize, usize); 18] = [
        (7, 13, 2), (17, 31, 1), (24, 8, 1), (38, 24, 2), (49, 10, 1), (61, 19, 1),
        (82, 11, 2), (91, 27, 1), (13, 51, 1), (29, 43, 2), (43, 58, 1), (55, 39, 1),
        (69, 61, 2), (87, 52, 1), (95, 44, 1), (8, 73, 1), (47, 77, 1), (78, 82, 1),
    ];
    for (sx, sy, size) in stars {
        let x = width * sx / 100;
        let y = height * sy / 100;
        crate::fill_rect(fb, x, y, size, size, color(fb, 110, 164, 220));
    }
}

fn draw_welcome(fb: &Framebuffer) {
    let width = fb.width as usize;
    let scale = if width >= 900 { 3 } else if width >= 600 { 2 } else { 1 };
    let heading = "HELLO, XAVIER.";
    crate::draw_text(fb, 54, 134, heading, scale, color(fb, 237, 243, 255));
    crate::draw_text(fb, 56, 134 + 10 * scale + 16, "YOUR SYSTEM. YOUR SPACE.", if scale > 1 { scale - 1 } else { 1 }, color(fb, 173, 195, 226));

    let card_y = 134 + 10 * scale + 48;
    let card_w = if width >= 900 { 204 } else if width >= 600 { 154 } else { width.saturating_sub(132) / 2 };
    let card_h = if width >= 900 { 74 } else { 60 };
    draw_status_card(fb, 54, card_y, card_w, card_h, "SYSTEM", "DESKTOP PREVIEW", color(fb, 67, 197, 255));
    let mem_text = "MEMORY READY";
    draw_status_card(fb, 54 + card_w + 12, card_y, card_w, card_h, "KERNEL", mem_text, color(fb, 117, 224, 185));
}

fn draw_status_card(fb: &Framebuffer, x: usize, y: usize, w: usize, h: usize, title: &str, value: &str, accent: u32) {
    rounded_rect(fb, x + 3, y + 4, w, h, 10, color(fb, 8, 12, 30));
    rounded_rect(fb, x, y, w, h, 10, color(fb, 20, 29, 58));
    rounded_rect(fb, x + 10, y + 11, 3, h.saturating_sub(22), 1, accent);
    crate::draw_text(fb, x + 22, y + 11, title, 1, color(fb, 144, 171, 214));
    crate::draw_text(fb, x + 22, y + 31, value, if w >= 190 { 2 } else { 1 }, color(fb, 234, 242, 255));
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

    // Glassy dock surface: opaque midnight glass with a quiet top highlight.
    rounded_rect(fb, dock_x + 2, dock_y + 5, dock_w, dock_h, 18, color(fb, 5, 8, 22));
    rounded_rect(fb, dock_x, dock_y, dock_w, dock_h, 18, color(fb, 25, 32, 55));
    crate::fill_rect(fb, dock_x + 20, dock_y + 2, dock_w.saturating_sub(40), 1, color(fb, 78, 91, 130));

    for index in 0..APP_COUNT {
        let slot_x = dock_x + padding + index * slot_w;
        let icon_size = if !compact && index == selected { 52 } else if compact && index == selected { 33 } else if compact { 28 } else { 44 };
        let center_x = slot_x + slot_w / 2;
        let icon_x = center_x.saturating_sub(icon_size / 2);
        let icon_y = dock_y + (dock_h.saturating_sub(icon_size)) / 2 - 2;
        if index == selected {
            rounded_rect(fb, center_x.saturating_sub((slot_w - 5) / 2), dock_y + 7, slot_w - 5, dock_h - 14, 12, color(fb, 45, 57, 87));
        }
        draw_app_icon(fb, index, icon_x, icon_y, icon_size);
        if index == selected {
            let label = APP_NAMES[index];
            let label_w = label.len() * 6;
            let label_x = center_x.saturating_sub(label_w / 2);
            rounded_rect(fb, label_x.saturating_sub(8), dock_y.saturating_sub(28), label_w + 16, 21, 7, color(fb, 11, 15, 30));
            crate::draw_text(fb, label_x, dock_y.saturating_sub(22), label, 1, color(fb, 238, 245, 255));
        }
        // Small cyan running dot under the terminal icon, even while the shell is hidden.
        if index == APP_TERMINAL {
            fill_circle(fb, center_x as i32, (dock_y + dock_h - 4) as i32, 2, color(fb, 93, 220, 255));
        }
    }
}

fn draw_app_icon(fb: &Framebuffer, app: usize, x: usize, y: usize, size: usize) {
    let r = (size / 5).max(4);
    match app {
        APP_FILES => {
            rounded_rect(fb, x + 2, y + size / 4, size.saturating_sub(4), size * 2 / 3, r, color(fb, 54, 137, 232));
            rounded_rect(fb, x + 3, y + size / 5, size / 2, size / 3, r / 2, color(fb, 102, 191, 255));
            rounded_rect(fb, x + 3, y + size / 3, size.saturating_sub(6), size / 2, r / 2, color(fb, 66, 165, 250));
            crate::fill_rect(fb, x + size / 5, y + size / 2, size * 3 / 5, 2, color(fb, 196, 232, 255));
        }
        APP_TERMINAL => {
            rounded_rect(fb, x, y, size, size, r, color(fb, 15, 21, 37));
            rounded_rect(fb, x + 2, y + 2, size.saturating_sub(4), size.saturating_sub(4), r.saturating_sub(2), color(fb, 27, 39, 57));
            let text_scale = if size >= 44 { 2 } else { 1 };
            crate::draw_text(fb, x + size / 4, y + size / 3, ">_", text_scale, color(fb, 105, 237, 187));
        }
        APP_NETWORK => {
            let node = (size / 7).max(3) as i32;
            let p1 = (x as i32 + size as i32 / 2, y as i32 + size as i32 / 4);
            let p2 = (x as i32 + size as i32 / 4, y as i32 + size as i32 * 3 / 4);
            let p3 = (x as i32 + size as i32 * 3 / 4, y as i32 + size as i32 * 3 / 4);
            draw_thick_line(fb, p1.0, p1.1, p2.0, p2.1, 3, color(fb, 103, 233, 204));
            draw_thick_line(fb, p1.0, p1.1, p3.0, p3.1, 3, color(fb, 103, 233, 204));
            draw_thick_line(fb, p2.0, p2.1, p3.0, p3.1, 3, color(fb, 103, 233, 204));
            fill_circle(fb, p1.0, p1.1, node, color(fb, 123, 251, 225));
            fill_circle(fb, p2.0, p2.1, node, color(fb, 123, 251, 225));
            fill_circle(fb, p3.0, p3.1, node, color(fb, 123, 251, 225));
        }
        APP_CLOCK => {
            let cx = (x + size / 2) as i32;
            let cy = (y + size / 2) as i32;
            fill_circle(fb, cx, cy, (size / 2) as i32, color(fb, 77, 111, 216));
            fill_circle(fb, cx, cy, (size / 2).saturating_sub(3) as i32, color(fb, 226, 239, 255));
            draw_thick_line(fb, cx, cy, cx, cy - (size as i32 / 4), 2, color(fb, 39, 55, 93));
            draw_thick_line(fb, cx, cy, cx + (size as i32 / 5), cy + (size as i32 / 6), 2, color(fb, 39, 55, 93));
            fill_circle(fb, cx, cy, (size / 14).max(2) as i32, color(fb, 63, 113, 220));
        }
        APP_SETTINGS => {
            let cx = (x + size / 2) as i32;
            let cy = (y + size / 2) as i32;
            rounded_rect(fb, x, y, size, size, r, color(fb, 104, 117, 143));
            fill_circle(fb, cx, cy, (size * 3 / 10) as i32, color(fb, 223, 231, 245));
            fill_circle(fb, cx, cy, (size / 8).max(2) as i32, color(fb, 80, 94, 119));
            for (dx, dy) in [(0, -1), (1, -1), (1, 0), (1, 1), (0, 1), (-1, 1), (-1, 0), (-1, -1)] {
                crate::fill_rect(fb,
                    (cx + dx * (size as i32 / 3) - 2).max(0) as usize,
                    (cy + dy * (size as i32 / 3) - 2).max(0) as usize, 4, 4, color(fb, 182, 194, 216));
            }
        }
        _ => {
            rounded_rect(fb, x, y, size, size, r, color(fb, 51, 170, 224));
            rounded_rect(fb, x + 2, y + 2, size.saturating_sub(4), size.saturating_sub(4), r.saturating_sub(1), color(fb, 33, 99, 168));
            let scale = if size >= 44 { 4 } else { 2 };
            crate::draw_text(fb, x + size / 2 - (6 * scale) / 2, y + size / 2 - (7 * scale) / 2, "X", scale, color(fb, 245, 251, 255));
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
            crate::draw_text(fb, left, row_y, "EDIT AND SAVE WITH CONFIG IN TERMINAL", 1, color(fb, 155, 176, 207));
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
    if x < 0 || y < 0 { return; }

    let x = x as usize;
    let y = y as usize;
    let fb_width = fb.width as usize;
    let fb_height = fb.height as usize;
    if x >= fb_width || y >= fb_height { return; }

    background.x = x;
    background.y = y;
    background.width = CURSOR_SAVE_WIDTH.min(fb_width - x);
    background.height = CURSOR_SAVE_HEIGHT.min(fb_height - y);
    for row_index in 0..background.height {
        let row = unsafe { fb.address.add((y + row_index) * fb.pitch as usize) as *const u32 };
        for col in 0..background.width {
            background.pixels[row_index * CURSOR_SAVE_WIDTH + col] = unsafe {
                core::ptr::read_volatile(row.add(x + col))
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
    // Small black outline plus white interior; visible on both dark and bright wallpaper.
    for row in 0..17i32 {
        let width = (row / 2 + 1).max(1);
        for col in 0..width {
            let px = x + col;
            let py = y + row;
            if row == 16 || col == width - 1 || col == 0 {
                if px >= 0 && py >= 0 { crate::fill_rect(fb, px as usize, py as usize, 1, 1, color(fb, 4, 8, 17)); }
            } else if px >= 0 && py >= 0 {
                crate::fill_rect(fb, px as usize, py as usize, 1, 1, color(fb, 250, 252, 255));
            }
        }
    }
    draw_thick_line(fb, x + 6, y + 11, x + 10, y + 18, 2, color(fb, 3, 7, 16));
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
