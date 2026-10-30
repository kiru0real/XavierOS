//! XavierOS in-kernel desktop and login manager for v0.2.2.
//!
//! This module provides the framebuffer compositor, desktop windows, file manager,
//! system actions, and application panels. Applications still run in kernel space;
//! this is not a user-mode GUI process model. Mouse input is polled from the legacy
//! PS/2 auxiliary port and keyboard navigation remains available.

use crate::{clock, config, interrupts, net, users, Framebuffer, KeyboardKey, MemoryStats};

const APP_COUNT: usize = 6;
const CURSOR_WIDTH: usize = 24;
const CURSOR_HEIGHT: usize = 24;
const CURSOR_SAVE_WIDTH: usize = CURSOR_WIDTH;
const CURSOR_SAVE_HEIGHT: usize = CURSOR_HEIGHT;
const CURSOR_HOTSPOT_X: i32 = 7;
const CURSOR_HOTSPOT_Y: i32 = 4;
const CURSOR_RGBA: &[u8] = include_bytes!("../assets/cursor.rgba");
const DOCK_LABEL_SAVE_WIDTH: usize = 64;
const DOCK_LABEL_SAVE_HEIGHT: usize = 21;
const APP_FILES: usize = 0;
const APP_TERMINAL: usize = 1;
const APP_NETWORK: usize = 2;
const APP_CLOCK: usize = 3;
const APP_SETTINGS: usize = 4;
const APP_ABOUT: usize = 5;
const BUILD_LABEL: &str = "XAVIEROS 0.2.2";
const WALLPAPER_WIDTH: usize = 1470;
const WALLPAPER_HEIGHT: usize = 904;
// RGB24 pixels generated from the user-supplied wallpaper. Keeping the image as
// raw RGB avoids needing a JPEG decoder inside this no_std kernel.
const WALLPAPER_RGB: &[u8] = include_bytes!("../assets/wallpaper.rgb");
const APP_NAMES: [&str; APP_COUNT] = ["FILES", "TERMINAL", "NETWORK", "CLOCK", "SETTINGS", "ABOUT"];

const FILE_PATH_CAPACITY: usize = 192;
const FILE_NAME_CAPACITY: usize = 64;
const FILE_PREVIEW_CAPACITY: usize = 160;
const FILES_PAGE_SIZE: usize = 6;
const FILES_HISTORY_CAPACITY: usize = 8;
const WINDOW_BACKDROP_WIDTH: usize = 705;
const WINDOW_BACKDROP_HEIGHT: usize = 428;

// A window is at most 700x420, plus its drop shadow. Saving just this region
// avoids repainting the wallpaper line-by-line when closing an application.
static mut WINDOW_BACKDROP_PIXELS: [u32; WINDOW_BACKDROP_WIDTH * WINDOW_BACKDROP_HEIGHT] =
    [0; WINDOW_BACKDROP_WIDTH * WINDOW_BACKDROP_HEIGHT];
static mut WINDOW_BACKDROP_X: usize = 0;
static mut WINDOW_BACKDROP_Y: usize = 0;
static mut WINDOW_BACKDROP_W: usize = 0;
static mut WINDOW_BACKDROP_H: usize = 0;
static mut WINDOW_BACKDROP_VALID: bool = false;

#[derive(Clone, Copy)]
struct DisplayMode {
    width: usize,
    height: usize,
    aspect: &'static str,
    quality: &'static str,
}

const DISPLAY_MODES: [DisplayMode; 3] = [
    DisplayMode { width: 1024, height: 768, aspect: "4:3 ASPECT", quality: "STANDARD" },
    DisplayMode { width: 1280, height: 720, aspect: "16:9 ASPECT", quality: "HD READY" },
    DisplayMode { width: 1366, height: 768, aspect: "16:9 ASPECT", quality: "WIDE HD" },
];

#[derive(Clone, Copy, PartialEq, Eq)]
enum DisplayNotice {
    None,
    Selected,
    Applied,
    Unsupported,
    NotEnoughMemory,
    Rejected,
}

#[derive(Clone, Copy)]
struct Rect {
    x: usize,
    y: usize,
    width: usize,
    height: usize,
}

impl Rect {
    fn contains(self, x: i32, y: i32) -> bool {
        x >= self.x as i32 && y >= self.y as i32
            && x < (self.x + self.width) as i32
            && y < (self.y + self.height) as i32
    }

    fn contains_slop(self, x: i32, y: i32, horizontal: usize, vertical: usize) -> bool {
        x >= self.x.saturating_sub(horizontal) as i32
            && y >= self.y.saturating_sub(vertical) as i32
            && x < self.x.saturating_add(self.width).saturating_add(horizontal) as i32
            && y < self.y.saturating_add(self.height).saturating_add(vertical) as i32
    }
}

struct SettingsLayout {
    sidebar: Rect,
    cards: [Rect; 3],
    apply: Rect,
    current: Rect,
}

enum SettingsHit {
    SelectMode(usize),
    Apply,
    UseCurrent,
}

#[derive(Clone, Copy)]
struct FileRow {
    name: [u8; FILE_NAME_CAPACITY],
    len: usize,
    is_dir: bool,
}

impl FileRow {
    fn empty() -> Self {
        Self { name: [0; FILE_NAME_CAPACITY], len: 0, is_dir: false }
    }
}

struct FilesView {
    path: [u8; FILE_PATH_CAPACITY],
    path_len: usize,
    page_start: usize,
    highlighted: usize,
    selected_path: [u8; FILE_PATH_CAPACITY],
    selected_path_len: usize,
    selected_name: [u8; FILE_NAME_CAPACITY],
    selected_name_len: usize,
    preview: [u8; FILE_PREVIEW_CAPACITY],
    preview_len: usize,
    notice: [u8; 96],
    notice_len: usize,
    prompt: [u8; FILE_NAME_CAPACITY],
    prompt_len: usize,
    prompt_mode: u8,
    history_paths: [[u8; FILE_PATH_CAPACITY]; FILES_HISTORY_CAPACITY],
    history_lens: [usize; FILES_HISTORY_CAPACITY],
    history_count: usize,
    favorite_path: [u8; FILE_PATH_CAPACITY],
    favorite_path_len: usize,
}

impl FilesView {
    fn new() -> Self {
        let mut path = [0; FILE_PATH_CAPACITY];
        let mut path_len = 1usize;
        path[0] = b'/';
        let mut home_buffer = [0u8; 48];
        if let Some(home) = users::home_path(&mut home_buffer) {
            let length = home.len().min(path.len());
            path[..length].copy_from_slice(&home.as_bytes()[..length]);
            path_len = length;
        }
        Self {
            path,
            path_len,
            page_start: 0,
            highlighted: 0,
            selected_path: [0; FILE_PATH_CAPACITY],
            selected_path_len: 0,
            selected_name: [0; FILE_NAME_CAPACITY],
            selected_name_len: 0,
            preview: [0; FILE_PREVIEW_CAPACITY],
            preview_len: 0,
            notice: [0; 96],
            notice_len: 0,
            prompt: [0; FILE_NAME_CAPACITY],
            prompt_len: 0,
            prompt_mode: 0,
            history_paths: [[0; FILE_PATH_CAPACITY]; FILES_HISTORY_CAPACITY],
            history_lens: [0; FILES_HISTORY_CAPACITY],
            history_count: 0,
            favorite_path: [0; FILE_PATH_CAPACITY],
            favorite_path_len: 0,
        }
    }
}

struct FilesLayout {
    content_x: usize,
    content_w: usize,
    path: Rect,
    back: Rect,
    up: Rect,
    previous: Rect,
    next: Rect,
    pin: Rect,
    new_directory: Rect,
    new_file: Rect,
    delete: Rect,
    rows: Rect,
    preview: Rect,
}

#[derive(Clone, Copy)]
enum FilesHit {
    Home,
    Disk,
    System,
    Favorite,
    Back,
    Up,
    Previous,
    Next,
    Pin,
    NewDirectory,
    NewFile,
    Delete,
    Row(usize),
}

#[derive(Clone, Copy)]
enum NetworkHit {
    Gateway,
    Dns,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum ClockHit {
    SyncNtp,
    TimezoneMinus,
    TimezonePlus,
    SaveTimezone,
}

struct DesktopState {
    selected: usize,
    open_window: Option<usize>,
    mouse_x: i32,
    mouse_y: i32,
    mouse_buttons: u8,
    mouse_ready: bool,
    selected_resolution: usize,
    display_notice: DisplayNotice,
    system_menu_open: bool,
    system_menu_selected: usize,
    files: FilesView,
    app_notice: [u8; 96],
    app_notice_len: usize,
    about_tab: usize,
}

// Save only the pixels underneath the small pointer. Redrawing the full desktop
// for every PS/2 packet caused visible flicker and wasted a large amount of CPU.
pub(super) struct CursorBackground {
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
    // The graphical Login Manager runs before the terminal is opened. Enable IRQ0/IRQ1
    // here so keyboard scancodes reach keyboard_poll_key() during first-run setup.
    // interactive_shell() enables interrupts too, but it is reached only after login.
    interrupts::enable_interrupts();

    // The desktop keeps a local copy because QEMU's Bochs VBE adapter may change
    // the live framebuffer dimensions while Settings is open.
    let mut active_framebuffer = *framebuffer;
    let mut width = active_framebuffer.width as usize;
    let mut height = active_framebuffer.height as usize;
    if users::is_initialized() {
        crate::serial_write("Starting graphical Login Manager.\r\n");
        login_manager(&active_framebuffer, mouse_ready);
    } else {
        crate::serial_write("Login Manager skipped: local user database is unavailable.\r\n");
    }
    let mut state = DesktopState {
        selected: APP_TERMINAL,
        open_window: None,
        mouse_x: (width / 2) as i32,
        mouse_y: (height.saturating_sub(150)) as i32,
        mouse_buttons: 0,
        mouse_ready,
        selected_resolution: display_mode_index(width, height),
        display_notice: DisplayNotice::None,
        system_menu_open: false,
        system_menu_selected: 0,
        files: FilesView::new(),
        app_notice: [0; 96],
        app_notice_len: 0,
        about_tab: 0,
    };
    let mut cursor_background = CursorBackground::new();
    let mut dock_label_background = DockLabelBackground::new();

    crate::serial_write("XavierOS Desktop entered. Terminal opens in a window; press ESC to close it.\r\n");
    render_desktop(&active_framebuffer, &state, memory_summary, &mut dock_label_background);
    if state.mouse_ready {
        draw_mouse_cursor_saved(&active_framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
    }

    let mut last_dynamic_second = interrupts::timer_ticks() / 100;
    loop {
        let window_before_input = state.open_window;
        let system_menu_before_input = state.system_menu_open;
        let now_second = interrupts::timer_ticks() / 100;
        let dynamic_refresh = now_second != last_dynamic_second
            && matches!(state.open_window, Some(APP_CLOCK) | Some(APP_NETWORK));
        last_dynamic_second = now_second;
        let mut force_full_repaint = false;
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
                if state.system_menu_open {
                    match system_menu_hit(state.mouse_x, state.mouse_y) {
                        Some(0) => { state.open_window = None; state.system_menu_open = false; repaint = true; }
                        Some(1) => { state.open_window = Some(APP_TERMINAL); state.system_menu_open = false; launch_terminal = true; repaint = true; }
                        Some(2) => { state.open_window = Some(APP_ABOUT); state.system_menu_open = false; repaint = true; }
                        Some(3) => {
                            state.open_window = None;
                            state.system_menu_open = false;
                            lock_screen(&active_framebuffer, state.mouse_ready);
                            force_full_repaint = true;
                            repaint = true;
                        }
                        Some(4) => {
                            open_login_manager_after_session_end(
                                &active_framebuffer,
                                &mut state,
                                "SELECT AN ACCOUNT TO SWITCH TO",
                            );
                            force_full_repaint = true;
                            repaint = true;
                        }
                        Some(5) => {
                            open_login_manager_after_session_end(
                                &active_framebuffer,
                                &mut state,
                                "SIGNED OUT — SELECT AN ACCOUNT",
                            );
                            force_full_repaint = true;
                            repaint = true;
                        }
                        Some(6) => reboot_from_desktop(),
                        Some(7) => shutdown_from_desktop(),
                        _ => { state.system_menu_open = false; repaint = true; }
                    }
                } else if state.mouse_y < 42 && state.mouse_x < 132 {
                    state.system_menu_open = true;
                    state.system_menu_selected = 0;
                    repaint = true;
                } else if let Some(index) = dock_hit_test(width, height, state.mouse_x, state.mouse_y) {
                    state.selected = index;
                    if index == APP_TERMINAL {
                        state.open_window = Some(APP_TERMINAL);
                        launch_terminal = true;
                        repaint = true;
                    } else {
                        state.open_window = Some(index);
                        repaint = true;
                    }
                } else if state.open_window == Some(APP_FILES) {
                    if close_button_hit(&active_framebuffer, state.mouse_x, state.mouse_y) {
                        state.open_window = None;
                        repaint = true;
                    } else if let Some(hit) = files_hit_test(&active_framebuffer, state.mouse_x, state.mouse_y, &state.files) {
                        handle_files_hit(&mut state, hit);
                        repaint = true;
                    }
                } else if state.open_window == Some(APP_NETWORK) {
                    if close_button_hit(&active_framebuffer, state.mouse_x, state.mouse_y) {
                        state.open_window = None;
                        repaint = true;
                    } else if let Some(action) = network_hit_test(&active_framebuffer, state.mouse_x, state.mouse_y) {
                        run_network_action(&mut state, action);
                        repaint = true;
                    }
                } else if state.open_window == Some(APP_CLOCK) {
                    if close_button_hit(&active_framebuffer, state.mouse_x, state.mouse_y) {
                        state.open_window = None;
                        repaint = true;
                    } else if let Some(action) = clock_hit_test(&active_framebuffer, state.mouse_x, state.mouse_y) {
                        run_clock_action(&mut state, action);
                        repaint = true;
                    }
                } else if state.open_window == Some(APP_ABOUT) {
                    if close_button_hit(&active_framebuffer, state.mouse_x, state.mouse_y) {
                        state.open_window = None;
                        repaint = true;
                    } else if let Some(tab) = about_tab_hit_test(&active_framebuffer, state.mouse_x, state.mouse_y) {
                        state.about_tab = tab;
                        repaint = true;
                    }
                } else if state.open_window == Some(APP_SETTINGS) {
                    match settings_hit_test(&active_framebuffer, state.mouse_x, state.mouse_y) {
                        Some(SettingsHit::SelectMode(index)) => {
                            state.selected_resolution = index;
                            state.display_notice = DisplayNotice::Selected;
                            repaint = true;
                        }
                        Some(SettingsHit::Apply) => {
                            let changed = apply_selected_resolution(
                                &mut active_framebuffer,
                                &mut state,
                                &mut width,
                                &mut height,
                                memory_summary,
                            );
                            force_full_repaint |= changed;
                            repaint = true;
                        }
                        Some(SettingsHit::UseCurrent) => {
                            state.selected_resolution = display_mode_index(width, height);
                            state.display_notice = DisplayNotice::None;
                            repaint = true;
                        }
                        None => {
                            if close_button_hit(&active_framebuffer, state.mouse_x, state.mouse_y) {
                                state.open_window = None;
                                repaint = true;
                            }
                        }
                    }
                } else if state.open_window.is_some() && close_button_hit(&active_framebuffer, state.mouse_x, state.mouse_y) {
                    state.open_window = None;
                    repaint = true;
                }
            }
            state.mouse_buttons = packet.buttons;
        }

        if let Some(key) = crate::keyboard_poll_key() {
            if state.system_menu_open {
                match key {
                    KeyboardKey::HistoryUp => { state.system_menu_selected = (state.system_menu_selected + SYSTEM_MENU_ITEMS.len() - 1) % SYSTEM_MENU_ITEMS.len(); repaint = true; }
                    KeyboardKey::HistoryDown | KeyboardKey::Tab => { state.system_menu_selected = (state.system_menu_selected + 1) % SYSTEM_MENU_ITEMS.len(); repaint = true; }
                    KeyboardKey::Escape => { state.system_menu_open = false; repaint = true; }
                    KeyboardKey::Enter => match state.system_menu_selected {
                        0 => { state.open_window = None; state.system_menu_open = false; repaint = true; }
                        1 => { state.open_window = Some(APP_TERMINAL); state.system_menu_open = false; launch_terminal = true; repaint = true; }
                        2 => { state.open_window = Some(APP_ABOUT); state.system_menu_open = false; repaint = true; }
                        3 => {
                            state.open_window = None;
                            state.system_menu_open = false;
                            lock_screen(&active_framebuffer, state.mouse_ready);
                            force_full_repaint = true;
                            repaint = true;
                        }
                        4 => {
                            open_login_manager_after_session_end(
                                &active_framebuffer,
                                &mut state,
                                "SELECT AN ACCOUNT TO SWITCH TO",
                            );
                            force_full_repaint = true;
                            repaint = true;
                        }
                        5 => {
                            open_login_manager_after_session_end(
                                &active_framebuffer,
                                &mut state,
                                "SIGNED OUT — SELECT AN ACCOUNT",
                            );
                            force_full_repaint = true;
                            repaint = true;
                        }
                        6 => reboot_from_desktop(),
                        _ => shutdown_from_desktop(),
                    },
                    _ => {}
                }
            } else if state.open_window == Some(APP_FILES) {
                handle_files_key(&mut state, key);
                repaint = true;
            } else if state.open_window == Some(APP_NETWORK) {
                match key {
                    KeyboardKey::Character(byte) if byte.to_ascii_uppercase() == b'G' => { run_network_action(&mut state, NetworkHit::Gateway); repaint = true; }
                    KeyboardKey::Character(byte) if byte.to_ascii_uppercase() == b'D' => { run_network_action(&mut state, NetworkHit::Dns); repaint = true; }
                    KeyboardKey::Escape => { state.open_window = None; repaint = true; }
                    _ => {}
                }
            } else if state.open_window == Some(APP_CLOCK) {
                match key {
                    KeyboardKey::Character(byte) if byte.to_ascii_uppercase() == b'N' => { run_clock_action(&mut state, ClockHit::SyncNtp); repaint = true; }
                    KeyboardKey::Character(byte) if byte.to_ascii_uppercase() == b'M' => { run_clock_action(&mut state, ClockHit::TimezoneMinus); repaint = true; }
                    KeyboardKey::Character(byte) if byte.to_ascii_uppercase() == b'P' => { run_clock_action(&mut state, ClockHit::TimezonePlus); repaint = true; }
                    KeyboardKey::Character(byte) if byte.to_ascii_uppercase() == b'S' => { run_clock_action(&mut state, ClockHit::SaveTimezone); repaint = true; }
                    KeyboardKey::Escape => { state.open_window = None; repaint = true; }
                    _ => {}
                }
            } else if state.open_window == Some(APP_ABOUT) {
                match key {
                    KeyboardKey::HistoryUp => { state.about_tab = (state.about_tab + 2) % 3; repaint = true; }
                    KeyboardKey::HistoryDown | KeyboardKey::Tab => { state.about_tab = (state.about_tab + 1) % 3; repaint = true; }
                    KeyboardKey::Escape => { state.open_window = None; repaint = true; }
                    _ => {}
                }
            } else { match key {
                KeyboardKey::CursorLeft | KeyboardKey::HistoryUp => {
                    if state.open_window == Some(APP_SETTINGS) {
                        state.selected_resolution = (state.selected_resolution + DISPLAY_MODES.len() - 1) % DISPLAY_MODES.len();
                        state.display_notice = DisplayNotice::Selected;
                        repaint = true;
                    } else {
                        state.selected = (state.selected + APP_COUNT - 1) % APP_COUNT;
                        dock_repaint = true;
                    }
                }
                KeyboardKey::CursorRight | KeyboardKey::HistoryDown | KeyboardKey::Tab => {
                    if state.open_window == Some(APP_SETTINGS) {
                        state.selected_resolution = (state.selected_resolution + 1) % DISPLAY_MODES.len();
                        state.display_notice = DisplayNotice::Selected;
                        repaint = true;
                    } else {
                        state.selected = (state.selected + 1) % APP_COUNT;
                        dock_repaint = true;
                    }
                }
                KeyboardKey::Enter => {
                    if state.open_window == Some(APP_SETTINGS) {
                        let changed = apply_selected_resolution(
                            &mut active_framebuffer,
                            &mut state,
                            &mut width,
                            &mut height,
                            memory_summary,
                        );
                        force_full_repaint |= changed;
                        repaint = true;
                    } else if state.selected == APP_TERMINAL {
                        state.open_window = Some(APP_TERMINAL);
                        launch_terminal = true;
                        repaint = true;
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
                    b'T' => { state.open_window = Some(APP_TERMINAL); launch_terminal = true; repaint = true; },
                    b'F' => { state.selected = APP_FILES; state.open_window = Some(APP_FILES); repaint = true; }
                    b'N' => { state.selected = APP_NETWORK; state.open_window = Some(APP_NETWORK); repaint = true; }
                    b'C' => { state.selected = APP_CLOCK; state.open_window = Some(APP_CLOCK); repaint = true; }
                    b'S' => { state.selected = APP_SETTINGS; state.open_window = Some(APP_SETTINGS); repaint = true; }
                    b'A' => { state.selected = APP_ABOUT; state.open_window = Some(APP_ABOUT); repaint = true; }
                    b'X' => { state.system_menu_open = true; state.system_menu_selected = 0; repaint = true; }
                    _ => {}
                },
                _ => {}
            }}
        }

        if launch_terminal {
            // Draw the normal app frame first, then render the existing shell into
            // a sub-framebuffer clipped to the Terminal window's content area.
            cursor_background.valid = false;
            dock_label_background.valid = false;
            render_desktop(&active_framebuffer, &state, memory_summary, &mut dock_label_background);
            if state.mouse_ready {
                draw_mouse_cursor_saved(&active_framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
            }
            let (window_x, window_y, window_w, window_h) = app_window_geometry(&active_framebuffer);
            let content_x = window_x + 8;
            let content_y = window_y + 40;
            let content_w = window_w.saturating_sub(16);
            let content_h = window_h.saturating_sub(62);
            let mut terminal_framebuffer = active_framebuffer;
            terminal_framebuffer.address = unsafe {
                active_framebuffer.address.add(content_y * active_framebuffer.pitch as usize + content_x * 4)
            };
            terminal_framebuffer.width = content_w as u64;
            terminal_framebuffer.height = content_h as u64;
            crate::run_console_window(
                &terminal_framebuffer,
                &active_framebuffer,
                memory_summary,
                state.mouse_ready,
                &mut state.mouse_x,
                &mut state.mouse_y,
                &mut state.mouse_buttons,
                &mut cursor_background,
            );
            state.open_window = None;
            repaint = false;
            restore_cursor_background(&active_framebuffer, &mut cursor_background);
            restore_window_backdrop(&active_framebuffer);
            draw_desktop_chrome(&active_framebuffer, &state, &mut dock_label_background);
            if state.mouse_ready {
                draw_mouse_cursor_saved(&active_framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
            }
        }

        if repaint {
            if force_full_repaint {
                cursor_background.valid = false;
                dock_label_background.valid = false;
                render_desktop(&active_framebuffer, &state, memory_summary, &mut dock_label_background);
            } else if window_before_input.is_some() && state.open_window.is_none() {
                restore_cursor_background(&active_framebuffer, &mut cursor_background);
                restore_window_backdrop(&active_framebuffer);
                draw_desktop_chrome(&active_framebuffer, &state, &mut dock_label_background);
            } else if !force_full_repaint
                && window_before_input.is_some()
                && state.open_window == window_before_input
                && state.system_menu_open == system_menu_before_input
            {
                restore_cursor_background(&active_framebuffer, &mut cursor_background);
                render_window_only(&active_framebuffer, &state, memory_summary);
            } else {
                cursor_background.valid = false;
                dock_label_background.valid = false;
                render_desktop(&active_framebuffer, &state, memory_summary, &mut dock_label_background);
            }
            if state.mouse_ready {
                draw_mouse_cursor_saved(&active_framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
            }
        } else if dynamic_refresh && state.open_window.is_some() {
            restore_cursor_background(&active_framebuffer, &mut cursor_background);
            render_window_only(&active_framebuffer, &state, memory_summary);
            if state.mouse_ready {
                draw_mouse_cursor_saved(&active_framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
            }
        } else if dock_repaint || cursor_moved {
            if state.mouse_ready {
                restore_cursor_background(&active_framebuffer, &mut cursor_background);
            }
            if dock_repaint {
                restore_dock_label_background(&active_framebuffer, &mut dock_label_background);
                save_dock_label_background(&active_framebuffer, state.selected, &mut dock_label_background);
                draw_dock(&active_framebuffer, state.selected);
            }
            if state.mouse_ready {
                draw_mouse_cursor_saved(&active_framebuffer, state.mouse_x, state.mouse_y, &mut cursor_background);
            }
        }

        interrupts::wait_for_interrupt();
    }
}

fn open_login_manager_after_session_end(
    fb: &Framebuffer,
    state: &mut DesktopState,
    notice: &'static str,
) {
    let mouse_ready = state.mouse_ready;
    // Logging out leaves the kernel in the passwordless LIVE-USER context. Always
    // show the picker afterwards so the user gets visible confirmation and can
    // sign in to another account instead of silently remaining on the desktop.
    if !users::is_live() {
        if let Err(error) = users::logout() {
            crate::serial_write("Desktop: logout failed: ");
            crate::serial_write(error);
            crate::serial_write("\r\n");
            return;
        }
    }
    state.open_window = None;
    state.system_menu_open = false;
    state.files = FilesView::new();
    login_manager_with_notice(fb, mouse_ready, notice);
    // A successful sign-in may have changed the current account and therefore
    // its home directory; rebuild the Files view after returning from the picker.
    state.files = FilesView::new();
}

const LOGIN_USER_LIMIT: usize = 8;
const LOGIN_NAME_CAPACITY: usize = 16;
const LOGIN_PASSWORD_CAPACITY: usize = 63;

#[derive(Clone, Copy, PartialEq, Eq)]
enum LoginView { ChooseUser, Password }

// The first-run/login screens are rendered before the regular desktop mouse loop.
// Keep a small independent pointer state so the supplied cursor remains visible
// while the user creates an account or enters a password.
struct LoginPointer {
    x: i32,
    y: i32,
    ready: bool,
    background: CursorBackground,
}

impl LoginPointer {
    fn new(fb: &Framebuffer, ready: bool) -> Self {
        let width = fb.width as usize;
        let height = fb.height as usize;
        Self {
            x: (width / 2) as i32,
            y: (height / 2).max(1) as i32,
            ready,
            background: CursorBackground::new(),
        }
    }

    fn after_repaint(&mut self, fb: &Framebuffer) {
        // The whole login screen has just been repainted, so the old saved cursor
        // background is stale and must not be copied over the new screen.
        self.background.valid = false;
        if self.ready {
            draw_mouse_cursor_saved(fb, self.x, self.y, &mut self.background);
        }
    }

    fn poll(&mut self, fb: &Framebuffer) {
        if !self.ready { return; }
        let Some(packet) = interrupts::poll_mouse_packet() else { return; };
        let width = fb.width as usize;
        let height = fb.height as usize;
        let next_x = (self.x + packet.dx as i32).clamp(1, width.saturating_sub(2) as i32);
        let next_y = (self.y - packet.dy as i32).clamp(1, height.saturating_sub(2) as i32);
        let moved = next_x != self.x || next_y != self.y;
        if moved {
            restore_cursor_background(fb, &mut self.background);
            self.x = next_x;
            self.y = next_y;
            draw_mouse_cursor_saved(fb, self.x, self.y, &mut self.background);
        }
    }
}

fn login_manager(fb: &Framebuffer, mouse_ready: bool) {
    login_manager_with_notice(fb, mouse_ready, "SELECT AN ACCOUNT, THEN PRESS ENTER");
}

fn login_manager_with_notice(fb: &Framebuffer, mouse_ready: bool, initial_notice: &'static str) {
    if !users::is_initialized() { return; }
    let mut pointer = LoginPointer::new(fb, mouse_ready);
    if users::count() == 0 {
        first_run_setup(fb, mouse_ready);
        return;
    }

    let mut view = LoginView::ChooseUser;
    let mut selected = 0usize;
    let mut selected_name = [0u8; LOGIN_NAME_CAPACITY];
    let mut selected_name_len = 0usize;
    let mut password = [0u8; LOGIN_PASSWORD_CAPACITY];
    let mut password_len = 0usize;
    let mut notice = initial_notice;
    let mut repaint = true;

    loop {
        if repaint {
            match view {
                LoginView::ChooseUser => draw_user_picker(fb, selected, notice),
                LoginView::Password => draw_password_prompt(fb, &selected_name[..selected_name_len], &password[..password_len], notice, false),
            }
            pointer.after_repaint(fb);
            repaint = false;
        }
        pointer.poll(fb);
        match crate::keyboard_poll_key() {
            Some(crate::KeyboardKey::HistoryUp) if view == LoginView::ChooseUser => {
                let rows = users::count() + 1;
                selected = (selected + rows - 1) % rows;
                notice = "SELECT AN ACCOUNT, THEN PRESS ENTER";
                repaint = true;
            }
            Some(crate::KeyboardKey::HistoryDown) if view == LoginView::ChooseUser => {
                let rows = users::count() + 1;
                selected = (selected + 1) % rows;
                notice = "SELECT AN ACCOUNT, THEN PRESS ENTER";
                repaint = true;
            }
            Some(crate::KeyboardKey::Enter) if view == LoginView::ChooseUser => {
                if selected == users::count() {
                    crate::serial_write("Login Manager: LIVE-USER guest session selected.\r\n");
                    return;
                }
                if let Some((name, length)) = login_username_at(selected) {
                    selected_name = name;
                    selected_name_len = length;
                    password.fill(0);
                    password_len = 0;
                    notice = "ENTER YOUR PASSWORD";
                    view = LoginView::Password;
                    repaint = true;
                }
            }
            Some(crate::KeyboardKey::Character(byte)) if view == LoginView::Password && is_login_character(byte) && password_len < LOGIN_PASSWORD_CAPACITY => {
                password[password_len] = byte;
                password_len += 1;
                notice = "PRESS ENTER TO SIGN IN";
                repaint = true;
            }
            Some(crate::KeyboardKey::Backspace) if view == LoginView::Password && password_len > 0 => {
                password_len -= 1;
                password[password_len] = 0;
                notice = "PRESS ENTER TO SIGN IN";
                repaint = true;
            }
            Some(crate::KeyboardKey::Enter) if view == LoginView::Password => {
                let name = core::str::from_utf8(&selected_name[..selected_name_len]).unwrap_or("");
                match users::authenticate(name, &password[..password_len]) {
                    Ok(()) => {
                        password.fill(0);
                        crate::serial_write("Login Manager: user authenticated.\r\n");
                        return;
                    }
                    Err(error) => {
                        notice = error;
                        password.fill(0);
                        password_len = 0;
                        repaint = true;
                    }
                }
            }
            Some(crate::KeyboardKey::Escape) if view == LoginView::Password => {
                password.fill(0);
                password_len = 0;
                view = LoginView::ChooseUser;
                notice = "SELECT AN ACCOUNT, THEN PRESS ENTER";
                repaint = true;
            }
            _ => {}
        }
        // Repaint only after an input/state change; PIT wakeups must not redraw the full screen.
        interrupts::wait_for_interrupt();
    }
}

fn first_run_setup(fb: &Framebuffer, mouse_ready: bool) {
    let mut pointer = LoginPointer::new(fb, mouse_ready);
    let mut username = [0u8; LOGIN_NAME_CAPACITY];
    let mut username_len = 0usize;
    let mut password = [0u8; LOGIN_PASSWORD_CAPACITY];
    let mut password_len = 0usize;
    let mut confirm = [0u8; LOGIN_PASSWORD_CAPACITY];
    let mut confirm_len = 0usize;
    let mut field = 0usize;
    let mut notice = "CREATE THE FIRST ADMINISTRATOR ACCOUNT";
    let mut repaint = true;

    loop {
        if repaint {
            draw_first_run(fb, &username[..username_len], &password[..password_len], &confirm[..confirm_len], field, notice);
            pointer.after_repaint(fb);
            repaint = false;
        }
        pointer.poll(fb);
        match crate::keyboard_poll_key() {
            Some(crate::KeyboardKey::Character(byte)) if field == 0 && is_login_character(byte) && username_len < 15 => {
                username[username_len] = byte.to_ascii_lowercase();
                username_len += 1;
                notice = "USERNAME: 1-15 CHARS; USE LETTERS, DIGITS, _ OR -";
                repaint = true;
            }
            Some(crate::KeyboardKey::Character(byte)) if field == 1 && is_login_character(byte) && password_len < LOGIN_PASSWORD_CAPACITY => {
                password[password_len] = byte;
                password_len += 1;
                notice = "PASSWORD MUST HAVE AT LEAST 8 CHARACTERS";
                repaint = true;
            }
            Some(crate::KeyboardKey::Character(byte)) if field == 2 && is_login_character(byte) && confirm_len < LOGIN_PASSWORD_CAPACITY => {
                confirm[confirm_len] = byte;
                confirm_len += 1;
                notice = "RE-ENTER THE PASSWORD TO CONFIRM";
                repaint = true;
            }
            Some(crate::KeyboardKey::Backspace) if field == 0 && username_len > 0 => { username_len -= 1; username[username_len] = 0; repaint = true; }
            Some(crate::KeyboardKey::Backspace) if field == 1 && password_len > 0 => { password_len -= 1; password[password_len] = 0; repaint = true; }
            Some(crate::KeyboardKey::Backspace) if field == 2 && confirm_len > 0 => { confirm_len -= 1; confirm[confirm_len] = 0; repaint = true; }
            Some(crate::KeyboardKey::Tab) => { field = (field + 1) % 3; notice = "TAB CHANGES FIELD; ENTER ADVANCES"; repaint = true; }
            Some(crate::KeyboardKey::Enter) if field < 2 => { field += 1; notice = "ENTER THE NEXT FIELD"; repaint = true; }
            Some(crate::KeyboardKey::Enter) if field == 2 => {
                if password_len != confirm_len || password[..password_len] != confirm[..confirm_len] {
                    notice = "PASSWORDS DO NOT MATCH";
                    confirm.fill(0);
                    confirm_len = 0;
                    repaint = true;
                } else {
                    let name = core::str::from_utf8(&username[..username_len]).unwrap_or("");
                    match users::create(name, &password[..password_len]) {
                        Ok(()) => match users::authenticate(name, &password[..password_len]) {
                            Ok(()) => {
                                password.fill(0);
                                confirm.fill(0);
                                crate::serial_write("Login Manager: first administrator account created and signed in.\r\n");
                                return;
                            }
                            Err(error) => { notice = error; repaint = true; }
                        },
                        Err(error) => { notice = error; repaint = true; }
                    }
                }
            }
            _ => {}
        }
        interrupts::wait_for_interrupt();
    }
}

fn lock_screen(fb: &Framebuffer, mouse_ready: bool) {
    if users::is_live() {
        login_manager(fb, mouse_ready);
        return;
    }
    let mut pointer = LoginPointer::new(fb, mouse_ready);
    let mut name_buffer = [0u8; LOGIN_NAME_CAPACITY];
    let name = users::current_name(&mut name_buffer);
    let name_bytes = name.as_bytes();
    let mut password = [0u8; LOGIN_PASSWORD_CAPACITY];
    let mut password_len = 0usize;
    let mut notice = "THIS SESSION IS LOCKED - ENTER YOUR PASSWORD";
    let mut repaint = true;
    loop {
        if repaint {
            draw_password_prompt(fb, name_bytes, &password[..password_len], notice, true);
            pointer.after_repaint(fb);
            repaint = false;
        }
        pointer.poll(fb);
        match crate::keyboard_poll_key() {
            Some(crate::KeyboardKey::Character(byte)) if is_login_character(byte) && password_len < LOGIN_PASSWORD_CAPACITY => {
                password[password_len] = byte;
                password_len += 1;
                repaint = true;
            }
            Some(crate::KeyboardKey::Backspace) if password_len > 0 => { password_len -= 1; password[password_len] = 0; repaint = true; }
            Some(crate::KeyboardKey::Enter) => match users::verify_current_password(&password[..password_len]) {
                Ok(()) => { password.fill(0); crate::serial_write("Login Manager: session unlocked.\r\n"); return; }
                Err(error) => { notice = error; password.fill(0); password_len = 0; repaint = true; }
            },
            _ => {}
        }
        interrupts::wait_for_interrupt();
    }
}

fn login_username_at(wanted: usize) -> Option<([u8; LOGIN_NAME_CAPACITY], usize)> {
    let mut names = [[0u8; LOGIN_NAME_CAPACITY]; LOGIN_USER_LIMIT];
    let mut lengths = [0usize; LOGIN_USER_LIMIT];
    let mut count = 0usize;
    users::list(|name| {
        if count < LOGIN_USER_LIMIT {
            lengths[count] = name.len().min(LOGIN_NAME_CAPACITY);
            names[count][..lengths[count]].copy_from_slice(&name.as_bytes()[..lengths[count]]);
            count += 1;
        }
    });
    if wanted >= count { return None; }
    Some((names[wanted], lengths[wanted]))
}

fn draw_user_picker(fb: &Framebuffer, selected: usize, notice: &str) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    crate::fill_rect(fb, 0, 0, width, height, color(fb, 12, 18, 31));
    let rows = users::count() + 1;
    let panel_w = 560usize.min(width.saturating_sub(24));
    let panel_h = (174 + rows * 34).min(height.saturating_sub(24));
    let x = width.saturating_sub(panel_w) / 2;
    let y = height.saturating_sub(panel_h) / 2;
    rounded_rect(fb, x, y, panel_w, panel_h, 16, color(fb, 23, 33, 53));
    crate::draw_text(fb, x + 28, y + 24, "XAVIEROS", 2, color(fb, 106, 218, 255));
    crate::draw_text(fb, x + 28, y + 46, "LOGIN MANAGER", 1, color(fb, 231, 238, 248));
    crate::draw_text(fb, x + 28, y + 72, "CHOOSE A USER ACCOUNT", 1, color(fb, 151, 170, 196));
    let row_x = x + 20;
    let row_y = y + 96;
    let mut row = 0usize;
    users::list(|name| {
        let py = row_y + row * 34;
        if row == selected { rounded_rect(fb, row_x, py, panel_w.saturating_sub(40), 30, 6, color(fb, 42, 81, 116)); }
        crate::draw_text(fb, row_x + 14, py + 10, name, 1, color(fb, 235, 243, 252));
        crate::draw_text(fb, row_x + panel_w.saturating_sub(116), py + 10, if row == 0 { "ADMIN" } else { "USER" }, 1, color(fb, 123, 211, 186));
        row += 1;
    });
    let py = row_y + row * 34;
    if selected == row { rounded_rect(fb, row_x, py, panel_w.saturating_sub(40), 30, 6, color(fb, 42, 81, 116)); }
    crate::draw_text(fb, row_x + 14, py + 10, "LIVE-USER", 1, color(fb, 235, 243, 252));
    crate::draw_text(fb, row_x + panel_w.saturating_sub(116), py + 10, "GUEST", 1, color(fb, 226, 194, 124));
    crate::draw_text(fb, x + 28, y + panel_h.saturating_sub(28), notice, 1, color(fb, 163, 186, 213));
    crate::draw_text(fb, x + 28, height.saturating_sub(20), "UP/DOWN SELECT  ENTER CONTINUE", 1, color(fb, 105, 128, 157));
}

fn draw_password_prompt(fb: &Framebuffer, name: &[u8], password: &[u8], notice: &str, locked: bool) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    crate::fill_rect(fb, 0, 0, width, height, color(fb, 12, 18, 31));
    let panel_w = 500usize.min(width.saturating_sub(24));
    let panel_h = 330usize.min(height.saturating_sub(24));
    let x = width.saturating_sub(panel_w) / 2;
    let y = height.saturating_sub(panel_h) / 2;
    rounded_rect(fb, x, y, panel_w, panel_h, 16, color(fb, 23, 33, 53));
    crate::draw_text(fb, x + 28, y + 28, if locked { "SESSION LOCKED" } else { "WELCOME BACK" }, 2, color(fb, 106, 218, 255));
    let name = core::str::from_utf8(name).unwrap_or("USER");
    crate::draw_text(fb, x + 28, y + 76, name, 2, color(fb, 235, 243, 252));
    crate::draw_text(fb, x + 28, y + 130, "PASSWORD", 1, color(fb, 151, 170, 196));
    rounded_rect(fb, x + 24, y + 150, panel_w.saturating_sub(48), 40, 6, color(fb, 10, 15, 26));
    let mut mask = [0u8; LOGIN_PASSWORD_CAPACITY];
    mask[..password.len()].fill(b'*');
    let masked = core::str::from_utf8(&mask[..password.len()]).unwrap_or("");
    crate::draw_text(fb, x + 36, y + 166, masked, 1, color(fb, 245, 248, 252));
    let caret_x = x + 36 + masked.len() * 6;
    crate::fill_rect(fb, caret_x, y + 165, 2, 9, color(fb, 106, 218, 255));
    crate::draw_text(fb, x + 28, y + 218, notice, 1, color(fb, 239, 170, 146));
    crate::draw_text(fb, x + 28, y + panel_h.saturating_sub(28), if locked { "ENTER UNLOCKS THIS SESSION" } else { "ENTER SIGN IN  ESC BACK" }, 1, color(fb, 105, 128, 157));
}

fn draw_first_run(fb: &Framebuffer, username: &[u8], password: &[u8], confirm: &[u8], field: usize, notice: &str) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    crate::fill_rect(fb, 0, 0, width, height, color(fb, 12, 18, 31));
    let panel_w = 560usize.min(width.saturating_sub(24));
    let panel_h = 400usize.min(height.saturating_sub(24));
    let x = width.saturating_sub(panel_w) / 2;
    let y = height.saturating_sub(panel_h) / 2;
    rounded_rect(fb, x, y, panel_w, panel_h, 16, color(fb, 23, 33, 53));
    crate::draw_text(fb, x + 28, y + 24, "WELCOME TO XAVIEROS", 2, color(fb, 106, 218, 255));
    crate::draw_text(fb, x + 28, y + 58, "FIRST-RUN ACCOUNT SETUP", 1, color(fb, 231, 238, 248));
    crate::draw_text(fb, x + 28, y + 94, "CREATE THE FIRST ADMINISTRATOR", 1, color(fb, 151, 170, 196));
    let labels = ["USERNAME", "PASSWORD", "CONFIRM PASSWORD"];
    let values: [&[u8]; 3] = [username, password, confirm];
    for index in 0..3 {
        let py = y + 126 + index * 58;
        crate::draw_text(fb, x + 28, py, labels[index], 1, color(fb, if field == index { 106 } else { 151 }, if field == index { 218 } else { 170 }, if field == index { 255 } else { 196 }));
        rounded_rect(fb, x + 24, py + 14, panel_w.saturating_sub(48), 32, 5, color(fb, if field == index { 36 } else { 10 }, if field == index { 53 } else { 15 }, if field == index { 75 } else { 26 }));
        let mut mask = [0u8; LOGIN_PASSWORD_CAPACITY];
        let show = if index == 0 {
            core::str::from_utf8(values[index]).unwrap_or("")
        } else {
            mask[..values[index].len()].fill(b'*');
            core::str::from_utf8(&mask[..values[index].len()]).unwrap_or("")
        };
        crate::draw_text(fb, x + 36, py + 26, show, 1, color(fb, 245, 248, 252));
        if field == index {
            let caret_x = x + 36 + show.len() * 6;
            let caret_y = py + 25;
            crate::fill_rect(fb, caret_x, caret_y, 2, 9, color(fb, 106, 218, 255));
        }
    }
    crate::draw_text(fb, x + 28, y + panel_h.saturating_sub(42), notice, 1, color(fb, 239, 170, 146));
    crate::draw_text(fb, x + 28, y + panel_h.saturating_sub(22), "TAB SELECTS FIELD  ENTER CONTINUES", 1, color(fb, 105, 128, 157));
}

fn is_login_character(byte: u8) -> bool { (0x20..=0x7E).contains(&byte) }

fn render_desktop(
    fb: &Framebuffer,
    state: &DesktopState,
    memory_summary: Option<(MemoryStats, bool)>,
    dock_label_background: &mut DockLabelBackground,
) {
    draw_wallpaper(fb);
    if let Some(app) = state.open_window {
        save_window_backdrop(fb);
        draw_app_window(fb, app, memory_summary, state);
    }
    draw_menu_bar(fb, state.mouse_ready);
    save_dock_label_background(fb, state.selected, dock_label_background);
    draw_dock(fb, state.selected);
    if state.system_menu_open { draw_system_menu(fb, state.system_menu_selected); }
}

fn draw_desktop_chrome(fb: &Framebuffer, state: &DesktopState, dock_label_background: &mut DockLabelBackground) {
    restore_dock_label_background(fb, dock_label_background);
    draw_menu_bar(fb, state.mouse_ready);
    save_dock_label_background(fb, state.selected, dock_label_background);
    draw_dock(fb, state.selected);
    if state.system_menu_open { draw_system_menu(fb, state.system_menu_selected); }
}

fn render_window_only(fb: &Framebuffer, state: &DesktopState, memory_summary: Option<(MemoryStats, bool)>) {
    if let Some(app) = state.open_window {
        restore_window_backdrop(fb);
        save_window_backdrop(fb);
        draw_app_window(fb, app, memory_summary, state);
        draw_menu_bar(fb, state.mouse_ready);
        if state.system_menu_open { draw_system_menu(fb, state.system_menu_selected); }
    }
}

fn save_window_backdrop(fb: &Framebuffer) {
    let (x, y, window_w, window_h) = app_window_geometry(fb);
    let width = (window_w + 5).min(WINDOW_BACKDROP_WIDTH).min((fb.width as usize).saturating_sub(x));
    let height = (window_h + 8).min(WINDOW_BACKDROP_HEIGHT).min((fb.height as usize).saturating_sub(y));
    if width == 0 || height == 0 { return; }
    unsafe {
        WINDOW_BACKDROP_X = x;
        WINDOW_BACKDROP_Y = y;
        WINDOW_BACKDROP_W = width;
        WINDOW_BACKDROP_H = height;
        WINDOW_BACKDROP_VALID = false;
        let cache = core::ptr::addr_of_mut!(WINDOW_BACKDROP_PIXELS).cast::<u32>();
        for row_index in 0..height {
            let source = fb.address.add((y + row_index) * fb.pitch as usize).cast::<u32>();
            for column in 0..width {
                core::ptr::write(cache.add(row_index * WINDOW_BACKDROP_WIDTH + column),
                    core::ptr::read_volatile(source.add(x + column)));
            }
        }
        WINDOW_BACKDROP_VALID = true;
    }
}

fn restore_window_backdrop(fb: &Framebuffer) {
    unsafe {
        if !WINDOW_BACKDROP_VALID { return; }
        let x = WINDOW_BACKDROP_X;
        let y = WINDOW_BACKDROP_Y;
        let width = WINDOW_BACKDROP_W.min((fb.width as usize).saturating_sub(x));
        let height = WINDOW_BACKDROP_H.min((fb.height as usize).saturating_sub(y));
        let cache = core::ptr::addr_of!(WINDOW_BACKDROP_PIXELS).cast::<u32>();
        for row_index in 0..height {
            let destination = fb.address.add((y + row_index) * fb.pitch as usize).cast::<u32>();
            for column in 0..width {
                core::ptr::write_volatile(destination.add(x + column),
                    core::ptr::read(cache.add(row_index * WINDOW_BACKDROP_WIDTH + column)));
            }
        }
        WINDOW_BACKDROP_VALID = false;
    }
}

fn draw_wallpaper(fb: &Framebuffer) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    if width == 0 || height == 0 || WALLPAPER_RGB.len() < WALLPAPER_WIDTH * WALLPAPER_HEIGHT * 3 {
        return;
    }

    // The original 735x452 wallpaper is stored as a high-quality 2x Lanczos
    // upscale with a restrained unsharp mask, then bilinear interpolation during cover mapping gives
    // smoother gradients and fewer block edges at the supported display modes.
    const FP_SHIFT: u32 = 16;
    const FP_ONE: u64 = 1 << FP_SHIFT;
    let width_u = width as u64;
    let height_u = height as u64;
    let source_w = WALLPAPER_WIDTH as u64;
    let source_h = WALLPAPER_HEIGHT as u64;
    let target_is_wider = width_u * source_h >= height_u * source_w;
    let step_fp: u64;
    let start_x_fp: u64;
    let start_y_fp: u64;
    if target_is_wider {
        step_fp = ((source_w << FP_SHIFT) / width_u).max(1);
        start_x_fp = 0;
        start_y_fp = ((width_u * source_h - height_u * source_w) << (FP_SHIFT - 1)) / width_u;
    } else {
        step_fp = ((source_h << FP_SHIFT) / height_u).max(1);
        start_x_fp = ((height_u * source_w - width_u * source_h) << (FP_SHIFT - 1)) / height_u;
        start_y_fp = 0;
    }

    for y in 0..height {
        let source_y_fp = start_y_fp.saturating_add(y as u64 * step_fp);
        let source_y0 = ((source_y_fp >> FP_SHIFT) as usize).min(WALLPAPER_HEIGHT - 1);
        let source_y1 = (source_y0 + 1).min(WALLPAPER_HEIGHT - 1);
        let fy = (source_y_fp & (FP_ONE - 1)) as u32;
        let row = unsafe { fb.address.add(y * fb.pitch as usize) as *mut u32 };
        for x in 0..width {
            let source_x_fp = start_x_fp.saturating_add(x as u64 * step_fp);
            let source_x0 = ((source_x_fp >> FP_SHIFT) as usize).min(WALLPAPER_WIDTH - 1);
            let source_x1 = (source_x0 + 1).min(WALLPAPER_WIDTH - 1);
            let fx = (source_x_fp & (FP_ONE - 1)) as u32;
            let top_left = (source_y0 * WALLPAPER_WIDTH + source_x0) * 3;
            let top_right = (source_y0 * WALLPAPER_WIDTH + source_x1) * 3;
            let bottom_left = (source_y1 * WALLPAPER_WIDTH + source_x0) * 3;
            let bottom_right = (source_y1 * WALLPAPER_WIDTH + source_x1) * 3;
            let interpolate = |channel: usize| -> u8 {
                let a = WALLPAPER_RGB[top_left + channel] as u64;
                let b = WALLPAPER_RGB[top_right + channel] as u64;
                let c = WALLPAPER_RGB[bottom_left + channel] as u64;
                let d = WALLPAPER_RGB[bottom_right + channel] as u64;
                let top = a * (FP_ONE - fx as u64) + b * fx as u64;
                let bottom = c * (FP_ONE - fx as u64) + d * fx as u64;
                let mixed = top * (FP_ONE - fy as u64) + bottom * fy as u64;
                ((mixed + (1u64 << 31)) >> 32).min(255) as u8
            };
            let pixel = color(fb, interpolate(0), interpolate(1), interpolate(2));
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

const SYSTEM_MENU_ITEMS: [&str; 8] = [
    "DESKTOP", "OPEN TERMINAL", "ABOUT XAVIEROS", "LOCK SCREEN",
    "SWITCH USER", "LOG OUT", "RESTART", "SHUT DOWN",
];

fn draw_system_menu(fb: &Framebuffer, selected: usize) {
    let x = 12usize;
    let y = 46usize;
    let w = 224usize;
    let row_h = 34usize;
    let h = 49 + row_h * SYSTEM_MENU_ITEMS.len();
    rounded_rect(fb, x + 3, y + 5, w, h, 10, color(fb, 3, 5, 12));
    rounded_rect(fb, x, y, w, h, 10, color(fb, 22, 31, 51));
    crate::draw_text(fb, x + 14, y + 12, "XAVIEROS SYSTEM", 1, color(fb, 100, 220, 255));
    for (i, item) in SYSTEM_MENU_ITEMS.iter().enumerate() {
        let row_y = y + 31 + i * row_h;
        if i == selected {
            rounded_rect(fb, x + 7, row_y, w - 14, row_h - 2, 6, color(fb, 43, 72, 103));
        }
        let text_color = if i >= 6 { color(fb, 255, 170, 160) } else { color(fb, 226, 237, 251) };
        crate::draw_text(fb, x + 17, row_y + 11, item, 1, text_color);
    }
}

fn system_menu_hit(x: i32, y: i32) -> Option<usize> {
    if !(12..=236).contains(&x) || y < 77 || y >= 77 + (SYSTEM_MENU_ITEMS.len() as i32 * 34) { return None; }
    let index = ((y - 77) / 34) as usize;
    if index < SYSTEM_MENU_ITEMS.len() { Some(index) } else { None }
}

fn reboot_from_desktop() -> ! {
    crate::serial_write("Desktop requested reboot.\r\n");
    unsafe {
        for _ in 0..100_000 {
            let status: u8;
            core::arch::asm!("in al, dx", in("dx") 0x64u16, out("al") status, options(nomem, nostack, preserves_flags));
            if status & 0x02 == 0 {
                core::arch::asm!("out dx, al", in("dx") 0x64u16, in("al") 0xFEu8, options(nomem, nostack, preserves_flags));
                break;
            }
            core::hint::spin_loop();
        }
    }
    halt_desktop()
}

fn shutdown_from_desktop() -> ! {
    crate::serial_write("Desktop requested shutdown.\r\n");
    unsafe {
        // QEMU/Bochs-compatible ACPI shutdown ports. Firmware may ignore these on real hardware.
        core::arch::asm!("out dx, ax", in("dx") 0x604u16, in("ax") 0x2000u16, options(nomem, nostack, preserves_flags));
        core::arch::asm!("out dx, ax", in("dx") 0xB004u16, in("ax") 0x2000u16, options(nomem, nostack, preserves_flags));
    }
    halt_desktop()
}

fn halt_desktop() -> ! {
    loop { unsafe { core::arch::asm!("cli; hlt", options(nomem, nostack)); } }
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

fn draw_app_window(fb: &Framebuffer, app: usize, memory_summary: Option<(MemoryStats, bool)>, state: &DesktopState) {
    let width = fb.width as usize;
    let (x, y, win_w, win_h) = app_window_geometry(fb);
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
            draw_files_panel(fb, state);
        }
        APP_TERMINAL => {
            draw_app_sidebar(fb, x + 12, y + 48, 132, win_h.saturating_sub(60), "TERMINAL", &["SESSION", "COMMANDS", "HELP"], 0);
            let content_x = x + 160;
            let content_w = win_w.saturating_sub(180);
            draw_page_heading(fb, content_x, y + 56, "INTERACTIVE TERMINAL", "Run commands in a window, without leaving the desktop.");
            rounded_rect(fb, content_x, y + 100, content_w, 105, 10, color(fb, 8, 14, 26));
            crate::fill_rect(fb, content_x + 12, y + 114, 3, 76, color(fb, 93, 224, 178));
            crate::draw_text(fb, content_x + 28, y + 119, "XAVIEROS SHELL", 1, color(fb, 100, 229, 188));
            crate::draw_text(fb, content_x + 28, y + 145, "TYPE HELP TO EXPLORE COMMANDS.", 1, color(fb, 223, 234, 251));
            crate::draw_text(fb, content_x + 28, y + 166, "ESC CLOSES THIS WINDOW AND RETURNS TO DESKTOP.", 1, color(fb, 155, 177, 208));
            draw_stat_card(fb, content_x, y + 219, (content_w.saturating_sub(12)) / 2, 57, "INPUT", "KEYBOARD READY", color(fb, 104, 224, 255));
            draw_stat_card(fb, content_x + (content_w.saturating_sub(12)) / 2 + 12, y + 219, (content_w.saturating_sub(12)) / 2, 57, "SHELL", "NO STD / RUST", color(fb, 102, 232, 183));
        }
        APP_NETWORK => {
            draw_app_sidebar(fb, x + 12, y + 48, 132, win_h.saturating_sub(60), "NETWORK", &["STATUS", "ADAPTER", "DNS"], 0);
            let content_x = x + 160;
            let content_w = win_w.saturating_sub(180);
            draw_page_heading(fb, content_x, y + 56, "NETWORK OVERVIEW", "Adapter and address information.");
            let status_text = if net::is_ready() { "CONNECTED" } else { "OFFLINE" };
            rounded_rect(fb, content_x, y + 95, content_w, 48, 9, color(fb, 17, 31, 49));
            fill_circle(fb, (content_x + 17) as i32, (y + 119) as i32, 4, color(fb, if net::is_ready() { 98 } else { 255 }, if net::is_ready() { 229 } else { 152 }, if net::is_ready() { 178 } else { 145 }));
            crate::draw_text(fb, content_x + 31, y + 113, status_text, 1, color(fb, 239, 247, 255));
            crate::draw_text(fb, content_x + content_w.saturating_sub(132), y + 113, "QEMU E1000", 1, color(fb, 142, 171, 205));
            let gap = 10;
            let card_w = content_w.saturating_sub(gap) / 2;
            if let Some(info) = net::info() {
                let mac = format_mac(&info.mac);
                let local_ip = format_ipv4(info.local_ip);
                let gateway = format_ipv4(info.gateway);
                let dns = format_ipv4(info.dns);
                draw_stat_card(fb, content_x, y + 155, card_w, 63, "MAC ADDRESS", bytes_text(&mac), color(fb, 98, 215, 255));
                draw_stat_card(fb, content_x + card_w + gap, y + 155, card_w, 63, "IPV4 ADDRESS", bytes_text(&local_ip), color(fb, 103, 230, 183));
                draw_stat_card(fb, content_x, y + 228, card_w, 63, "GATEWAY", bytes_text(&gateway), color(fb, 255, 201, 112));
                draw_stat_card(fb, content_x + card_w + gap, y + 228, card_w, 63, "DNS SERVER", bytes_text(&dns), color(fb, 190, 150, 255));
            } else {
                rounded_rect(fb, content_x, y + 155, content_w, 88, 9, color(fb, 36, 28, 40));
                crate::draw_text(fb, content_x + 14, y + 173, "NO NETWORK INFORMATION AVAILABLE", 1, color(fb, 255, 171, 158));
                crate::draw_text(fb, content_x + 14, y + 196, "CHECK THE VIRTUAL NIC AND DHCP STATUS.", 1, color(fb, 194, 202, 221));
            }
            let gateway_button = network_action_rects(fb).0;
            let dns_button = network_action_rects(fb).1;
            draw_action_button(fb, gateway_button, "CHECK GATEWAY", color(fb, 26, 92, 111));
            draw_action_button(fb, dns_button, "CHECK DNS", color(fb, 24, 91, 78));
            let stats = net::statistics();
            let status_text = core::str::from_utf8(&state.app_notice[..state.app_notice_len]).unwrap_or("");
            crate::draw_text(fb, content_x, y + 347, if status_text.is_empty() { "CHECK GATEWAY CONNECTIVITY OR RESOLVE EXAMPLE.COM." } else { status_text }, 1, color(fb, 158, 214, 240));
            let mut tx = [0u8; 24];
            let mut rx = [0u8; 24];
            let tx_text = push_decimal_text(&mut tx, stats.tx_frames as u64);
            let rx_text = push_decimal_text(&mut rx, stats.rx_frames as u64);
            crate::draw_text(fb, content_x, y + 367, "TX FRAMES:", 1, color(fb, 119, 147, 183));
            crate::draw_text(fb, content_x + 66, y + 367, tx_text, 1, color(fb, 225, 237, 252));
            crate::draw_text(fb, content_x + 180, y + 367, "RX FRAMES:", 1, color(fb, 119, 147, 183));
            crate::draw_text(fb, content_x + 246, y + 367, rx_text, 1, color(fb, 225, 237, 252));
        }
        APP_CLOCK => {
            draw_app_sidebar(fb, x + 12, y + 48, 132, win_h.saturating_sub(60), "CLOCK", &["LOCAL TIME", "TIMEZONE", "SYNC"], 0);
            let content_x = x + 160;
            let content_w = win_w.saturating_sub(180);
            draw_page_heading(fb, content_x, y + 56, "TIME & DATE", "Your system clock at a glance.");
            rounded_rect(fb, content_x, y + 98, content_w, 108, 10, color(fb, 12, 23, 40));
            crate::draw_text(fb, content_x + 15, y + 113, "LOCAL TIME", 1, color(fb, 107, 222, 255));
            if let Ok(now) = clock::now_local() {
                let mut buffer = [0u8; 24];
                let text = format_datetime(&mut buffer, now);
                crate::draw_text(fb, content_x + 15, y + 141, text, 2, color(fb, 244, 248, 255));
                let mut time_buffer = [0u8; 24];
                let time = format_clock(&mut time_buffer);
                crate::draw_text(fb, content_x + 15, y + 177, time, 1, color(fb, 170, 196, 224));
            } else {
                crate::draw_text(fb, content_x + 15, y + 148, "CLOCK UNAVAILABLE", 2, color(fb, 255, 171, 158));
            }
            let card_w = content_w.saturating_sub(12) / 2;
            let timezone = format_timezone(clock::timezone_offset_minutes());
            draw_stat_card(fb, content_x, y + 219, card_w, 58, "TIMEZONE", bytes_text(&timezone), color(fb, 98, 215, 255));
            draw_stat_card(fb, content_x + card_w + 12, y + 219, card_w, 58, "TIME SOURCE", if clock::is_ntp_synchronized() { "NTP SYNCED" } else { "CMOS RTC" }, color(fb, 103, 230, 183));
            let clock_buttons = clock_action_rects(fb);
            draw_action_button(fb, clock_buttons.0, "SYNC NTP", color(fb, 26, 92, 111));
            draw_action_button(fb, clock_buttons.1, "TZ -1H", color(fb, 28, 43, 65));
            draw_action_button(fb, clock_buttons.2, "TZ +1H", color(fb, 28, 43, 65));
            draw_action_button(fb, clock_buttons.3, "SAVE TZ", color(fb, 24, 91, 78));
            let notice = core::str::from_utf8(&state.app_notice[..state.app_notice_len]).unwrap_or("");
            crate::draw_text(fb, content_x, y + 320, if notice.is_empty() { "SYNC THE CLOCK OR ADJUST TIMEZONE BELOW." } else { notice }, 1, color(fb, 158, 214, 240));
            crate::draw_text(fb, content_x, y + 344, "TIMEZONE IS A FIXED UTC OFFSET; DST IS NOT AUTOMATIC.", 1, color(fb, 119, 147, 183));
        }
        APP_SETTINGS => {
            draw_settings_panel(fb, state);
        }
        APP_ABOUT => {
            let items = &["OVERVIEW", "KERNEL", "LICENSE"];
            draw_app_sidebar(fb, x + 12, y + 48, 132, win_h.saturating_sub(60), "SYSTEM", items, state.about_tab.min(2));
            let content_x = x + 160;
            let content_w = win_w.saturating_sub(180);
            match state.about_tab {
                0 => {
                    draw_page_heading(fb, content_x, y + 56, "MEET XAVIEROS", "A small operating system built from the kernel up.");
                    rounded_rect(fb, content_x, y + 100, content_w, 79, 10, color(fb, 13, 26, 44));
                    rounded_rect(fb, content_x + 14, y + 113, 51, 51, 12, color(fb, 23, 81, 112));
                    draw_thick_line(fb, (content_x + 25) as i32, (y + 125) as i32, (content_x + 54) as i32, (y + 153) as i32, 4, color(fb, 112, 233, 255));
                    draw_thick_line(fb, (content_x + 54) as i32, (y + 125) as i32, (content_x + 25) as i32, (y + 153) as i32, 4, color(fb, 112, 233, 255));
                    crate::draw_text(fb, content_x + 79, y + 119, "XAVIEROS", 2, color(fb, 243, 248, 255));
                    crate::draw_text(fb, content_x + 79, y + 151, BUILD_LABEL, 1, color(fb, 110, 225, 255));
                    let card_w = content_w.saturating_sub(16) / 3;
                    draw_stat_card(fb, content_x, y + 193, card_w, 62, "KERNEL", "RUST NO_STD", color(fb, 101, 215, 255));
                    draw_stat_card(fb, content_x + card_w + 8, y + 193, card_w, 62, "BOOT", "LIMINE", color(fb, 103, 230, 183));
                    draw_stat_card(fb, content_x + (card_w + 8) * 2, y + 193, card_w, 62, "ARCH", "X86_64", color(fb, 255, 199, 108));
                    crate::draw_text(fb, content_x, y + 278, "EXPERIMENTAL HOBBY OS  /  DIRECT FRAMEBUFFER UI", 1, color(fb, 162, 184, 215));
                }
                1 => {
                    draw_page_heading(fb, content_x, y + 56, "KERNEL ARCHITECTURE", "Core components currently included in the release.");
                    let card_w = content_w.saturating_sub(12) / 2;
                    draw_stat_card(fb, content_x, y + 101, card_w, 60, "BOOT", "LIMINE", color(fb, 98, 215, 255));
                    draw_stat_card(fb, content_x + card_w + 12, y + 101, card_w, 60, "MEMORY", "PMM / HEAP", color(fb, 103, 230, 183));
                    draw_stat_card(fb, content_x, y + 171, card_w, 60, "INPUT", "PS/2 KEY + MOUSE", color(fb, 255, 199, 108));
                    draw_stat_card(fb, content_x + card_w + 12, y + 171, card_w, 60, "NETWORK", "E1000 / IPV4", color(fb, 188, 145, 255));
                    crate::draw_text(fb, content_x, y + 253, "SHELL, VFS, XVDISKFS, CLOCK AND TASKING PROTOTYPE.", 1, color(fb, 162, 184, 215));
                    crate::draw_text(fb, content_x, y + 274, "APPS RUN IN KERNEL SPACE; PROCESS ISOLATION IS NOT PRESENT.", 1, color(fb, 255, 190, 137));
                }
                _ => {
                    draw_page_heading(fb, content_x, y + 56, "LICENSE & ACKNOWLEDGEMENTS", "Project license and third-party bootloader.");
                    rounded_rect(fb, content_x, y + 101, content_w, 100, 10, color(fb, 13, 26, 44));
                    crate::draw_text(fb, content_x + 15, y + 118, "MIT LICENSE", 2, color(fb, 112, 233, 255));
                    crate::draw_text(fb, content_x + 15, y + 150, "PERMISSION TO USE, COPY, MODIFY AND DISTRIBUTE.", 1, color(fb, 225, 234, 249));
                    crate::draw_text(fb, content_x + 15, y + 173, "SEE LICENSE FILE IN THE SOURCE ARCHIVE.", 1, color(fb, 155, 179, 211));
                    crate::draw_text(fb, content_x, y + 224, "BOOTLOADER: LIMINE", 1, color(fb, 103, 222, 255));
                    crate::draw_text(fb, content_x, y + 248, "HARDWARE SUPPORT AND USER-MODE ISOLATION ARE EXPERIMENTAL.", 1, color(fb, 255, 190, 137));
                }
            }
        }
        _ => {}
    }
    if app == APP_SETTINGS {
        let layout = settings_layout(fb);
        crate::draw_text(fb, layout.cards[0].x, y + win_h.saturating_sub(18), "ESC TO CLOSE  /  CLICK DOCK TO SWITCH APPS", 1, color(fb, 123, 150, 190));
    } else if app == APP_TERMINAL {
        crate::draw_text(fb, left, y + win_h.saturating_sub(18), "ESC TO CLOSE TERMINAL  /  SHELL INPUT ACTIVE", 1, color(fb, 123, 150, 190));
    } else if app == APP_FILES {
        crate::draw_text(fb, left, y + win_h.saturating_sub(18), "BACK/BACKSPACE  UP/ESC=PARENT  PIN=P  FAVORITE=V", 1, color(fb, 123, 150, 190));
    } else {
        let hint = "ESC TO CLOSE  /  CLICK DOCK TO SWITCH APPS";
        crate::draw_text(fb, left, y + win_h.saturating_sub(18), hint, 1, color(fb, 123, 150, 190));
    }
    let _ = memory_summary;
}

fn draw_page_heading(fb: &Framebuffer, x: usize, y: usize, title: &str, subtitle: &str) {
    crate::draw_text(fb, x, y, title, 2, color(fb, 239, 246, 255));
    crate::draw_text(fb, x, y + 24, subtitle, 1, color(fb, 155, 179, 211));
}

fn set_app_notice(state: &mut DesktopState, text: &str) {
    state.app_notice.fill(0);
    state.app_notice_len = text.len().min(state.app_notice.len());
    for (destination, source) in state.app_notice[..state.app_notice_len].iter_mut().zip(text.as_bytes()) {
        *destination = source.to_ascii_uppercase();
    }
}

fn app_window_content(fb: &Framebuffer) -> (usize, usize, usize) {
    let (x, y, win_w, _) = app_window_geometry(fb);
    (x + 160, y, win_w.saturating_sub(180))
}

fn network_action_rects(fb: &Framebuffer) -> (Rect, Rect) {
    let (content_x, y, _) = app_window_content(fb);
    (Rect { x: content_x, y: y + 305, width: 122, height: 29 }, Rect { x: content_x + 132, y: y + 305, width: 100, height: 29 })
}

fn clock_action_rects(fb: &Framebuffer) -> (Rect, Rect, Rect, Rect) {
    let (x, y, _) = app_window_content(fb);
    (Rect { x, y: y + 286, width: 88, height: 29 }, Rect { x: x + 96, y: y + 286, width: 63, height: 29 }, Rect { x: x + 167, y: y + 286, width: 63, height: 29 }, Rect { x: x + 238, y: y + 286, width: 75, height: 29 })
}

fn about_tab_hit_test(fb: &Framebuffer, x: i32, y: i32) -> Option<usize> {
    if x < 0 || y < 0 { return None; }
    let (left, top, _, height) = app_window_geometry(fb);
    let sidebar_x = left + 12;
    if x < sidebar_x as i32 || x >= (sidebar_x + 132) as i32 { return None; }
    for index in 0..3 {
        let row_y = top + 83 + index * 36;
        if y >= row_y as i32 && y < (row_y + 29) as i32 && row_y + 29 < top + height - 30 {
            return Some(index);
        }
    }
    None
}

fn network_hit_test(fb: &Framebuffer, x: i32, y: i32) -> Option<NetworkHit> {
    let (gateway, dns) = network_action_rects(fb);
    if gateway.contains(x, y) { Some(NetworkHit::Gateway) }
    else if dns.contains(x, y) { Some(NetworkHit::Dns) }
    else { None }
}

fn clock_hit_test(fb: &Framebuffer, x: i32, y: i32) -> Option<ClockHit> {
    let (sync, minus, plus, save) = clock_action_rects(fb);
    if sync.contains(x, y) { Some(ClockHit::SyncNtp) }
    else if minus.contains(x, y) { Some(ClockHit::TimezoneMinus) }
    else if plus.contains(x, y) { Some(ClockHit::TimezonePlus) }
    else if save.contains(x, y) { Some(ClockHit::SaveTimezone) }
    else { None }
}

fn draw_action_button(fb: &Framebuffer, rect: Rect, label: &str, shade: u32) {
    rounded_rect(fb, rect.x, rect.y, rect.width, rect.height, 5, shade);
    let max_chars = rect.width.saturating_sub(10) / 6;
    crate::draw_text(fb, rect.x + 5, rect.y + 11, &label[..label.len().min(max_chars)], 1, color(fb, 237, 246, 255));
}

fn run_network_action(state: &mut DesktopState, action: NetworkHit) {
    match action {
        NetworkHit::Gateway => match crate::net::check_gateway() {
            Ok(()) => set_app_notice(state, "GATEWAY CHECK PASSED"),
            Err(error) => set_app_notice(state, error),
        },
        NetworkHit::Dns => match crate::net::check_dns() {
            Ok(_address) => set_app_notice(state, "DNS RESOLUTION SUCCEEDED"),
            Err(error) => set_app_notice(state, error),
        },
    }
}

fn run_clock_action(state: &mut DesktopState, action: ClockHit) {
    match action {
        ClockHit::SyncNtp => {
            let settings = crate::config::current();
            match crate::clock::synchronize(settings.ntp_server_str()) {
                Ok(_) => set_app_notice(state, "TIME SYNCHRONIZED WITH NTP"),
                Err(error) => set_app_notice(state, error),
            }
        }
        ClockHit::TimezoneMinus | ClockHit::TimezonePlus => {
            let delta = if matches!(action, ClockHit::TimezoneMinus) { -60 } else { 60 };
            let current = crate::clock::timezone_offset_minutes();
            let next = current.saturating_add(delta);
            match crate::config::set_timezone_minutes(next) {
                Ok(()) => set_app_notice(state, "TIMEZONE CHANGED IN MEMORY; PRESS SAVE TZ"),
                Err(error) => set_app_notice(state, error),
            }
        }
        ClockHit::SaveTimezone => match crate::config::save() {
            Ok(()) => set_app_notice(state, "TIMEZONE SAVED TO DISK"),
            Err(error) => set_app_notice(state, error),
        },
    }
}

fn push_decimal_text(buffer: &mut [u8; 24], mut value: u64) -> &str {
    let mut reversed = [0u8; 20];
    let mut count = 0usize;
    loop {
        reversed[count] = b'0' + (value % 10) as u8;
        count += 1;
        value /= 10;
        if value == 0 { break; }
    }
    for index in 0..count { buffer[index] = reversed[count - index - 1]; }
    core::str::from_utf8(&buffer[..count]).unwrap_or("0")
}

fn files_layout(fb: &Framebuffer) -> FilesLayout {
    let (x, y, win_w, _win_h) = app_window_geometry(fb);
    let content_x = x + 160;
    let content_w = win_w.saturating_sub(180);
    let buttons_y = y + 132;
    let mut bx = content_x;
    let back = Rect { x: bx, y: buttons_y, width: 43, height: 29 }; bx += 46;
    let up = Rect { x: bx, y: buttons_y, width: 34, height: 29 }; bx += 37;
    let previous = Rect { x: bx, y: buttons_y, width: 42, height: 29 }; bx += 45;
    let next = Rect { x: bx, y: buttons_y, width: 42, height: 29 }; bx += 45;
    let pin = Rect { x: bx, y: buttons_y, width: 42, height: 29 }; bx += 45;
    let new_directory = Rect { x: bx, y: buttons_y, width: 62, height: 29 }; bx += 65;
    let new_file = Rect { x: bx, y: buttons_y, width: 58, height: 29 }; bx += 61;
    let delete = Rect { x: bx, y: buttons_y, width: 52, height: 29 };
    FilesLayout {
        content_x, content_w,
        path: Rect { x: content_x, y: y + 96, width: content_w, height: 30 },
        back, up, previous, next, pin, new_directory, new_file, delete,
        rows: Rect { x: content_x, y: y + 166, width: content_w, height: FILES_PAGE_SIZE * 25 },
        preview: Rect { x: content_x, y: y + 321, width: content_w, height: 74 },
    }
}

fn files_path(view: &FilesView) -> &str {
    core::str::from_utf8(&view.path[..view.path_len]).unwrap_or("/")
}

fn files_assign_path(view: &mut FilesView, path: &str) -> Result<(), &'static str> {
    if path.is_empty() || path.len() > FILE_PATH_CAPACITY { return Err("PATH TOO LONG"); }
    view.path.fill(0);
    view.path[..path.len()].copy_from_slice(path.as_bytes());
    view.path_len = path.len();
    view.page_start = 0;
    view.highlighted = 0;
    view.selected_path.fill(0);
    view.selected_path_len = 0;
    view.selected_name.fill(0);
    view.selected_name_len = 0;
    view.preview.fill(0);
    view.preview_len = 0;
    view.notice.fill(0);
    view.notice_len = 0;
    Ok(())
}

fn files_push_history(view: &mut FilesView) {
    let current_len = view.path_len;
    if current_len == 0 { return; }
    if view.history_count > 0 {
        let last = view.history_count - 1;
        if view.history_lens[last] == current_len
            && view.history_paths[last][..current_len] == view.path[..current_len]
        { return; }
    }
    if view.history_count == FILES_HISTORY_CAPACITY {
        for index in 1..FILES_HISTORY_CAPACITY {
            view.history_paths[index - 1] = view.history_paths[index];
            view.history_lens[index - 1] = view.history_lens[index];
        }
        view.history_count -= 1;
    }
    let slot = view.history_count;
    view.history_paths[slot].fill(0);
    view.history_paths[slot][..current_len].copy_from_slice(&view.path[..current_len]);
    view.history_lens[slot] = current_len;
    view.history_count += 1;
}

fn files_set_path(view: &mut FilesView, path: &str) -> Result<(), &'static str> {
    if path == files_path(view) {
        view.page_start = 0;
        view.highlighted = 0;
        view.selected_path.fill(0);
        view.selected_path_len = 0;
        view.selected_name.fill(0);
        view.selected_name_len = 0;
        view.preview.fill(0);
        view.preview_len = 0;
        files_set_notice(view, "ALREADY IN THIS FOLDER");
        return Ok(());
    }
    files_push_history(view);
    files_assign_path(view, path)
}

fn files_go_back(view: &mut FilesView) {
    if view.history_count == 0 {
        files_set_notice(view, "NO PREVIOUS FOLDER IN HISTORY");
        return;
    }
    view.history_count -= 1;
    let index = view.history_count;
    let length = view.history_lens[index].min(FILE_PATH_CAPACITY);
    let mut target = [0u8; FILE_PATH_CAPACITY];
    target[..length].copy_from_slice(&view.history_paths[index][..length]);
    let path = core::str::from_utf8(&target[..length]).unwrap_or("/");
    if let Err(error) = files_assign_path(view, path) {
        files_set_notice(view, error);
    } else {
        files_set_notice(view, "RETURNED TO PREVIOUS FOLDER");
    }
}

fn files_pin_current(view: &mut FilesView) {
    // Copy the current path first so the mutable favorite fields do not alias the
    // shared borrow returned by files_path().
    let path = files_path(view);
    let path_len = path.len();
    let mut current = [0u8; FILE_PATH_CAPACITY];
    if path_len == 0 || path_len > FILE_PATH_CAPACITY {
        files_set_notice(view, "FAVORITE PATH TOO LONG");
        return;
    }
    current[..path_len].copy_from_slice(path.as_bytes());
    view.favorite_path.fill(0);
    view.favorite_path[..path_len].copy_from_slice(&current[..path_len]);
    view.favorite_path_len = path_len;
    files_set_notice(view, "CURRENT FOLDER PINNED AS FAVORITE");
}

fn files_open_favorite(view: &mut FilesView) {
    if view.favorite_path_len == 0 {
        files_set_notice(view, "NO FAVORITE YET - OPEN A FOLDER AND PIN IT");
        return;
    }
    let favorite_len = view.favorite_path_len.min(FILE_PATH_CAPACITY);
    let mut target = [0u8; FILE_PATH_CAPACITY];
    target[..favorite_len].copy_from_slice(&view.favorite_path[..favorite_len]);
    let path = core::str::from_utf8(&target[..favorite_len]).unwrap_or("/");
    match crate::vfs::stat(path) {
        Ok(info) if info.is_dir && crate::users::may_access_path(path) => {
            let _ = files_set_path(view, path);
        }
        _ => files_set_notice(view, "FAVORITE FOLDER IS NO LONGER AVAILABLE"),
    }
}

fn files_set_notice(view: &mut FilesView, notice: &str) {
    view.notice.fill(0);
    view.notice_len = notice.len().min(view.notice.len());
    view.notice[..view.notice_len].copy_from_slice(&notice.as_bytes()[..view.notice_len]);
}

fn files_join_path(base: &str, leaf: &str, output: &mut [u8; FILE_PATH_CAPACITY]) -> Option<usize> {
    if leaf.is_empty() || leaf.contains('/') || leaf.contains('\\') || leaf == "." || leaf == ".." { return None; }
    let prefix = if base == "/" { 0 } else { base.len() };
    let required = prefix.checked_add(1)?.checked_add(leaf.len())?;
    if required > output.len() { return None; }
    output.fill(0);
    let mut used = 0;
    if prefix > 0 { output[..prefix].copy_from_slice(base.as_bytes()); used = prefix; }
    output[used] = b'/'; used += 1;
    output[used..used + leaf.len()].copy_from_slice(leaf.as_bytes());
    Some(used + leaf.len())
}

fn files_parent_path(path: &str) -> &str {
    if path == "/" { return "/"; }
    match path.rfind('/') {
        Some(0) | None => "/",
        Some(index) => &path[..index],
    }
}

fn files_get_row(path: &str, wanted: usize) -> Result<Option<FileRow>, &'static str> {
    let mut index = 0usize;
    let mut found = None;
    crate::vfs::list_directory(path, |name, is_dir| {
        if index == wanted && name.len() <= FILE_NAME_CAPACITY {
            let mut row = FileRow::empty();
            row.name[..name.len()].copy_from_slice(name.as_bytes());
            row.len = name.len();
            row.is_dir = is_dir;
            found = Some(row);
        }
        index += 1;
    })?;
    Ok(found)
}

fn files_activate_row(state: &mut DesktopState, index: usize) {
    let current = files_path(&state.files);
    let mut path_buf = [0u8; FILE_PATH_CAPACITY];
    path_buf[..current.len()].copy_from_slice(current.as_bytes());
    let current_owned = core::str::from_utf8(&path_buf[..current.len()]).unwrap_or("/");
    let row = match files_get_row(current_owned, index) {
        Ok(Some(row)) => row,
        Ok(None) => { files_set_notice(&mut state.files, "NO ITEM AT THIS POSITION"); return; }
        Err(error) => { files_set_notice(&mut state.files, error); return; }
    };
    let name = core::str::from_utf8(&row.name[..row.len]).unwrap_or("");
    let mut target_buf = [0u8; FILE_PATH_CAPACITY];
    let Some(target_len) = files_join_path(current_owned, name, &mut target_buf) else {
        files_set_notice(&mut state.files, "PATH TOO LONG OR INVALID NAME"); return;
    };
    let target = core::str::from_utf8(&target_buf[..target_len]).unwrap_or("");
    if !crate::users::may_access_path(target) {
        files_set_notice(&mut state.files, "ACCESS DENIED FOR THIS PATH");
        return;
    }
    if row.is_dir {
        if let Err(error) = files_set_path(&mut state.files, target) { files_set_notice(&mut state.files, error); }
        return;
    }
    state.files.selected_path.fill(0);
    state.files.selected_path[..target_len].copy_from_slice(target.as_bytes());
    state.files.selected_path_len = target_len;
    state.files.selected_name.fill(0);
    state.files.selected_name[..row.len].copy_from_slice(&row.name[..row.len]);
    state.files.selected_name_len = row.len;
    state.files.preview.fill(0);
    state.files.preview_len = 0;
    let mut contents = [0u8; 512];
    match crate::vfs::read_file(target, &mut contents) {
        Ok(size) => {
            let mut used = 0usize;
            for byte in contents[..size.min(contents.len())].iter().copied() {
                if used >= state.files.preview.len() { break; }
                if byte == b'\r' { continue; }
                if byte == b'\n' {
                    if used > 0 && state.files.preview[used - 1] != b'\n' { state.files.preview[used] = b'\n'; used += 1; }
                } else if byte == b'\t' {
                    state.files.preview[used] = b' ';
                    used += 1;
                } else if (32..=126).contains(&byte) {
                    state.files.preview[used] = byte.to_ascii_uppercase();
                    used += 1;
                } else {
                    state.files.preview[used] = b'.';
                    used += 1;
                }
            }
            while used > 0 && state.files.preview[used - 1] == b'\n' { used -= 1; }
            state.files.preview_len = used;
            files_set_notice(&mut state.files, "FILE PREVIEW LOADED");
        }
        Err(error) => files_set_notice(&mut state.files, error),
    }
}

fn files_is_writable_path(path: &str) -> bool {
    path.eq_ignore_ascii_case("/DISK")
        || path.get(..6).map(|prefix| prefix.eq_ignore_ascii_case("/DISK/")).unwrap_or(false)
}

fn files_begin_prompt(view: &mut FilesView, mode: u8) {
    let path = files_path(view);
    if !files_is_writable_path(path) {
        files_set_notice(view, "WRITES REQUIRE /DISK; OPEN DISK FIRST");
        return;
    }
    view.prompt_mode = mode;
    view.prompt.fill(0);
    view.prompt_len = 0;
    files_set_notice(view, if mode == 1 { "TYPE FOLDER NAME, THEN ENTER" } else { "TYPE FILE NAME, THEN ENTER" });
}

fn files_commit_prompt(view: &mut FilesView) {
    if view.prompt_mode == 0 { return; }
    let name = match core::str::from_utf8(&view.prompt[..view.prompt_len]) {
        Ok(name) => name,
        Err(_) => { files_set_notice(view, "INVALID UTF-8 NAME"); view.prompt_mode = 0; return; }
    };
    let base = files_path(view);
    let mut base_buf = [0u8; FILE_PATH_CAPACITY];
    base_buf[..base.len()].copy_from_slice(base.as_bytes());
    let base = core::str::from_utf8(&base_buf[..base.len()]).unwrap_or("/DISK");
    let mut path_buf = [0u8; FILE_PATH_CAPACITY];
    let Some(path_len) = files_join_path(base, name, &mut path_buf) else {
        files_set_notice(view, "INVALID NAME OR PATH TOO LONG"); view.prompt_mode = 0; return;
    };
    let path = core::str::from_utf8(&path_buf[..path_len]).unwrap_or("");
    if !crate::users::may_access_path(path) {
        files_set_notice(view, "ACCESS DENIED FOR THIS PATH"); view.prompt_mode = 0; return;
    }
    let result = if view.prompt_mode == 1 { crate::vfs::mkdir(path) } else { crate::vfs::touch(path) };
    view.prompt_mode = 0;
    view.prompt_len = 0;
    match result {
        Ok(()) => { view.page_start = 0; files_set_notice(view, "ITEM CREATED"); }
        Err(error) => files_set_notice(view, error),
    }
}

fn handle_files_key(state: &mut DesktopState, key: KeyboardKey) {
    if state.files.prompt_mode != 0 {
        match key {
            KeyboardKey::Escape => { state.files.prompt_mode = 0; state.files.prompt_len = 0; files_set_notice(&mut state.files, "CREATE CANCELLED"); }
            KeyboardKey::Backspace => {
                if state.files.prompt_len > 0 { state.files.prompt_len -= 1; state.files.prompt[state.files.prompt_len] = 0; }
            }
            KeyboardKey::Enter => files_commit_prompt(&mut state.files),
            KeyboardKey::Character(byte) if (32..=126).contains(&byte) && state.files.prompt_len < 48 => {
                state.files.prompt[state.files.prompt_len] = byte;
                state.files.prompt_len += 1;
            }
            _ => {}
        }
        return;
    }
    match key {
        KeyboardKey::HistoryUp => {
            if state.files.highlighted > 0 { state.files.highlighted -= 1; }
            else if state.files.page_start >= FILES_PAGE_SIZE { state.files.page_start -= FILES_PAGE_SIZE; }
        }
        KeyboardKey::HistoryDown => {
            let total = crate::vfs::list_directory(files_path(&state.files), |_, _| {}).unwrap_or(0);
            if state.files.highlighted + 1 < FILES_PAGE_SIZE && state.files.page_start + state.files.highlighted + 1 < total {
                state.files.highlighted += 1;
            } else if state.files.page_start + FILES_PAGE_SIZE < total {
                state.files.page_start += FILES_PAGE_SIZE;
                state.files.highlighted = 0;
            }
        }
        KeyboardKey::Enter => files_activate_row(state, state.files.page_start + state.files.highlighted),
        KeyboardKey::Backspace => files_go_back(&mut state.files),
        KeyboardKey::Escape => {
            let path = files_path(&state.files);
            if path != "/" {
                let parent = files_parent_path(path);
                let mut parent_buf = [0u8; FILE_PATH_CAPACITY];
                parent_buf[..parent.len()].copy_from_slice(parent.as_bytes());
                let parent = core::str::from_utf8(&parent_buf[..parent.len()]).unwrap_or("/");
                let _ = files_set_path(&mut state.files, parent);
            } else { state.open_window = None; }
        }
        KeyboardKey::Character(byte) => match byte.to_ascii_uppercase() {
            b'U' => { let path = files_path(&state.files); if path != "/" { let parent = files_parent_path(path); let mut buffer = [0u8; FILE_PATH_CAPACITY]; buffer[..parent.len()].copy_from_slice(parent.as_bytes()); let parent = core::str::from_utf8(&buffer[..parent.len()]).unwrap_or("/"); let _ = files_set_path(&mut state.files, parent); } }
            b'N' => files_begin_prompt(&mut state.files, 2),
            b'D' => files_begin_prompt(&mut state.files, 1),
            b'B' => files_go_back(&mut state.files),
            b'P' => files_pin_current(&mut state.files),
            b'V' => files_open_favorite(&mut state.files),
            _ => {}
        },
        _ => {}
    }
}

fn files_hit_test(fb: &Framebuffer, x: i32, y: i32, view: &FilesView) -> Option<FilesHit> {
    if x < 0 || y < 0 { return None; }
    let (window_x, window_y, _, _) = app_window_geometry(fb);
    let side_x = window_x + 12;
    let side_y = window_y + 83;
    if x >= (side_x - 7) as i32 && x < (side_x + 125) as i32 {
        for index in 0..4 {
            let row_y = side_y + index * 36;
            if y >= row_y as i32 - 3 && y < (row_y + 29) as i32 + 3 {
                return Some(match index { 0 => FilesHit::Home, 1 => FilesHit::Disk, 2 => FilesHit::System, _ => FilesHit::Favorite });
            }
        }
    }
    let layout = files_layout(fb);
    // The cursor hotspot is aligned to the visible arrow tip; a small target slop
    // compensates for relative PS/2 motion granularity on compact toolbar buttons.
    if layout.back.contains_slop(x, y, 2, 5) { return Some(FilesHit::Back); }
    if layout.up.contains_slop(x, y, 2, 5) { return Some(FilesHit::Up); }
    if layout.previous.contains_slop(x, y, 2, 5) { return Some(FilesHit::Previous); }
    if layout.next.contains_slop(x, y, 2, 5) { return Some(FilesHit::Next); }
    if layout.pin.contains_slop(x, y, 2, 5) { return Some(FilesHit::Pin); }
    if layout.new_directory.contains_slop(x, y, 2, 5) { return Some(FilesHit::NewDirectory); }
    if layout.new_file.contains_slop(x, y, 2, 5) { return Some(FilesHit::NewFile); }
    if layout.delete.contains_slop(x, y, 2, 5) { return Some(FilesHit::Delete); }
    if layout.rows.contains(x, y) {
        let row = ((y as usize - layout.rows.y) / 25).min(FILES_PAGE_SIZE - 1);
        let total = crate::vfs::list_directory(files_path(view), |_, _| {}).ok()?;
        let index = view.page_start + row;
        if index < total { return Some(FilesHit::Row(index)); }
    }
    None
}

fn handle_files_hit(state: &mut DesktopState, hit: FilesHit) {
    match hit {
        FilesHit::Home => { let _ = files_set_path(&mut state.files, "/"); }
        FilesHit::Disk => {
            if !crate::users::may_access_path("/DISK") {
                files_set_notice(&mut state.files, "ACCESS DENIED FOR THIS DISK PATH");
            } else if crate::vfs::exists("/DISK") {
                let _ = files_set_path(&mut state.files, "/DISK");
            } else { files_set_notice(&mut state.files, "WRITABLE DISK IS NOT MOUNTED"); }
        }
        FilesHit::System => { let _ = files_set_path(&mut state.files, "/ETC"); }
        FilesHit::Favorite => files_open_favorite(&mut state.files),
        FilesHit::Back => files_go_back(&mut state.files),
        FilesHit::Pin => files_pin_current(&mut state.files),
        FilesHit::Up => {
            let path = files_path(&state.files);
            if path == "/" { files_set_notice(&mut state.files, "ALREADY AT ROOT"); }
            else { let parent = files_parent_path(path); let mut buffer = [0u8; FILE_PATH_CAPACITY]; buffer[..parent.len()].copy_from_slice(parent.as_bytes()); let parent = core::str::from_utf8(&buffer[..parent.len()]).unwrap_or("/"); let _ = files_set_path(&mut state.files, parent); }
        }
        FilesHit::Previous => {
            if state.files.page_start > 0 {
                state.files.page_start = state.files.page_start.saturating_sub(FILES_PAGE_SIZE);
                state.files.highlighted = 0;
                files_set_notice(&mut state.files, "PREVIOUS PAGE");
            } else { files_set_notice(&mut state.files, "ALREADY ON FIRST PAGE"); }
        }
        FilesHit::Next => {
            let total = crate::vfs::list_directory(files_path(&state.files), |_, _| {}).unwrap_or(0);
            if state.files.page_start + FILES_PAGE_SIZE < total {
                state.files.page_start += FILES_PAGE_SIZE;
                state.files.highlighted = 0;
                files_set_notice(&mut state.files, "NEXT PAGE");
            } else { files_set_notice(&mut state.files, "ALREADY ON LAST PAGE"); }
        }
        FilesHit::NewDirectory => files_begin_prompt(&mut state.files, 1),
        FilesHit::NewFile => files_begin_prompt(&mut state.files, 2),
        FilesHit::Delete => {
            if state.files.selected_path_len == 0 { files_set_notice(&mut state.files, "SELECT A FILE BEFORE DELETE"); }
            else {
                let path = core::str::from_utf8(&state.files.selected_path[..state.files.selected_path_len]).unwrap_or("");
                if files_is_writable_path(path) && !path.eq_ignore_ascii_case("/DISK") && crate::users::may_access_path(path) {
                    match crate::vfs::remove(path) {
                        Ok(()) => { state.files.selected_path_len = 0; state.files.selected_name_len = 0; state.files.preview_len = 0; files_set_notice(&mut state.files, "FILE DELETED"); }
                        Err(error) => files_set_notice(&mut state.files, error),
                    }
                } else { files_set_notice(&mut state.files, "DELETE IS LIMITED TO /DISK FILES"); }
            }
        }
        FilesHit::Row(index) => { state.files.highlighted = index.saturating_sub(state.files.page_start); files_activate_row(state, index); }
    }
}

fn draw_files_panel(fb: &Framebuffer, state: &DesktopState) {
    let (x, y, win_w, win_h) = app_window_geometry(fb);
    let current_path = files_path(&state.files);
    let active_sidebar = if state.files.favorite_path_len > 0 && current_path == core::str::from_utf8(&state.files.favorite_path[..state.files.favorite_path_len]).unwrap_or("") { 3 }
        else if current_path.eq_ignore_ascii_case("/DISK") || current_path.get(..6).map(|prefix| prefix.eq_ignore_ascii_case("/DISK/")).unwrap_or(false) { 1 }
        else if current_path.eq_ignore_ascii_case("/ETC") || current_path.get(..5).map(|prefix| prefix.eq_ignore_ascii_case("/ETC/")).unwrap_or(false) { 2 }
        else { 0 };
    draw_app_sidebar(fb, x + 12, y + 48, 132, win_h.saturating_sub(60), "FILES", &["HOME", "DISK", "SYSTEM", "FAVORITE"], active_sidebar);
    let layout = files_layout(fb);
    draw_page_heading(fb, layout.content_x, y + 56, "FILES & STORAGE", "Browse folders and manage writable disk files.");
    if state.files.notice_len > 0 {
        let status = core::str::from_utf8(&state.files.notice[..state.files.notice_len]).unwrap_or("");
        let visible = &status[..status.len().min(layout.content_w / 6)];
        crate::draw_text(fb, layout.content_x, y + 80, visible, 1, color(fb, 113, 231, 192));
    }
    rounded_rect(fb, layout.path.x, layout.path.y, layout.path.width, layout.path.height, 7, color(fb, 19, 29, 47));
    crate::draw_text(fb, layout.path.x + 10, layout.path.y + 11, "PATH", 1, color(fb, 103, 222, 255));
    let current_path = files_path(&state.files);
    let max_path = layout.path.width.saturating_sub(66) / 6;
    let visible_path = if current_path.len() > max_path { &current_path[current_path.len().saturating_sub(max_path)..] } else { current_path };
    crate::draw_text(fb, layout.path.x + 52, layout.path.y + 11, visible_path, 1, color(fb, 232, 242, 255));
    let buttons = [
        (layout.back, "BACK", color(fb, 30, 75, 103)),
        (layout.up, "UP", color(fb, 30, 75, 103)),
        (layout.previous, "PREV", color(fb, 28, 43, 65)),
        (layout.next, "NEXT", color(fb, 28, 43, 65)),
        (layout.pin, "PIN", color(fb, 66, 62, 104)),
        (layout.new_directory, "NEW DIR", color(fb, 25, 91, 78)),
        (layout.new_file, "NEW FILE", color(fb, 25, 91, 78)),
        (layout.delete, "DELETE", color(fb, 105, 40, 52)),
    ];
    for (rect, label, shade) in buttons {
        rounded_rect(fb, rect.x, rect.y, rect.width, rect.height, 5, shade);
        let max_chars = rect.width.saturating_sub(8) / 6;
        crate::draw_text(fb, rect.x + 4, rect.y + 11, &label[..label.len().min(max_chars)], 1, color(fb, 237, 246, 255));
    }
    crate::draw_text(fb, layout.content_x + 5, layout.rows.y - 13, "NAME", 1, color(fb, 111, 222, 255));
    crate::draw_text(fb, layout.content_x + layout.content_w.saturating_sub(94), layout.rows.y - 13, "TYPE / SIZE", 1, color(fb, 111, 222, 255));
    let mut rows = [FileRow::empty(); FILES_PAGE_SIZE];
    let mut total = 0usize;
    let list_result = crate::vfs::list_directory(current_path, |name, is_dir| {
        let index = total;
        if index >= state.files.page_start && index < state.files.page_start + FILES_PAGE_SIZE && name.len() <= FILE_NAME_CAPACITY {
            let visible = index - state.files.page_start;
            rows[visible].name[..name.len()].copy_from_slice(name.as_bytes());
            rows[visible].len = name.len();
            rows[visible].is_dir = is_dir;
        }
        total += 1;
    });
    if list_result.is_err() {
        crate::draw_text(fb, layout.content_x + 8, layout.rows.y + 12, "CANNOT READ THIS DIRECTORY.", 1, color(fb, 255, 164, 153));
    } else if total == 0 {
        crate::draw_text(fb, layout.content_x + 8, layout.rows.y + 12, "THIS DIRECTORY IS EMPTY.", 1, color(fb, 177, 194, 220));
    } else {
        for (index, row) in rows.iter().enumerate() {
            if row.len == 0 { continue; }
            let row_y = layout.rows.y + index * 25;
            let name = core::str::from_utf8(&row.name[..row.len]).unwrap_or("?");
            let mut target = [0u8; FILE_PATH_CAPACITY];
            let target_path = files_join_path(current_path, name, &mut target)
                .and_then(|length| core::str::from_utf8(&target[..length]).ok());
            let selected = target_path.map(|p| p.eq_ignore_ascii_case(core::str::from_utf8(&state.files.selected_path[..state.files.selected_path_len]).unwrap_or(""))).unwrap_or(false);
            let highlighted = index == state.files.highlighted;
            let bg = if selected { color(fb, 27, 76, 83) } else if highlighted { color(fb, 25, 48, 73) } else { color(fb, 14, 23, 39) };
            rounded_rect(fb, layout.rows.x, row_y, layout.rows.width, 22, 4, bg);
            draw_app_icon(fb, if row.is_dir { APP_FILES } else { APP_TERMINAL }, layout.content_x + 5, row_y + 1, 19);
            let name_max = layout.content_w.saturating_sub(135) / 6;
            let shown_name = &name[..name.len().min(name_max)];
            crate::draw_text(fb, layout.content_x + 31, row_y + 8, shown_name, 1, color(fb, 231, 241, 254));
            let kind = if row.is_dir { "FOLDER" } else { "FILE" };
            crate::draw_text(fb, layout.content_x + layout.content_w.saturating_sub(55), row_y + 8, kind, 1, color(fb, if row.is_dir { 99 } else { 111 }, if row.is_dir { 222 } else { 228 }, if row.is_dir { 255 } else { 184 }));
        }
    }
    rounded_rect(fb, layout.preview.x, layout.preview.y, layout.preview.width, layout.preview.height, 7, color(fb, 13, 21, 37));
    if state.files.prompt_mode != 0 {
        let label = if state.files.prompt_mode == 1 { "NEW DIRECTORY:" } else { "NEW FILE:" };
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 10, label, 1, color(fb, 101, 226, 255));
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 29, core::str::from_utf8(&state.files.prompt[..state.files.prompt_len]).unwrap_or(""), 1, color(fb, 240, 247, 255));
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 48, "ENTER SAVE  /  ESC CANCEL", 1, color(fb, 147, 173, 207));
    } else if state.files.selected_name_len > 0 {
        let selected_name = core::str::from_utf8(&state.files.selected_name[..state.files.selected_name_len]).unwrap_or("FILE");
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 9, selected_name, 1, color(fb, 104, 230, 189));
        let preview = core::str::from_utf8(&state.files.preview[..state.files.preview_len]).unwrap_or("");
        // The preview is deliberately capped to two rows to keep the file list responsive.
        let first = preview.split('\n').next().unwrap_or("");
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 27, &first[..first.len().min(layout.content_w.saturating_sub(20) / 6)], 1, color(fb, 224, 237, 252));
        let second = preview.split('\n').nth(1).unwrap_or("");
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 45, &second[..second.len().min(layout.content_w.saturating_sub(20) / 6)], 1, color(fb, 165, 188, 219));
    } else {
        let notice = core::str::from_utf8(&state.files.notice[..state.files.notice_len]).unwrap_or("");
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 12, if notice.is_empty() { "SELECT A FILE TO PREVIEW ITS CONTENTS." } else { notice }, 1, color(fb, 222, 234, 249));
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 33, "CREATE / DELETE WORKS INSIDE /DISK ONLY.", 1, color(fb, 139, 165, 201));
        crate::draw_text(fb, layout.preview.x + 10, layout.preview.y + 52, "UP ARROW = PARENT  /  ENTER = OPEN HIGHLIGHTED ITEM", 1, color(fb, 139, 165, 201));
    }
    let page_count = (total + FILES_PAGE_SIZE - 1) / FILES_PAGE_SIZE;
    let current_page = if total == 0 { 0 } else { state.files.page_start / FILES_PAGE_SIZE + 1 };
    let page_text = if total == 0 { "EMPTY" } else if current_page == 1 { "PAGE 1" } else if page_count > 1 { "MORE PAGES" } else { "PAGE 1" };
    crate::draw_text(fb, layout.content_x + layout.content_w.saturating_sub(66), y + 56, page_text, 1, color(fb, 117, 147, 185));
}

fn draw_app_sidebar(fb: &Framebuffer, x: usize, y: usize, width: usize, height: usize, title: &str, items: &[&str], active: usize) {
    rounded_rect(fb, x, y, width, height, 9, color(fb, 11, 18, 34));
    crate::draw_text(fb, x + 12, y + 13, title, 1, color(fb, 119, 148, 187));
    for (index, item) in items.iter().enumerate() {
        let item_y = y + 35 + index * 36;
        if item_y + 28 >= y + height { break; }
        if index == active {
            rounded_rect(fb, x + 7, item_y, width.saturating_sub(14), 29, 7, color(fb, 28, 88, 119));
            crate::fill_rect(fb, x + 7, item_y + 5, 2, 19, color(fb, 102, 228, 255));
        }
        crate::draw_text(fb, x + 17, item_y + 11, item, 1, color(fb, if index == active { 239 } else { 167 }, if index == active { 247 } else { 190 }, if index == active { 255 } else { 220 }));
    }
    let footer_y = y + height.saturating_sub(30);
    crate::fill_rect(fb, x + 12, footer_y, width.saturating_sub(24), 1, color(fb, 34, 48, 70));
    crate::draw_text(fb, x + 12, footer_y + 10, "XAVIEROS", 1, color(fb, 100, 216, 245));
}

fn draw_stat_card(fb: &Framebuffer, x: usize, y: usize, width: usize, height: usize, label: &str, value: &str, accent: u32) {
    if width < 24 || height < 32 { return; }
    rounded_rect(fb, x, y, width, height, 8, color(fb, 19, 29, 47));
    crate::fill_rect(fb, x + 10, y + 9, 3, height.saturating_sub(18).min(28), accent);
    crate::draw_text(fb, x + 20, y + 10, label, 1, color(fb, 125, 151, 188));
    let max_chars = width.saturating_sub(29) / 6;
    let visible = &value[..value.len().min(max_chars)];
    crate::draw_text(fb, x + 20, y + 30, visible, 1, color(fb, 231, 241, 254));
}

fn draw_content_row(
    fb: &Framebuffer,
    x: usize,
    y: usize,
    width: usize,
    height: usize,
    icon_app: usize,
    label: &str,
    kind: &str,
    accent: u32,
) {
    if width < 72 || height < 24 { return; }
    rounded_rect(fb, x, y, width, height, 6, color(fb, 17, 27, 44));
    draw_app_icon(fb, icon_app, x + 5, y + 3, height.saturating_sub(6).min(25));
    let max_label_chars = width.saturating_sub(116) / 6;
    crate::draw_text(fb, x + 38, y + 7, &label[..label.len().min(max_label_chars)], 1, color(fb, 226, 237, 252));
    let kind_width = kind.len() * 6;
    crate::draw_text(fb, x + width.saturating_sub(kind_width + 10), y + 11, kind, 1, accent);
}

fn app_window_geometry(fb: &Framebuffer) -> (usize, usize, usize, usize) {
    let width = fb.width as usize;
    let height = fb.height as usize;
    let win_w = width.saturating_sub(54).min(700).max(250);
    let win_h = height.saturating_sub(230).min(420).max(180);
    let x = width.saturating_sub(win_w) / 2;
    let y = 74 + height.saturating_sub(74 + win_h + 104) / 2;
    (x, y, win_w, win_h)
}

fn settings_layout(fb: &Framebuffer) -> SettingsLayout {
    let (x, y, win_w, win_h) = app_window_geometry(fb);
    let content_x = x + 160;
    let content_width = win_w.saturating_sub(176);
    let card_gap = 8;
    let card_width = content_width.saturating_sub(card_gap * 2) / 3;
    let card_y = y + 124;
    let card_height = 78;
    SettingsLayout {
        sidebar: Rect {
            x: x + 12,
            y: y + 48,
            width: 132,
            height: win_h.saturating_sub(61),
        },
        cards: [
            Rect { x: content_x, y: card_y, width: card_width, height: card_height },
            Rect { x: content_x + card_width + card_gap, y: card_y, width: card_width, height: card_height },
            Rect { x: content_x + (card_width + card_gap) * 2, y: card_y, width: card_width, height: card_height },
        ],
        apply: Rect { x: content_x, y: card_y + 89, width: 112, height: 34 },
        current: Rect { x: content_x + 122, y: card_y + 89, width: 104, height: 34 },
    }
}

fn draw_settings_panel(fb: &Framebuffer, state: &DesktopState) {
    let layout = settings_layout(fb);
    let (_, window_y, _, _) = app_window_geometry(fb);
    let content_x = layout.cards[0].x;
    let current_index = display_mode_index(fb.width as usize, fb.height as usize);
    let settings = config::current();

    // A compact, calm sidebar keeps the main display controls visually grouped.
    rounded_rect(fb, layout.sidebar.x, layout.sidebar.y, layout.sidebar.width, layout.sidebar.height, 9, color(fb, 11, 18, 34));
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 14, "PREFERENCES", 1, color(fb, 118, 147, 186));
    rounded_rect(fb, layout.sidebar.x + 7, layout.sidebar.y + 38, layout.sidebar.width - 14, 34, 7, color(fb, 28, 91, 124));
    rounded_rect(fb, layout.sidebar.x + 17, layout.sidebar.y + 48, 16, 13, 3, color(fb, 116, 232, 255));
    crate::fill_rect(fb, layout.sidebar.x + 20, layout.sidebar.y + 51, 10, 7, color(fb, 22, 49, 76));
    crate::draw_text(fb, layout.sidebar.x + 40, layout.sidebar.y + 50, "DISPLAY", 1, color(fb, 240, 249, 255));
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 91, "ACTIVE MODE", 1, color(fb, 118, 147, 186));
    let mut resolution_buffer = [0u8; 24];
    let current_resolution = format_resolution(&mut resolution_buffer, fb.width, fb.height);
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 108, current_resolution, 1, color(fb, 231, 242, 255));
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 139, "COLOR", 1, color(fb, 118, 147, 186));
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 155, "32-BIT RGB", 1, color(fb, 192, 211, 237));
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 192, "NETWORK", 1, color(fb, 118, 147, 186));
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 208, if settings.use_dhcp { "DHCP" } else { "STATIC" }, 1, color(fb, 192, 211, 237));
    let timezone = format_timezone(settings.timezone_minutes);
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 235, "TIMEZONE", 1, color(fb, 118, 147, 186));
    crate::draw_text(fb, layout.sidebar.x + 12, layout.sidebar.y + 251, bytes_text(&timezone), 1, color(fb, 192, 211, 237));

    crate::draw_text(fb, content_x, window_y + 57, "DISPLAY & RESOLUTION", 2, color(fb, 238, 246, 255));
    crate::draw_text(fb, content_x, window_y + 81, "Choose a mode for the virtual display.", 1, color(fb, 154, 177, 210));
    crate::draw_text(fb, content_x, window_y + 101, "AVAILABLE MODES", 1, color(fb, 103, 222, 255));

    for index in 0..DISPLAY_MODES.len() {
        let mode = DISPLAY_MODES[index];
        let rect = layout.cards[index];
        let is_selected = state.selected_resolution == index;
        let is_current = current_index == index;
        let card_color = if is_selected { color(fb, 28, 83, 111) } else { color(fb, 21, 31, 51) };
        rounded_rect(fb, rect.x, rect.y, rect.width, rect.height, 8, card_color);
        if is_selected {
            crate::fill_rect(fb, rect.x + 12, rect.y + rect.height - 4, rect.width.saturating_sub(24), 2, color(fb, 87, 225, 255));
        } else {
            crate::fill_rect(fb, rect.x + 12, rect.y + rect.height - 3, rect.width.saturating_sub(24), 1, color(fb, 47, 61, 83));
        }
        let resolution_label = match index {
            0 => "1024 x 768",
            1 => "1280 x 720",
            _ => "1366 x 768",
        };
        let title_x = rect.x + rect.width.saturating_sub(resolution_label.len() * 6) / 2;
        crate::draw_text(fb, title_x, rect.y + 13, resolution_label, 1, color(fb, 240, 247, 255));
        let aspect_x = rect.x + rect.width.saturating_sub(mode.aspect.len() * 6) / 2;
        crate::draw_text(fb, aspect_x, rect.y + 34, mode.aspect, 1, color(fb, 159, 186, 218));
        let badge = if is_current { "CURRENT" } else if is_selected { "SELECTED" } else { mode.quality };
        let badge_x = rect.x + rect.width.saturating_sub(badge.len() * 6) / 2;
        crate::draw_text(fb, badge_x, rect.y + 55, badge, 1, if is_current { color(fb, 107, 235, 182) } else { color(fb, 108, 219, 255) });
    }

    rounded_rect(fb, layout.apply.x, layout.apply.y, layout.apply.width, layout.apply.height, 7, color(fb, 31, 147, 184));
    crate::draw_text(fb, layout.apply.x + 19, layout.apply.y + 13, "APPLY MODE", 1, color(fb, 248, 253, 255));
    rounded_rect(fb, layout.current.x, layout.current.y, layout.current.width, layout.current.height, 7, color(fb, 34, 46, 67));
    crate::draw_text(fb, layout.current.x + 19, layout.current.y + 13, "USE CURRENT", 1, color(fb, 202, 218, 240));

    let notice_text = match state.display_notice {
        DisplayNotice::None => "Select a resolution, then apply it.",
        DisplayNotice::Selected => "New mode selected. Apply to switch the display.",
        DisplayNotice::Applied => "Display resolution changed successfully.",
        DisplayNotice::Unsupported => "Live switching is unavailable in this video setup.",
        DisplayNotice::NotEnoughMemory => "This mode needs more video memory than is available.",
        DisplayNotice::Rejected => "The video adapter rejected this mode; previous mode restored.",
    };
    let notice_color = match state.display_notice {
        DisplayNotice::Applied => color(fb, 104, 235, 182),
        DisplayNotice::Unsupported | DisplayNotice::NotEnoughMemory | DisplayNotice::Rejected => color(fb, 255, 169, 155),
        _ => color(fb, 154, 177, 210),
    };
    crate::draw_text(fb, content_x, layout.apply.y + 48, notice_text, 1, notice_color);
    crate::draw_text(fb, content_x, layout.apply.y + 68, "CHANGES APPLY TO THIS QEMU SESSION.", 1, color(fb, 113, 142, 181));
}

fn settings_hit_test(fb: &Framebuffer, x: i32, y: i32) -> Option<SettingsHit> {
    let layout = settings_layout(fb);
    for index in 0..layout.cards.len() {
        if layout.cards[index].contains(x, y) {
            return Some(SettingsHit::SelectMode(index));
        }
    }
    if layout.apply.contains(x, y) { return Some(SettingsHit::Apply); }
    if layout.current.contains(x, y) { return Some(SettingsHit::UseCurrent); }
    None
}

fn display_mode_index(width: usize, height: usize) -> usize {
    DISPLAY_MODES.iter().position(|mode| mode.width == width && mode.height == height).unwrap_or(0)
}

fn apply_selected_resolution(
    framebuffer: &mut Framebuffer,
    state: &mut DesktopState,
    current_width: &mut usize,
    current_height: &mut usize,
    memory_summary: Option<(MemoryStats, bool)>,
) -> bool {
    let mode = DISPLAY_MODES[state.selected_resolution.min(DISPLAY_MODES.len() - 1)];
    if framebuffer.width as usize == mode.width && framebuffer.height as usize == mode.height {
        state.display_notice = DisplayNotice::Applied;
        return false;
    }

    let hhdm_offset = memory_summary.map(|(stats, _)| stats.hhdm_offset);
    match set_qemu_vbe_mode(
        mode,
        *current_width,
        *current_height,
        framebuffer.address,
        hhdm_offset,
    ) {
        Ok(mapped_framebuffer) => {
            framebuffer.address = mapped_framebuffer;
            framebuffer.width = mode.width as u64;
            framebuffer.height = mode.height as u64;
            framebuffer.pitch = (mode.width * 4) as u64;
            *current_width = mode.width;
            *current_height = mode.height;
            state.mouse_x = state.mouse_x.clamp(1, (*current_width).saturating_sub(2) as i32);
            state.mouse_y = state.mouse_y.clamp(1, (*current_height).saturating_sub(2) as i32);
            state.display_notice = DisplayNotice::Applied;
            crate::serial_write("Display mode changed through QEMU Bochs VBE.\r\n");
            true
        }
        Err(notice) => {
            state.display_notice = notice;
            crate::serial_write("Display mode switch failed; see the Settings panel for details.\r\n");
            false
        }
    }
}

const VBE_INDEX_PORT: u16 = 0x01CE;
const VBE_DATA_PORT: u16 = 0x01CF;
const VBE_INDEX_ID: u16 = 0;
const VBE_INDEX_XRES: u16 = 1;
const VBE_INDEX_YRES: u16 = 2;
const VBE_INDEX_BPP: u16 = 3;
const VBE_INDEX_ENABLE: u16 = 4;
const VBE_INDEX_VIRT_WIDTH: u16 = 6;
const VBE_INDEX_VIRT_HEIGHT: u16 = 7;
const VBE_INDEX_X_OFFSET: u16 = 8;
const VBE_INDEX_Y_OFFSET: u16 = 9;
const VBE_INDEX_VIDEO_MEMORY_64K: u16 = 10;
const VBE_ENABLE: u16 = 0x0041;
const VBE_ID_MIN: u16 = 0xB0C0;
const VBE_ID_MAX: u16 = 0xB0C5;

fn set_qemu_vbe_mode(
    mode: DisplayMode,
    old_width: usize,
    old_height: usize,
    framebuffer_address: *mut u8,
    hhdm_offset: Option<u64>,
) -> Result<*mut u8, DisplayNotice> {
    let id = vbe_read_register(VBE_INDEX_ID);
    if !(VBE_ID_MIN..=VBE_ID_MAX).contains(&id) {
        return Err(DisplayNotice::Unsupported);
    }

    let video_memory_64k = vbe_read_register(VBE_INDEX_VIDEO_MEMORY_64K) as usize;
    let required_bytes = (mode.width * mode.height * 4) as u64;
    let required_64k = ((required_bytes + 65_535) / 65_536) as usize;
    if video_memory_64k != 0 && required_64k > video_memory_64k {
        return Err(DisplayNotice::NotEnoughMemory);
    }

    // Extend the HHDM mapping across the adapter's linear framebuffer before
    // allowing a larger resolution to write more pixels than Limine initially mapped.
    let offset = hhdm_offset.ok_or(DisplayNotice::Unsupported)?;
    let address = framebuffer_address as u64;
    let physical_base = address.checked_sub(offset).ok_or(DisplayNotice::Unsupported)?;
    // In this QEMU-standard-VGA path, the linear framebuffer must be in the
    // conventional high PCI MMIO window. Reject a non-HHDM or unexpected alias
    // rather than accidentally mapping an unrelated physical range.
    if !(0xC000_0000..4 * 1024 * 1024 * 1024).contains(&physical_base) {
        return Err(DisplayNotice::Unsupported);
    }
    // Map only the aperture bytes needed by the selected mode, not the adapter's
    // entire advertised VRAM. Some emulated devices report a large VRAM capacity
    // even though the framebuffer BAR itself is a smaller window.
    crate::memory::map_mmio_region(physical_base, required_bytes)
        .map_err(|_| DisplayNotice::Unsupported)?;
    let mapped_address = offset.checked_add(physical_base).ok_or(DisplayNotice::Unsupported)? as *mut u8;

    if vbe_program_mode(mode.width, mode.height) {
        Ok(mapped_address)
    } else {
        // Avoid leaving the display in an unrecognized mode if the adapter
        // advertises VBE but refuses a requested resolution.
        let _ = vbe_program_mode(old_width, old_height);
        Err(DisplayNotice::Rejected)
    }
}

fn vbe_program_mode(width: usize, height: usize) -> bool {
    unsafe {
        vbe_write_register(VBE_INDEX_ENABLE, 0);
        vbe_write_register(VBE_INDEX_XRES, width as u16);
        vbe_write_register(VBE_INDEX_YRES, height as u16);
        vbe_write_register(VBE_INDEX_BPP, 32);
        vbe_write_register(VBE_INDEX_VIRT_WIDTH, width as u16);
        vbe_write_register(VBE_INDEX_VIRT_HEIGHT, height as u16);
        vbe_write_register(VBE_INDEX_X_OFFSET, 0);
        vbe_write_register(VBE_INDEX_Y_OFFSET, 0);
        vbe_write_register(VBE_INDEX_ENABLE, VBE_ENABLE);
    }
    vbe_read_register(VBE_INDEX_XRES) as usize == width
        && vbe_read_register(VBE_INDEX_YRES) as usize == height
        && vbe_read_register(VBE_INDEX_BPP) == 32
        && vbe_read_register(VBE_INDEX_VIRT_WIDTH) as usize == width
}

fn vbe_read_register(index: u16) -> u16 {
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") VBE_INDEX_PORT, in("ax") index, options(nomem, nostack, preserves_flags));
        let value: u16;
        core::arch::asm!("in ax, dx", in("dx") VBE_DATA_PORT, out("ax") value, options(nomem, nostack, preserves_flags));
        value
    }
}

fn vbe_write_register(index: u16, value: u16) {
    // Port I/O is privileged; this helper is only called by the ring-0 kernel
    // after confirming that the QEMU Bochs VBE device advertises its ID.
    unsafe {
        core::arch::asm!("out dx, ax", in("dx") VBE_INDEX_PORT, in("ax") index, options(nomem, nostack, preserves_flags));
        core::arch::asm!("out dx, ax", in("dx") VBE_DATA_PORT, in("ax") value, options(nomem, nostack, preserves_flags));
    }
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

pub(super) fn close_button_hit(fb: &Framebuffer, x: i32, y: i32) -> bool {
    if x < 0 || y < 0 { return false; }
    let (left, top, _win_w, _win_h) = app_window_geometry(fb);
    let x = x as usize;
    let y = y as usize;
    x >= left + 7 && x <= left + 29 && y >= top + 7 && y <= top + 31
}

pub(super) fn draw_mouse_cursor_saved(fb: &Framebuffer, x: i32, y: i32, background: &mut CursorBackground) {
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

pub(super) fn restore_cursor_background(fb: &Framebuffer, background: &mut CursorBackground) {
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
