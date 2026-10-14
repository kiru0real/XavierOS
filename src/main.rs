#![no_std]
#![no_main]

mod memory;
mod interrupts;
mod heap;
mod fs;
mod ata;
mod diskfs;
mod vfs;
mod task;
mod crypto;
mod users;

use memory::MemoryStats;
use core::arch::asm;
use core::panic::PanicInfo;
use core::ptr::{addr_of, read_volatile, write_volatile};
use core::sync::atomic::{AtomicU8, Ordering};

const COM1: u16 = 0x3F8;
static SHIFT_STATE: AtomicU8 = AtomicU8::new(0);
static EXTENDED_SCANCODE: AtomicU8 = AtomicU8::new(0);
const LIMINE_COMMON_MAGIC: [u64; 2] = [
    0xC7B1_DD30_DF4C_8B88,
    0x0A82_E883_A194_F07B,
];

// Ask Limine to negotiate its base protocol revision.
#[used]
#[unsafe(link_section = ".limine_requests")]
static mut LIMINE_BASE_REVISION: [u64; 3] = [
    0xF956_2B2D_5C95_A6C8,
    0x6A7B_3849_4453_6BDC,
    3,
];

#[used]
#[unsafe(link_section = ".limine_requests_start")]
static LIMINE_REQUESTS_START: [u64; 4] = [
    0xF6B8_F4B3_9DE7_D1AE,
    0xFAB9_1A69_40FC_B9CF,
    0x785C_6ED0_15D3_E316,
    0x181E_920A_7852_B9D9,
];

// Limine framebuffer protocol structures (C ABI, as defined by limine-protocol).
#[repr(C)]
struct FramebufferResponse {
    revision: u64,
    framebuffer_count: u64,
    framebuffers: *mut *mut Framebuffer,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Framebuffer {
    address: *mut u8,
    width: u64,
    height: u64,
    pitch: u64,
    bpp: u16,
    memory_model: u8,
    red_mask_size: u8,
    red_mask_shift: u8,
    green_mask_size: u8,
    green_mask_shift: u8,
    blue_mask_size: u8,
    blue_mask_shift: u8,
    unused: [u8; 7],
    edid_size: u64,
    edid: *mut u8,
    mode_count: u64,
    modes: *mut *mut u8,
}

#[repr(C)]
struct FramebufferRequest {
    magic: [u64; 2],
    id: [u64; 2],
    revision: u64,
    response: *mut FramebufferResponse,
}

#[used]
#[unsafe(link_section = ".limine_requests")]
static mut FRAMEBUFFER_REQUEST: FramebufferRequest = FramebufferRequest {
    magic: LIMINE_COMMON_MAGIC,
    id: [0x9D58_27DC_D881_DD75, 0xA314_8604_F6FA_B11B],
    revision: 0,
    response: core::ptr::null_mut(),
};

#[used]
#[unsafe(link_section = ".limine_requests_end")]
static LIMINE_REQUESTS_END: [u64; 2] = [
    0xADC0_E053_1BB1_0D03,
    0x9572_709F_3176_4C62,
];

#[unsafe(no_mangle)]
pub extern "C" fn _start() -> ! {
    unsafe { asm!("cli", options(nomem, nostack)); }
    serial_init();
    interrupts::initialize();
    interrupts::initialize_hardware();
    serial_write("\r\nXavierOS v0.1.2\r\n");
    serial_write("Rust x86_64 kernel entered.\r\n");
    serial_write("Requesting Limine framebuffer...\r\n");

    let response = unsafe { read_volatile(addr_of!(FRAMEBUFFER_REQUEST.response)) };
    if response.is_null() {
        serial_write("ERROR: Limine returned no framebuffer response.\r\n");
        halt_forever();
    }

    let count = unsafe { read_volatile(addr_of!((*response).framebuffer_count)) };
    let list = unsafe { read_volatile(addr_of!((*response).framebuffers)) };
    if count == 0 || list.is_null() {
        serial_write("ERROR: Limine framebuffer list is empty.\r\n");
        halt_forever();
    }

    let framebuffer_ptr = unsafe { read_volatile(list) };
    if framebuffer_ptr.is_null() {
        serial_write("ERROR: First framebuffer pointer is null.\r\n");
        halt_forever();
    }

    let framebuffer = unsafe { read_volatile(framebuffer_ptr) };
    serial_write("Limine framebuffer response received.\r\n");

    serial_write("Reading Limine memory map...\r\n");
    let memory_summary: Option<(MemoryStats, bool)> = match memory::initialize() {
        Ok(stats) => {
            serial_write("Memory-map entries: ");
            serial_write_u64(stats.entry_count as u64);
            serial_write("\r\nUsable RAM reported by Limine: ");
            serial_write_u64(stats.total_usable_bytes / (1024 * 1024));
            serial_write(" MiB\r\nHHDM direct-map offset: 0x");
            serial_write_u64_hex(stats.hhdm_offset);
            serial_write("\r\nHHDM direct map: READY\r\nPMM-managed pages below 4 GiB: ");
            serial_write_u64(stats.managed_pages as u64);
            serial_write("\r\nPMM free pages: ");
            serial_write_u64(memory::free_page_count() as u64);
            serial_write("\r\n");

            match memory::self_test() {
                Ok(()) => {
                    serial_write("PMM self-test: PASS (allocate/free/double-free guard).\r\n");
                    serial_write("HHDM test: PASS (zero page + read/write verification).\r\n");
                    Some((stats, true))
                }
                Err(error) => {
                    serial_write("PMM self-test: FAIL: ");
                    serial_write(error);
                    serial_write("\r\n");
                    Some((stats, false))
                }
            }
        }
        Err(error) => {
            serial_write("ERROR: Memory manager initialization failed: ");
            serial_write(error);
            serial_write("\r\n");
            None
        }
    };

    match memory_summary {
        Some((_, true)) => match heap::initialize() {
            Ok(stats) => {
                serial_write("Kernel heap self-test will be available in the shell. Capacity: ");
                serial_write_u64(stats.capacity_bytes as u64);
                serial_write(" bytes; free: ");
                serial_write_u64(stats.free_bytes as u64);
                serial_write(" bytes.\r\n");
            }
            Err(error) => {
                serial_write("WARNING: Kernel heap initialization failed: ");
                serial_write(error);
                serial_write("\r\n");
            }
        },
        Some((_, false)) => serial_write("WARNING: Heap initialization skipped because PMM self-test failed.\r\n"),
        None => serial_write("WARNING: Heap initialization skipped because memory manager is unavailable.\r\n"),
    }

    match diskfs::initialize() {
        Ok(info) => {
            serial_write("Writable XVDISKFS mounted. ATA sectors: ");
            serial_write_u64(info.total_sectors as u64);
            serial_write("; files: ");
            serial_write_u64(info.files as u64);
            serial_write("; directories: ");
            serial_write_u64(info.directories as u64);
            serial_write(".\r\n");
        }
        Err(error) => {
            serial_write("Writable disk filesystem unavailable: ");
            serial_write(error);
            serial_write("\r\n");
        }
    }
    match users::initialize() {
        Ok(()) => {
            serial_write("Local user database ready; initial console identity is live.\r\n");
            serial_write("User account access is experimental and does not isolate kernel privileges.\r\n");
        }
        Err(error) => {
            serial_write("User database unavailable: ");
            serial_write(error);
            serial_write("; continuing in live session.\r\n");
        }
    }

    if !framebuffer_is_supported(&framebuffer) {
        serial_write("ERROR: Unsupported framebuffer format or dimensions.\r\n");
        halt_forever();
    }

    serial_write("Framebuffer ready. Starting interactive console...\r\n");
    run_console(&framebuffer, memory_summary)
}

fn halt_forever() -> ! {
    loop {
        unsafe { asm!("cli; hlt", options(nomem, nostack)); }
    }
}


const CONSOLE_MAX_COLS: usize = 120;
const CONSOLE_MAX_LINES: usize = 64;
const CONSOLE_INPUT_CAPACITY: usize = 128;
const CONSOLE_TEXT_X: usize = 40;
const CONSOLE_TEXT_Y: usize = 116;
const CONSOLE_FOOTER_HEIGHT: usize = 42;

#[derive(Clone, Copy)]
struct ConsoleLine {
    bytes: [u8; CONSOLE_MAX_COLS],
    len: usize,
}

impl ConsoleLine {
    const fn empty() -> Self {
        Self { bytes: [b' '; CONSOLE_MAX_COLS], len: 0 }
    }
}

struct Console {
    framebuffer: Framebuffer,
    lines: [ConsoleLine; CONSOLE_MAX_LINES],
    line_count: usize,
    scale: usize,
    line_height: usize,
    max_cols: usize,
}

impl Console {
    fn new(framebuffer: Framebuffer) -> Self {
        let scale = if framebuffer.width >= 640 { 2 } else { 1 };
        let max_cols = (framebuffer.width as usize)
            .saturating_sub(80)
            .checked_div(6 * scale)
            .unwrap_or(1)
            .max(20)
            .min(CONSOLE_MAX_COLS);
        Self {
            framebuffer,
            lines: [ConsoleLine::empty(); CONSOLE_MAX_LINES],
            line_count: 1,
            scale,
            line_height: 7 * scale + 4,
            max_cols,
        }
    }

    fn clear(&mut self) {
        self.lines = [ConsoleLine::empty(); CONSOLE_MAX_LINES];
        self.line_count = 1;
    }

    fn write(&mut self, text: &str) {
        serial_write(text);
        for byte in text.bytes() {
            match byte {
                b'\r' => {}
                b'\n' => self.newline(),
                b'\t' => {
                    for _ in 0..4 { self.put_byte(b' '); }
                }
                32..=126 => self.put_byte(byte.to_ascii_uppercase()),
                _ => {}
            }
        }
    }

    fn write_line(&mut self, text: &str) {
        self.write(text);
        self.newline();
    }

    fn write_decimal(&mut self, value: u64) {
        let mut buffer = [0u8; 20];
        self.write(decimal_text(value, &mut buffer));
    }

    fn write_hex(&mut self, value: u64) {
        const HEX: &[u8; 16] = b"0123456789ABCDEF";
        let mut buffer = [b'0'; 16];
        for (index, byte) in buffer.iter_mut().enumerate() {
            let shift = 60 - index * 4;
            *byte = HEX[((value >> shift) & 0xF) as usize];
        }
        self.write(core::str::from_utf8(&buffer).unwrap_or("0000000000000000"));
    }

    fn put_byte(&mut self, byte: u8) {
        let index = self.line_count - 1;
        if self.lines[index].len >= self.max_cols {
            self.newline();
        }
        let index = self.line_count - 1;
        let column = self.lines[index].len;
        if column < CONSOLE_MAX_COLS {
            self.lines[index].bytes[column] = byte;
            self.lines[index].len += 1;
        }
    }

    fn backspace(&mut self) {
        let index = self.line_count - 1;
        if self.lines[index].len > 0 {
            self.lines[index].len -= 1;
            let column = self.lines[index].len;
            self.lines[index].bytes[column] = b' ';
        }
    }

    fn newline(&mut self) {
        if self.line_count < CONSOLE_MAX_LINES {
            self.lines[self.line_count] = ConsoleLine::empty();
            self.line_count += 1;
        } else {
            self.lines.copy_within(1..CONSOLE_MAX_LINES, 0);
            self.lines[CONSOLE_MAX_LINES - 1] = ConsoleLine::empty();
        }
        serial_write("\r\n");
    }

    fn input_limit(&self, prompt_len: usize) -> usize {
        self.max_cols.saturating_sub(prompt_len + 1).min(CONSOLE_INPUT_CAPACITY)
    }

    fn visible_rows(&self) -> usize {
        let available = (self.framebuffer.height as usize)
            .saturating_sub(CONSOLE_FOOTER_HEIGHT + CONSOLE_TEXT_Y + 10);
        (available / self.line_height).max(1)
    }

    fn draw_chrome(&self) {
        let fb = &self.framebuffer;
        let width = fb.width as usize;
        let height = fb.height as usize;
        let background = rgb(fb, 10, 15, 29);
        let header = rgb(fb, 19, 28, 49);
        let panel = rgb(fb, 12, 18, 32);
        let border = rgb(fb, 49, 66, 93);
        let cyan = rgb(fb, 68, 205, 255);

        fill_rect(fb, 0, 0, width, height, background);
        fill_rect(fb, 0, 0, width, 82, header);
        fill_rect(fb, 0, 80, width, 4, cyan);
        draw_text(fb, 32, 20, "XAVIEROS", 3, rgb(fb, 89, 218, 255));
        draw_text(fb, 230, 30, "INTERACTIVE CONSOLE", 2, rgb(fb, 225, 234, 249));
        fill_rect(fb, 32, 96, width.saturating_sub(64), 2, border);

        let panel_bottom = height.saturating_sub(CONSOLE_FOOTER_HEIGHT + 6);
        fill_rect(
            fb,
            24,
            104,
            width.saturating_sub(48),
            panel_bottom.saturating_sub(104),
            panel,
        );
        fill_rect(fb, 24, 104, 3, panel_bottom.saturating_sub(104), rgb(fb, 95, 225, 170));

        let footer_y = height.saturating_sub(CONSOLE_FOOTER_HEIGHT);
        fill_rect(fb, 0, footer_y, width, CONSOLE_FOOTER_HEIGHT, header);
        fill_rect(fb, 0, footer_y, width, 2, border);
        draw_text(
            fb,
            32,
            height.saturating_sub(27),
            "XAVIEROS // BUILD 0.1.2 // INTERACTIVE CONSOLE",
            if width >= 640 { 2 } else { 1 },
            rgb(fb, 151, 169, 196),
        );
    }

    fn render(&self) {
        self.draw_chrome();
        let visible_rows = self.visible_rows();
        let first_line = self.line_count.saturating_sub(visible_rows);
        for (screen_row, line_index) in (first_line..self.line_count).enumerate() {
            let line = &self.lines[line_index];
            let len = line.len.min(self.max_cols);
            let text = core::str::from_utf8(&line.bytes[..len]).unwrap_or("");
            let color = if line_index + 1 == self.line_count {
                rgb(&self.framebuffer, 104, 235, 174)
            } else {
                rgb(&self.framebuffer, 225, 234, 249)
            };
            let y = CONSOLE_TEXT_Y + screen_row * self.line_height;
            if is_x_logo_line(text.as_bytes()) {
                let logo_color = rgb(&self.framebuffer, 89, 218, 255);
                let logo = core::str::from_utf8(&text.as_bytes()[..18]).unwrap_or("");
                draw_text(&self.framebuffer, CONSOLE_TEXT_X, y, logo, self.scale, logo_color);
                let rest = core::str::from_utf8(&text.as_bytes()[18..]).unwrap_or("");
                draw_text(
                    &self.framebuffer,
                    CONSOLE_TEXT_X + 18 * 6 * self.scale,
                    y,
                    rest,
                    self.scale,
                    color,
                );
            } else {
                draw_text(
                    &self.framebuffer,
                    CONSOLE_TEXT_X,
                    y,
                    text,
                    self.scale,
                    color,
                );
            }
        }
    }

    fn current_line_len(&self) -> usize {
        self.lines[self.line_count - 1].len
    }

    fn replace_current_input(&mut self, start: usize, input: &[u8]) {
        let line_index = self.line_count - 1;
        if start > self.max_cols || start > CONSOLE_MAX_COLS {
            return;
        }
        let available = self.max_cols.saturating_sub(start).min(CONSOLE_MAX_COLS - start);
        let length = input.len().min(available);
        self.lines[line_index].bytes[start..].fill(b' ');
        self.lines[line_index].bytes[start..start + length].copy_from_slice(&input[..length]);
        self.lines[line_index].len = start + length;
        self.render_current_line();
    }

    fn render_current_line(&self) {
        let visible_rows = self.visible_rows();
        let first_line = self.line_count.saturating_sub(visible_rows);
        let index = self.line_count - 1;
        let screen_row = index.saturating_sub(first_line);
        let fb = &self.framebuffer;
        let y = CONSOLE_TEXT_Y + screen_row * self.line_height;
        fill_rect(
            fb,
            32,
            y,
            (fb.width as usize).saturating_sub(64),
            self.line_height,
            rgb(fb, 12, 18, 32),
        );
        let line = &self.lines[index];
        let len = line.len.min(self.max_cols);
        let text = core::str::from_utf8(&line.bytes[..len]).unwrap_or("");
        draw_text(
            fb,
            CONSOLE_TEXT_X,
            y,
            text,
            self.scale,
            rgb(fb, 104, 235, 174),
        );
    }
}

fn is_x_logo_line(bytes: &[u8]) -> bool {
    if bytes.len() < 18 {
        return false;
    }
    let prefix = &bytes[..18];
    prefix.iter().all(|byte| *byte == b' ' || *byte == b'X')
        && prefix.iter().filter(|byte| **byte == b'X').count() >= 4
}

#[derive(Clone, Copy)]
enum KeyboardKey {
    Character(u8),
    Enter,
    Backspace,
    HistoryUp,
    HistoryDown,
    Tab,
}

const COMMAND_HISTORY_CAPACITY: usize = 16;

#[derive(Clone, Copy)]
struct HistoryEntry {
    bytes: [u8; CONSOLE_INPUT_CAPACITY],
    len: usize,
}

impl HistoryEntry {
    const EMPTY: Self = Self { bytes: [0; CONSOLE_INPUT_CAPACITY], len: 0 };
}

struct CommandHistory {
    entries: [HistoryEntry; COMMAND_HISTORY_CAPACITY],
    next: usize,
    count: usize,
}

impl CommandHistory {
    const fn new() -> Self {
        Self { entries: [HistoryEntry::EMPTY; COMMAND_HISTORY_CAPACITY], next: 0, count: 0 }
    }

    fn push(&mut self, command: &[u8]) {
        if command.is_empty() || command.iter().all(|byte| byte.is_ascii_whitespace()) { return; }
        if self.count > 0 {
            let previous = (self.next + COMMAND_HISTORY_CAPACITY - 1) % COMMAND_HISTORY_CAPACITY;
            let entry = &self.entries[previous];
            if entry.len == command.len() && entry.bytes[..entry.len].iter().copied().eq(command.iter().copied()) {
                return;
            }
        }
        let length = command.len().min(CONSOLE_INPUT_CAPACITY);
        let entry = &mut self.entries[self.next];
        entry.bytes.fill(0);
        entry.bytes[..length].copy_from_slice(&command[..length]);
        entry.len = length;
        self.next = (self.next + 1) % COMMAND_HISTORY_CAPACITY;
        self.count = (self.count + 1).min(COMMAND_HISTORY_CAPACITY);
    }

    fn copy_back(&self, offset: usize, output: &mut [u8; CONSOLE_INPUT_CAPACITY]) -> Option<usize> {
        if offset >= self.count { return None; }
        let index = (self.next + COMMAND_HISTORY_CAPACITY - 1 - offset) % COMMAND_HISTORY_CAPACITY;
        let entry = &self.entries[index];
        output[..entry.len].copy_from_slice(&entry.bytes[..entry.len]);
        Some(entry.len)
    }
}

fn framebuffer_is_supported(fb: &Framebuffer) -> bool {
    !fb.address.is_null()
        && fb.width >= 320
        && fb.height >= 240
        && fb.bpp == 32
        && fb.memory_model == 1 // LIMINE_FRAMEBUFFER_RGB
        && fb.pitch >= fb.width.saturating_mul(4)
}

fn run_console(framebuffer: &Framebuffer, memory_summary: Option<(MemoryStats, bool)>) -> ! {
    if !framebuffer_is_supported(framebuffer) {
        serial_write("ERROR: Unsupported framebuffer format or dimensions.\r\n");
        halt_forever();
    }

    let mut console = Console::new(*framebuffer);
    console.clear();
    console.write_line("XAVIEROS INTERACTIVE SHELL V0.1.2");
    console.write_line("RUST NO_STD KERNEL | LIMINE | X86_64");
    console.write_line("INITRAMFS: XVRFS V1 | WRITABLE DISK: /DISK");
    console.write_line("TYPE HELP TO SEE AVAILABLE COMMANDS.");
    console.write_line("SHELL: UP/DOWN HISTORY | TAB COMMAND AND PATH COMPLETION");
    console.write_line("ACCOUNT SYSTEM: USERADD / LOGIN / LOGOUT / WHOAMI");
    console.write_line("FIRST SETUP: USERADD <name>, THEN LOGIN <name>.");
    console.write_line("");
    match memory_summary {
        Some((stats, pmm_ready)) => {
            console.write("USABLE RAM: ");
            console.write_decimal(stats.total_usable_bytes / (1024 * 1024));
            console.write_line(" MIB");
            console.write_line(if pmm_ready { "PMM STATUS: READY" } else { "PMM STATUS: TEST FAILED" });
        }
        None => console.write_line("MEMORY MANAGER: UNAVAILABLE"),
    }
    console.write_line("");
    write_shell_prompt(&mut console);
    console.render();

    serial_write("Interactive shell ready. PIC/PIT active; type HELP for commands.\r\n");
    interrupts::enable_interrupts();

    let mut history = CommandHistory::new();
    loop {
        let mut input = [0u8; CONSOLE_INPUT_CAPACITY];
        let mut input_len = 0usize;
        let prompt_start = console.current_line_len();
        let prompt_len = users::current_name_len() + "@XAVIEROS> ".len();
        let input_limit = console.input_limit(prompt_len);
        let mut history_offset: Option<usize> = None;
        let mut saved_input = [0u8; CONSOLE_INPUT_CAPACITY];
        let mut saved_input_len = 0usize;

        loop {
            match keyboard_poll_key() {
                Some(KeyboardKey::Character(byte)) => {
                    if input_len < input_limit && is_console_character(byte) {
                        input[input_len] = byte;
                        input_len += 1;
                        console.put_byte(byte);
                        serial_write_byte(byte);
                        console.render_current_line();
                        history_offset = None;
                    }
                }
                Some(KeyboardKey::Backspace) => {
                    if input_len > 0 {
                        input_len -= 1;
                        console.backspace();
                        serial_write("\x08 \x08");
                        console.render_current_line();
                        history_offset = None;
                    }
                }
                Some(KeyboardKey::HistoryUp) => {
                    if history.count > 0 {
                        let offset = match history_offset {
                            None => {
                                saved_input[..input_len].copy_from_slice(&input[..input_len]);
                                saved_input_len = input_len;
                                Some(0)
                            }
                            Some(current) if current + 1 < history.count => Some(current + 1),
                            Some(current) => Some(current),
                        };
                        if let Some(offset) = offset {
                            history_offset = Some(offset);
                            if let Some(length) = history.copy_back(offset, &mut input) {
                                input_len = length.min(input_limit);
                                console.replace_current_input(prompt_start, &input[..input_len]);
                            }
                        }
                    }
                }
                Some(KeyboardKey::HistoryDown) => {
                    if let Some(current) = history_offset {
                        if current == 0 {
                            history_offset = None;
                            input[..saved_input_len].copy_from_slice(&saved_input[..saved_input_len]);
                            input_len = saved_input_len.min(input_limit);
                        } else {
                            history_offset = Some(current - 1);
                            if let Some(length) = history.copy_back(current - 1, &mut input) {
                                input_len = length.min(input_limit);
                            }
                        }
                        console.replace_current_input(prompt_start, &input[..input_len]);
                    }
                }
                Some(KeyboardKey::Tab) => {
                    if let Some(new_len) = complete_input(&mut console, &mut input, input_len, input_limit) {
                        input_len = new_len;
                        history_offset = None;
                    }
                }
                Some(KeyboardKey::Enter) => {
                    console.newline();
                    history.push(&input[..input_len]);
                    let command_line = core::str::from_utf8(&input[..input_len]).unwrap_or("");
                    process_command(&mut console, command_line, memory_summary);
                    write_shell_prompt(&mut console);
                    console.render();
                    break;
                }
                None => interrupts::wait_for_interrupt(),
            }
        }
    }
}

fn is_console_character(byte: u8) -> bool {
    byte.is_ascii_alphanumeric()
        || matches!(
            byte,
            b' ' | b'-' | b'_' | b'.' | b'/' | b':' | b'?' | b'!' | b'=' | b'<' | b'>'
                | b',' | b'(' | b')' | b'+' | b'*' | b'[' | b']' | b'%' | b'^' | b'&'
                | b'@' | b'#' | b'$' | b'{' | b'}' | b'|' | b'\\' | b'\'' | b'"'
        )
}

fn keyboard_poll_key() -> Option<KeyboardKey> {
    const LEFT_SHIFT: u8 = 0x01;
    const RIGHT_SHIFT: u8 = 0x02;

    let scancode = interrupts::keyboard_pop_scancode()?;
    if scancode == 0xE0 {
        EXTENDED_SCANCODE.store(1, Ordering::Relaxed);
        return None;
    }
    if scancode == 0xE1 {
        EXTENDED_SCANCODE.store(0, Ordering::Relaxed);
        return None;
    }
    let extended = EXTENDED_SCANCODE.swap(0, Ordering::Relaxed) != 0;
    let released = scancode & 0x80 != 0;
    let code = scancode & 0x7F;
    if extended {
        if released { return None; }
        return match code {
            0x48 => Some(KeyboardKey::HistoryUp),
            0x50 => Some(KeyboardKey::HistoryDown),
            _ => None,
        };
    }
    if code == 0x2A || code == 0x36 {
        let bit = if code == 0x2A { LEFT_SHIFT } else { RIGHT_SHIFT };
        if released {
            SHIFT_STATE.fetch_and(!bit, Ordering::Relaxed);
        } else {
            SHIFT_STATE.fetch_or(bit, Ordering::Relaxed);
        }
        return None;
    }
    // Ignore key-release and extended-prefix scancodes.
    if released || code == 0xE0 {
        return None;
    }
    let shifted = SHIFT_STATE.load(Ordering::Relaxed) != 0;

    match code {
        0x1C => Some(KeyboardKey::Enter),
        0x0E => Some(KeyboardKey::Backspace),
        0x0F => Some(KeyboardKey::Tab),
        0x02 => Some(KeyboardKey::Character(if shifted { b'!' } else { b'1' })),
        0x03 => Some(KeyboardKey::Character(if shifted { b'@' } else { b'2' })),
        0x04 => Some(KeyboardKey::Character(if shifted { b'#' } else { b'3' })),
        0x05 => Some(KeyboardKey::Character(if shifted { b'$' } else { b'4' })),
        0x06 => Some(KeyboardKey::Character(if shifted { b'%' } else { b'5' })),
        0x07 => Some(KeyboardKey::Character(if shifted { b'^' } else { b'6' })),
        0x08 => Some(KeyboardKey::Character(if shifted { b'&' } else { b'7' })),
        0x09 => Some(KeyboardKey::Character(if shifted { b'*' } else { b'8' })),
        0x0A => Some(KeyboardKey::Character(if shifted { b'(' } else { b'9' })),
        0x0B => Some(KeyboardKey::Character(if shifted { b')' } else { b'0' })),
        0x0C => Some(KeyboardKey::Character(if shifted { b'_' } else { b'-' })),
        0x0D => Some(KeyboardKey::Character(if shifted { b'+' } else { b'=' })),
        0x10 => Some(KeyboardKey::Character(if shifted { b'Q' } else { b'q' })),
        0x11 => Some(KeyboardKey::Character(if shifted { b'W' } else { b'w' })),
        0x12 => Some(KeyboardKey::Character(if shifted { b'E' } else { b'e' })),
        0x13 => Some(KeyboardKey::Character(if shifted { b'R' } else { b'r' })),
        0x14 => Some(KeyboardKey::Character(if shifted { b'T' } else { b't' })),
        0x15 => Some(KeyboardKey::Character(if shifted { b'Y' } else { b'y' })),
        0x16 => Some(KeyboardKey::Character(if shifted { b'U' } else { b'u' })),
        0x17 => Some(KeyboardKey::Character(if shifted { b'I' } else { b'i' })),
        0x18 => Some(KeyboardKey::Character(if shifted { b'O' } else { b'o' })),
        0x19 => Some(KeyboardKey::Character(if shifted { b'P' } else { b'p' })),
        0x1A => Some(KeyboardKey::Character(if shifted { b'{' } else { b'[' })),
        0x1B => Some(KeyboardKey::Character(if shifted { b'}' } else { b']' })),
        0x1E => Some(KeyboardKey::Character(if shifted { b'A' } else { b'a' })),
        0x1F => Some(KeyboardKey::Character(if shifted { b'S' } else { b's' })),
        0x20 => Some(KeyboardKey::Character(if shifted { b'D' } else { b'd' })),
        0x21 => Some(KeyboardKey::Character(if shifted { b'F' } else { b'f' })),
        0x22 => Some(KeyboardKey::Character(if shifted { b'G' } else { b'g' })),
        0x23 => Some(KeyboardKey::Character(if shifted { b'H' } else { b'h' })),
        0x24 => Some(KeyboardKey::Character(if shifted { b'J' } else { b'j' })),
        0x25 => Some(KeyboardKey::Character(if shifted { b'K' } else { b'k' })),
        0x26 => Some(KeyboardKey::Character(if shifted { b'L' } else { b'l' })),
        0x27 => Some(KeyboardKey::Character(if shifted { b':' } else { b';' })),
        0x28 => Some(KeyboardKey::Character(if shifted { b'"' } else { b'\'' })),
        0x2B => Some(KeyboardKey::Character(if shifted { b'|' } else { b'\\' })),
        0x2C => Some(KeyboardKey::Character(if shifted { b'Z' } else { b'z' })),
        0x2D => Some(KeyboardKey::Character(if shifted { b'X' } else { b'x' })),
        0x2E => Some(KeyboardKey::Character(if shifted { b'C' } else { b'c' })),
        0x2F => Some(KeyboardKey::Character(if shifted { b'V' } else { b'v' })),
        0x30 => Some(KeyboardKey::Character(if shifted { b'B' } else { b'b' })),
        0x31 => Some(KeyboardKey::Character(if shifted { b'N' } else { b'n' })),
        0x32 => Some(KeyboardKey::Character(if shifted { b'M' } else { b'm' })),
        0x33 => Some(KeyboardKey::Character(if shifted { b'<' } else { b',' })),
        0x34 => Some(KeyboardKey::Character(if shifted { b'>' } else { b'.' })),
        0x35 => Some(KeyboardKey::Character(if shifted { b'?' } else { b'/' })),
        0x39 => Some(KeyboardKey::Character(b' ')),
        _ => None,
    }
}

const COMMAND_COMPLETIONS: &[&str] = &[
    "ABOUT", "BANNER", "CALC", "CAT", "CLEAR", "CPU", "DIAG", "DIR", "DISK", "ECHO",
    "FORMAT", "FSINFO", "FSTEST", "HALT", "HEAP", "HELP", "HHDM", "HOME", "IDT",
    "INFO", "IRQ", "LOGIN", "LOGOUT", "LS", "MEM", "MKDIR", "PASSWD", "PMM", "PWD",
    "REBOOT", "RM", "SELFTEST", "STAT", "SYSINFO", "TASKS", "TASKTEST", "TIME", "TOUCH",
    "TRAP", "USERADD", "USERS", "VERSION", "WHOAMI", "WRITE",
];

fn prefix_matches(candidate: &str, prefix: &[u8]) -> bool {
    candidate.as_bytes().len() >= prefix.len()
        && candidate.as_bytes()[..prefix.len()].iter().zip(prefix.iter())
            .all(|(left, right)| left.eq_ignore_ascii_case(right))
}

fn complete_input(
    console: &mut Console,
    input: &mut [u8; CONSOLE_INPUT_CAPACITY],
    input_len: usize,
    input_limit: usize,
) -> Option<usize> {
    if input_len >= input_limit { return None; }
    let snapshot = *input;
    let current = &snapshot[..input_len];
    let first_space = current.iter().position(|byte| byte.is_ascii_whitespace());
    if first_space.is_none() {
        let mut count = 0usize;
        let mut first_match = "";
        let mut common_len = 0usize;
        for &command in COMMAND_COMPLETIONS {
            if !prefix_matches(command, current) { continue; }
            if count == 0 {
                first_match = command;
                common_len = command.len();
            } else {
                common_len = common_len.min(command.as_bytes().iter().zip(first_match.as_bytes())
                    .take_while(|(a, b)| a.eq_ignore_ascii_case(b)).count());
            }
            count += 1;
        }
        if count == 0 { return None; }
        let chosen = if count == 1 { first_match.len() } else { common_len };
        if chosen <= input_len { return None; }
        let mut replacement = [0u8; CONSOLE_INPUT_CAPACITY];
        replacement[..chosen].copy_from_slice(&first_match.as_bytes()[..chosen]);
        let mut new_len = chosen;
        if count == 1 && new_len < input_limit {
            replacement[new_len] = b' ';
            new_len += 1;
        }
        let prompt_start = console.current_line_len().saturating_sub(input_len);
        input[..new_len].copy_from_slice(&replacement[..new_len]);
        input[new_len..].fill(0);
        console.replace_current_input(prompt_start, &input[..new_len]);
        return Some(new_len);
    }

    let command_end = first_space.unwrap_or(0);
    let command = core::str::from_utf8(&current[..command_end]).unwrap_or("");
    let path_command = ["LS", "DIR", "CAT", "STAT", "MKDIR", "WRITE", "TOUCH", "RM"]
        .iter().any(|known| command.eq_ignore_ascii_case(*known));
    if !path_command { return None; }
    // WRITE takes both a path and file contents; stop path completion once the content token starts.
    if command.eq_ignore_ascii_case("WRITE")
        && current.iter().filter(|byte| byte.is_ascii_whitespace()).count() > 1
    {
        return None;
    }
    let token_start = current.iter().rposition(|byte| byte.is_ascii_whitespace()).map(|i| i + 1).unwrap_or(0);
    let token = core::str::from_utf8(&current[token_start..]).unwrap_or("");

    let (directory, parent_prefix, leaf_prefix) = if token.is_empty() {
        ("/", "", "")
    } else if token.ends_with('/') {
        let parent = token.trim_end_matches('/');
        (if parent.is_empty() { "/" } else { parent }, token, "")
    } else if let Some(slash) = token.rfind('/') {
        let parent = &token[..slash];
        (if parent.is_empty() { "/" } else { parent }, &token[..slash + 1], &token[slash + 1..])
    } else {
        ("/", "", token)
    };
    if !users::may_access_path(directory) { return None; }

    let mut first_name = [0u8; 64];
    let mut first_name_len = 0usize;
    let mut first_is_dir = false;
    let mut count = 0usize;
    let mut common_len = 0usize;
    let result = vfs::list_directory(directory, |name, is_dir| {
        if !prefix_matches(name, leaf_prefix.as_bytes()) { return; }
        let name_bytes = name.as_bytes();
        if name_bytes.len() > first_name.len() { return; }
        let candidate_len = parent_prefix.len().saturating_add(name_bytes.len());
        if candidate_len > CONSOLE_INPUT_CAPACITY { return; }
        let mut candidate_path = [0u8; CONSOLE_INPUT_CAPACITY];
        candidate_path[..parent_prefix.len()].copy_from_slice(parent_prefix.as_bytes());
        candidate_path[parent_prefix.len()..candidate_len].copy_from_slice(name_bytes);
        let candidate_str = core::str::from_utf8(&candidate_path[..candidate_len]).unwrap_or("");
        if !users::may_access_path(candidate_str) { return; }
        if count == 0 {
            first_name[..name_bytes.len()].copy_from_slice(name_bytes);
            first_name_len = name_bytes.len();
            first_is_dir = is_dir;
            common_len = name_bytes.len();
        } else {
            common_len = common_len.min(name_bytes.iter().zip(first_name[..first_name_len].iter())
                .take_while(|(a, b)| a.eq_ignore_ascii_case(b)).count());
        }
        count += 1;
    });
    if result.is_err() || count == 0 { return None; }

    let unique = count == 1;
    let leaf_len = if unique { first_name_len } else { common_len };
    if leaf_len <= leaf_prefix.len() { return None; }
    let mut new_len = token_start.checked_add(parent_prefix.len())?.checked_add(leaf_len)?;
    // Reject an overlong completion before slicing into the fixed-size line buffer.
    if new_len > input_limit || new_len > CONSOLE_INPUT_CAPACITY { return None; }
    let mut replacement = [0u8; CONSOLE_INPUT_CAPACITY];
    replacement[..token_start].copy_from_slice(&current[..token_start]);
    let mut write_at = token_start;
    replacement[write_at..write_at + parent_prefix.len()].copy_from_slice(parent_prefix.as_bytes());
    write_at += parent_prefix.len();
    replacement[write_at..write_at + leaf_len].copy_from_slice(&first_name[..leaf_len]);
    write_at += leaf_len;
    if unique {
        let suffix = if first_is_dir { b'/' } else { b' ' };
        if write_at < input_limit { replacement[write_at] = suffix; new_len += 1; }
    }
    let prompt_start = console.current_line_len().saturating_sub(input_len);
    input[..new_len].copy_from_slice(&replacement[..new_len]);
    input[new_len..].fill(0);
    console.replace_current_input(prompt_start, &input[..new_len]);
    Some(new_len)
}

fn write_shell_prompt(console: &mut Console) {
    let mut name_buffer = [0u8; 16];
    console.write(users::current_name(&mut name_buffer));
    console.write("@XAVIEROS> ");
}

fn read_hidden_input(console: &mut Console, prompt: &str, output: &mut [u8]) -> Option<usize> {
    console.write(prompt);
    console.render();
    let mut length = 0usize;
    loop {
        match keyboard_poll_key() {
            Some(KeyboardKey::Character(byte)) if (0x20..=0x7E).contains(&byte) => {
                if length < output.len() {
                    output[length] = byte;
                    length += 1;
                    console.put_byte(b'*');
                    serial_write_byte(b'*');
                    console.render_current_line();
                }
            }
            Some(KeyboardKey::Backspace) if length > 0 => {
                length -= 1;
                output[length] = 0;
                console.backspace();
                serial_write("\x08 \x08");
                console.render_current_line();
            }
            Some(KeyboardKey::Enter) => {
                console.newline();
                console.render();
                return Some(length);
            }
            None => interrupts::wait_for_interrupt(),
            _ => {}
        }
    }
}

fn command_user_add(console: &mut Console, username: &str) {
    if !users::is_initialized() {
        console.write_line("USER DATABASE IS UNAVAILABLE. CHECK THE DISK OR USE FORMAT DISK TO RECOVER.");
        return;
    }
    if !users::can_manage_accounts() {
        console.write_line("USERADD REQUIRES FIRST-RUN LIVE SETUP OR ADMINISTRATOR ACCESS.");
        return;
    }
    if username.is_empty() {
        console.write_line("USAGE: USERADD <LOWERCASE_NAME>");
        return;
    }
    if let Err(error) = users::validate_new_username(username) {
        console.write("USERADD ERROR: "); console.write_line(error);
        return;
    }
    let mut first = [0u8; 64];
    let mut second = [0u8; 64];
    let first_len = match read_hidden_input(console, "NEW PASSWORD (8-63 CHARS): ", &mut first) {
        Some(length) => length,
        None => { first.fill(0); second.fill(0); return; }
    };
    let second_len = match read_hidden_input(console, "CONFIRM PASSWORD: ", &mut second) {
        Some(length) => length,
        None => { first.fill(0); second.fill(0); return; }
    };
    if first_len != second_len || first[..first_len] != second[..second_len] {
        first.fill(0); second.fill(0);
        console.write_line("PASSWORDS DO NOT MATCH.");
        return;
    }
    let result = users::create(username, &first[..first_len]);
    first.fill(0); second.fill(0);
    match result {
        Ok(()) => {
            console.write("USER CREATED: "); console.write_line(username);
            console.write_line("HOME DIRECTORY CREATED UNDER /DISK/HOME.");
            console.write_line("USE LOGIN <NAME> TO SIGN IN.");
        }
        Err(error) => { console.write("USERADD ERROR: "); console.write_line(error); }
    }
}

fn command_login(console: &mut Console, username: &str) {
    if username.is_empty() {
        console.write_line("USAGE: LOGIN <NAME>");
        return;
    }
    let mut password = [0u8; 64];
    let length = match read_hidden_input(console, "PASSWORD: ", &mut password) {
        Some(length) => length,
        None => { password.fill(0); return; }
    };
    let result = users::authenticate(username, &password[..length]);
    password.fill(0);
    match result {
        Ok(()) => {
            let mut name_buffer = [0u8; 16];
            console.write("LOGIN SUCCESSFUL. WELCOME, "); console.write_line(users::current_name(&mut name_buffer));
            if users::is_admin() {
                console.write_line("ROLE: ADMINISTRATOR. USER MANAGEMENT AND DISK ACCESS ENABLED.");
            } else {
                console.write_line("ROLE: USER. DISK ACCESS IS LIMITED TO YOUR HOME DIRECTORY.");
            }
            console.write_line("NOTE: THIS DOES NOT ISOLATE KERNEL PRIVILEGES.");
        }
        Err(error) => { console.write("LOGIN FAILED: "); console.write_line(error); }
    }
}

fn command_change_password(console: &mut Console) {
    if users::is_live() {
        console.write_line("LIVE SESSION HAS NO PASSWORD. USE USERADD TO CREATE AN ACCOUNT.");
        return;
    }
    let mut old_password = [0u8; 64];
    let mut new_password = [0u8; 64];
    let mut confirm = [0u8; 64];
    let old_len = match read_hidden_input(console, "CURRENT PASSWORD: ", &mut old_password) {
        Some(length) => length,
        None => { old_password.fill(0); new_password.fill(0); confirm.fill(0); return; }
    };
    let new_len = match read_hidden_input(console, "NEW PASSWORD (8-63 CHARS): ", &mut new_password) {
        Some(length) => length,
        None => { old_password.fill(0); new_password.fill(0); confirm.fill(0); return; }
    };
    let confirm_len = match read_hidden_input(console, "CONFIRM NEW PASSWORD: ", &mut confirm) {
        Some(length) => length,
        None => { old_password.fill(0); new_password.fill(0); confirm.fill(0); return; }
    };
    if new_len != confirm_len || new_password[..new_len] != confirm[..confirm_len] {
        old_password.fill(0); new_password.fill(0); confirm.fill(0);
        console.write_line("PASSWORDS DO NOT MATCH.");
        return;
    }
    let result = users::change_password(&old_password[..old_len], &new_password[..new_len]);
    old_password.fill(0); new_password.fill(0); confirm.fill(0);
    match result {
        Ok(()) => console.write_line("PASSWORD UPDATED."),
        Err(error) => { console.write("PASSWD ERROR: "); console.write_line(error); }
    }
}

fn process_command(
    console: &mut Console,
    command_line: &str,
    memory_summary: Option<(MemoryStats, bool)>,
) {
    let command_line = command_line.trim();
    if command_line.is_empty() {
        return;
    }
    serial_write("SHELL COMMAND: ");
    serial_write(command_line);
    serial_write("\r\n");

    let raw_command = command_line.split_whitespace().next().unwrap_or("");
    let mut command_bytes = [0u8; 24];
    if raw_command.len() > command_bytes.len() {
        console.write_line("COMMAND NAME TOO LONG.");
        return;
    }
    for (index, byte) in raw_command.bytes().enumerate() {
        command_bytes[index] = byte.to_ascii_uppercase();
    }
    let command = core::str::from_utf8(&command_bytes[..raw_command.len()]).unwrap_or("");
    let arguments = command_line.get(raw_command.len()..).unwrap_or("").trim();

    match command {
        "HELP" | "?" => {
            console.write_line("XAVIEROS COMMANDS:");
            console.write_line("HELP / ?    - SHOW THIS LIST");
            console.write_line("UP / DOWN   - NAVIGATE COMMAND HISTORY (16 COMMANDS)");
            console.write_line("TAB         - COMPLETE COMMANDS AND VFS PATHS");
            console.write_line("ABOUT       - ABOUT XAVIEROS");
            console.write_line("BANNER      - SHOW THE XAVIEROS LOGO");
            console.write_line("CALC A OP B - INTEGER CALCULATOR (+ - * /)");
            console.write_line("CLEAR       - CLEAR THE CONSOLE");
            console.write_line("CPU         - SHOW CPU DETAILS FROM CPUID");
            console.write_line("DIAG        - RUN A SYSTEM DIAGNOSTIC");
            console.write_line("ECHO TEXT   - PRINT TEXT");
            console.write_line("HHDM        - SHOW DIRECT-MAP INFORMATION");
            console.write_line("HEAP        - SHOW KERNEL HEAP STATISTICS");
            console.write_line("LS [PATH]   - LIST A VFS DIRECTORY (ALIAS: DIR)");
            console.write_line("CAT PATH    - PRINT A FILE FROM ANY MOUNT");
            console.write_line("STAT PATH   - SHOW FILE OR DIRECTORY METADATA");
            console.write_line("MKDIR PATH  - CREATE A DIRECTORY UNDER /DISK");
            console.write_line("WRITE PATH TEXT - WRITE TEXT UNDER /DISK");
            console.write_line("TOUCH PATH  - CREATE AN EMPTY FILE UNDER /DISK");
            console.write_line("RM PATH     - REMOVE A FILE / EMPTY DIRECTORY");
            console.write_line("USERADD NAME- CREATE A USER (FIRST USER / ADMIN ONLY)");
            console.write_line("USERS       - LIST USERS (ADMIN ONLY)");
            console.write_line("LOGIN NAME  - SIGN IN TO A LOCAL USER");
            console.write_line("LOGOUT      - RETURN TO THE LIVE SESSION");
            console.write_line("WHOAMI      - SHOW CURRENT SESSION IDENTITY");
            console.write_line("PASSWD      - CHANGE YOUR ACCOUNT PASSWORD");
            console.write_line("HOME        - SHOW THE CURRENT USER HOME");
            console.write_line("DISK        - SHOW WRITABLE DISK STATUS");
            console.write_line("FORMAT DISK - ERASE AND FORMAT THE DATA DISK");
            console.write_line("TASKS       - LIST COOPERATIVE KERNEL TASKS");
            console.write_line("TASKTEST    - TEST CONTEXT SWITCHING");
            console.write_line("PWD         - SHOW CURRENT DIRECTORY");
            console.write_line("FSINFO      - SHOW INITRAMFS STATISTICS");
            console.write_line("FSTEST      - TEST THE EMBEDDED INITRAMFS");
            console.write_line("IDT         - SHOW IDT / EXCEPTION / IRQ STATUS");
            console.write_line("IRQ         - SHOW PIC, KEYBOARD IRQ AND TIMER STATUS");
            console.write_line("TIME        - SHOW PIT TICKS AND UPTIME");
            console.write_line("INFO        - SHOW KERNEL INFORMATION");
            console.write_line("MEM         - SHOW MEMORY USAGE");
            console.write_line("PMM         - SHOW PHYSICAL PAGE ALLOCATOR");
            console.write_line("SELFTEST    - TEST MEMORY, IRQ, HEAP, VFS, DISK AND TASKS");
            console.write_line("SYSINFO     - SHOW THE XAVIEROS SYSTEM SUMMARY");
            console.write_line("TRAP        - TRIGGER A TEST INVALID-OPCODE FAULT (HALTS)");
            console.write_line("VERSION     - SHOW THE OS VERSION");
            console.write_line("REBOOT      - RESTART THE VIRTUAL MACHINE");
            console.write_line("HALT        - STOP THE CPU");
        }
        "CLEAR" => console.clear(),
        "ABOUT" => {
            console.write_line("XAVIEROS IS AN EXPERIMENTAL X86_64 OS.");
            console.write_line("KERNEL: RUST NO_STD, BUILT FROM SCRATCH.");
            console.write_line("BOOT: LIMINE | TEST MACHINE: QEMU");
            console.write_line("PROJECT VERSION: 0.1.2");
            console.write_line("NO DESKTOP. JUST THE KERNEL AND THE SHELL.");
        }
        "BANNER" => show_banner(console),
        "CALC" => calculate(console, arguments),
        "CPU" => show_cpu(console),
        "DIAG" => run_diagnostics(console, memory_summary),
        "INFO" => {
            console.write_line("KERNEL: XAVIEROS");
            console.write_line("VERSION: 0.1.2");
            console.write_line("ARCHITECTURE: X86_64");
            console.write_line("BOOTLOADER: LIMINE");
            console.write_line("LANGUAGE: RUST NO_STD");
            console.write_line("INTERFACE: FRAMEBUFFER + PS/2 KEYBOARD IRQ");
        }
        "SYSINFO" => show_sysinfo(console, memory_summary),
        "MEM" => match memory_summary {
            Some((stats, pmm_ready)) => {
                console.write("MEMORY MAP ENTRIES: ");
                console.write_decimal(stats.entry_count as u64);
                console.write_line("");
                console.write("USABLE RAM: ");
                console.write_decimal(stats.total_usable_bytes / (1024 * 1024));
                console.write_line(" MIB");
                console.write("PMM MANAGED PAGES: ");
                console.write_decimal(stats.managed_pages as u64);
                console.write_line("");
                console.write("PMM FREE PAGES: ");
                console.write_decimal(memory::free_page_count() as u64);
                console.write_line("");
                console.write("PMM FREE MEMORY: ");
                console.write_decimal((memory::free_page_count() as u64 * 4096) / (1024 * 1024));
                console.write_line(" MIB");
                console.write("HHDM OFFSET: 0X");
                console.write_hex(stats.hhdm_offset);
                console.write_line("");
                console.write_line(if pmm_ready { "PMM SELF-TEST: PASS" } else { "PMM SELF-TEST: FAIL" });
            }
            None => console.write_line("MEMORY MANAGER IS NOT AVAILABLE."),
        },
        "PMM" => show_pmm(console, memory_summary),
        "HHDM" => show_hhdm(console, memory_summary),
        "HEAP" => show_heap(console),
        "LS" | "DIR" => {
            let mut home_buffer = [0u8; 48];
            let path = if arguments.is_empty() && !users::is_live() {
                users::home_path(&mut home_buffer).unwrap_or("/")
            } else if arguments.is_empty() { "/" } else { arguments };
            if !users::may_access_path(path) {
                console.write_line("ACCESS DENIED: DISK PATH IS OUTSIDE YOUR HOME.");
                return;
            }
            console.write("DIRECTORY: ");
            console.write_line(path);
            match vfs::list_directory(path, |name, is_dir| {
                console.write(if is_dir { "DIR  " } else { "FILE " });
                console.write_line(name);
            }) {
                Ok(0) => console.write_line("(EMPTY DIRECTORY)"),
                Ok(count) => {
                    console.write("ITEMS: ");
                    console.write_decimal(count as u64);
                    console.write_line("");
                }
                Err(error) => {
                    console.write("LS ERROR: ");
                    console.write_line(error);
                }
            }
        }
        "CAT" => {
            if arguments.is_empty() {
                console.write_line("USAGE: CAT PATH");
            } else if !users::may_access_path(arguments) {
                console.write_line("ACCESS DENIED: DISK PATH IS OUTSIDE YOUR HOME.");
            } else {
                let mut buffer = [0u8; 4096];
                match vfs::read_file(arguments, &mut buffer) {
                    Ok(length) => match core::str::from_utf8(&buffer[..length]) {
                        Ok(text) => {
                            console.write(text);
                            if !text.ends_with('\n') { console.write_line(""); }
                        }
                        Err(_) => {
                            console.write_line("CAT: FILE IS NOT UTF-8 TEXT.");
                            console.write("SIZE: ");
                            console.write_decimal(length as u64);
                            console.write_line(" BYTES");
                        }
                    },
                    Err(error) => {
                        console.write("CAT ERROR: ");
                        console.write_line(error);
                    }
                }
            }
        }
        "STAT" => {
            if arguments.is_empty() {
                console.write_line("USAGE: STAT PATH");
            } else if !users::may_access_path(arguments) {
                console.write_line("ACCESS DENIED: DISK PATH IS OUTSIDE YOUR HOME.");
            } else {
                match vfs::stat(arguments) {
                    Ok(entry) => {
                        console.write("PATH: ");
                        console.write_line(arguments);
                        console.write_line(if entry.is_dir { "TYPE: DIRECTORY" } else { "TYPE: FILE" });
                        console.write("SIZE: ");
                        console.write_decimal(entry.size as u64);
                        console.write_line(" BYTES");
                        console.write_line(if entry.on_disk { "SOURCE: WRITABLE DISK" } else { "SOURCE: EMBEDDED INITRAMFS" });
                    }
                    Err(error) => {
                        console.write("STAT ERROR: ");
                        console.write_line(error);
                    }
                }
            }
        }
        "MKDIR" => {
            if arguments.is_empty() { console.write_line("USAGE: MKDIR /DISK/PATH"); }
            else if !users::may_access_path(arguments) { console.write_line("ACCESS DENIED: DISK PATH IS OUTSIDE YOUR HOME."); }
            else { match vfs::mkdir(arguments) {
                Ok(()) => console.write_line("DIRECTORY CREATED."),
                Err(error) => { console.write("MKDIR ERROR: "); console.write_line(error); }
            } }
        }
        "WRITE" => {
            match arguments.split_once(' ') {
                Some((path, text)) if !path.is_empty() && users::may_access_path(path) => match vfs::write_file(path, text.trim_start().as_bytes()) {
                    Ok(()) => console.write_line("FILE WRITTEN TO DISK."),
                    Err(error) => { console.write("WRITE ERROR: "); console.write_line(error); }
                },
                Some((_, _)) => console.write_line("ACCESS DENIED OR INVALID PATH."),
                _ => console.write_line("USAGE: WRITE /DISK/PATH TEXT"),
            }
        }
        "TOUCH" => {
            if arguments.is_empty() { console.write_line("USAGE: TOUCH /DISK/PATH"); }
            else if !users::may_access_path(arguments) { console.write_line("ACCESS DENIED: DISK PATH IS OUTSIDE YOUR HOME."); }
            else { match vfs::touch(arguments) {
                Ok(()) => console.write_line("EMPTY FILE CREATED."),
                Err(error) => { console.write("TOUCH ERROR: "); console.write_line(error); }
            } }
        }
        "RM" => {
            if arguments.is_empty() { console.write_line("USAGE: RM /DISK/PATH"); }
            else if !users::may_access_path(arguments) { console.write_line("ACCESS DENIED: DISK PATH IS OUTSIDE YOUR HOME."); }
            else { match vfs::remove(arguments) {
                Ok(()) => console.write_line("ENTRY REMOVED."),
                Err(error) => { console.write("RM ERROR: "); console.write_line(error); }
            } }
        }
        "USERADD" => command_user_add(console, arguments),
        "USERS" => {
            if !users::can_manage_accounts() {
                console.write_line("ACCESS DENIED: ADMINISTRATOR REQUIRED.");
            } else if !users::is_initialized() {
                console.write_line("USER DATABASE IS UNAVAILABLE; CHECK DISK STATUS.");
            } else if users::count() == 0 {
                console.write_line("NO PERSISTENT USERS. USE: USERADD <NAME>");
            } else {
                console.write_line("LOCAL USERS:");
                users::list(|name| console.write_line(name));
            }
        }
        "LOGIN" => command_login(console, arguments),
        "LOGOUT" => match users::logout() {
            Ok(()) => console.write_line("SIGNED OUT. RETURNED TO LIVE SESSION."),
            Err(error) => { console.write("LOGOUT ERROR: "); console.write_line(error); }
        },
        "WHOAMI" => {
            let mut name_buffer = [0u8; 16];
            console.write("CURRENT USER: "); console.write_line(users::current_name(&mut name_buffer));
            console.write_line(if users::is_live() { "SESSION: LIVE / TEMPORARY" } else if users::is_admin() { "SESSION: PERSISTENT ADMINISTRATOR" } else { "SESSION: PERSISTENT USER" });
        }
        "PASSWD" => command_change_password(console),
        "HOME" => {
            if users::is_live() { console.write_line("LIVE SESSION HOME: / (NO PRIVATE HOME)"); }
            else {
                let mut buffer = [0u8; 48];
                console.write("HOME: ");
                console.write_line(users::home_path(&mut buffer).unwrap_or("/"));
            }
        }
        "DISK" | "DINFO" => show_disk_info(console),
        "FORMAT" => {
            if !users::can_manage_accounts() {
                console.write_line("FORMAT REQUIRES ADMINISTRATOR ACCESS.");
            } else if !arguments.eq_ignore_ascii_case("DISK") {
                console.write_line("USAGE: FORMAT DISK (ERASES THE WRITABLE DISK)");
            } else {
                console.write_line("WARNING: FORMATTING ERASES ALL FILES UNDER /DISK.");
                match diskfs::format() {
                    Ok(()) => {
                        console.write_line("XVDISKFS FORMATTED AND MOUNTED.");
                        match users::initialize() {
                            Ok(()) => console.write_line("EMPTY LIVE USER DATABASE INITIALIZED."),
                            Err(error) => { console.write("USER STORE RESET WARNING: "); console.write_line(error); }
                        }
                    },
                    Err(error) => { console.write("FORMAT ERROR: "); console.write_line(error); }
                }
            }
        }
        "TASKS" => {
            let (total, runnable, switches) = task::stats();
            console.write("TASKS: "); console.write_decimal(total as u64);
            console.write(" | RUNNABLE: "); console.write_decimal(runnable as u64);
            console.write(" | CONTEXT SWITCHES: "); console.write_decimal(switches);
            console.write_line("");
            task::list(|id, name, state, count| {
                console.write("#"); console.write_decimal(id as u64); console.write(" ");
                console.write(name); console.write(" "); console.write(state);
                console.write(" SWITCHES="); console.write_decimal(count); console.write_line("");
            });
        }
        "TASKTEST" => match task::self_test() {
            Ok(()) => console.write_line("COOPERATIVE SCHEDULER SELF-TEST: PASS"),
            Err(error) => { console.write("TASKTEST FAIL: "); console.write_line(error); }
        },
        "PWD" => {
            if users::is_live() { console.write_line("CURRENT DIRECTORY: / (LIVE SESSION)"); }
            else {
                let mut buffer = [0u8; 48];
                console.write("CURRENT DIRECTORY: ");
                console.write_line(users::home_path(&mut buffer).unwrap_or("/"));
            }
        },
        "FSINFO" => show_fsinfo(console),
        "FSTEST" => match fs::self_test() {
            Ok(()) => console.write_line("FILESYSTEM SELF-TEST: PASS"),
            Err(error) => {
                console.write("FILESYSTEM SELF-TEST: FAIL: ");
                console.write_line(error);
            }
        },
        "IDT" => show_idt(console),
        "IRQ" => show_irq(console),
        "TIME" => show_time(console),
        "TRAP" => {
            console.write_line("TEST FAULT: EXECUTING UD2; CHECK THE SERIAL LOG.");
            console.render();
            unsafe { asm!("ud2", options(nomem, nostack)); }
            halt_forever();
        }
        "VERSION" => console.write_line("XAVIEROS V0.1.2"),
        "ECHO" => console.write_line(arguments),
        "SELFTEST" => {
            let mut failed = false;
            match memory_summary {
                Some((_, true)) => match memory::self_test() {
                    Ok(()) => console.write_line("PMM / HHDM SELF-TEST: PASS"),
                    Err(error) => {
                        failed = true;
                        console.write("PMM / HHDM SELF-TEST: FAIL: ");
                        console.write_line(error);
                    }
                },
                Some(_) => {
                    failed = true;
                    console.write_line("PMM / HHDM SELF-TEST: SKIPPED (INIT TEST FAILED)");
                }
                None => {
                    failed = true;
                    console.write_line("PMM / HHDM SELF-TEST: SKIPPED (MEMORY MANAGER UNAVAILABLE)");
                }
            }
            match heap::self_test() {
                Ok(()) => console.write_line("KERNEL HEAP SELF-TEST: PASS"),
                Err(error) => {
                    failed = true;
                    console.write("KERNEL HEAP SELF-TEST: FAIL: ");
                    console.write_line(error);
                }
            }
            match fs::self_test() {
                Ok(()) => console.write_line("INITRAMFS SELF-TEST: PASS"),
                Err(error) => {
                    failed = true;
                    console.write("INITRAMFS SELF-TEST: FAIL: ");
                    console.write_line(error);
                }
            }
            if diskfs::mount_status() {
                match diskfs::self_test() {
                    Ok(()) => console.write_line("WRITABLE DISK FILESYSTEM SELF-TEST: PASS"),
                    Err(error) => {
                        failed = true;
                        console.write("WRITABLE DISK FILESYSTEM SELF-TEST: FAIL: ");
                        console.write_line(error);
                    }
                }
            } else {
                console.write_line("WRITABLE DISK FILESYSTEM: SKIPPED (NOT MOUNTED)");
            }
            match vfs::self_test() {
                Ok(()) => console.write_line("VFS MOUNT SELF-TEST: PASS"),
                Err(error) => {
                    failed = true;
                    console.write("VFS MOUNT SELF-TEST: FAIL: ");
                    console.write_line(error);
                }
            }
            match task::self_test() {
                Ok(()) => console.write_line("COOPERATIVE SCHEDULER SELF-TEST: PASS"),
                Err(error) => {
                    failed = true;
                    console.write("COOPERATIVE SCHEDULER SELF-TEST: FAIL: ");
                    console.write_line(error);
                }
            }
            if users::is_initialized() {
                match users::self_test() {
                    Ok(()) => console.write_line("USER DATABASE / PASSWORD HASH SELF-TEST: PASS"),
                    Err(error) => { failed = true; console.write("USER SELF-TEST: FAIL: "); console.write_line(error); }
                }
            } else {
                console.write_line("USER DATABASE: SKIPPED (DISK / DATABASE UNAVAILABLE)");
            }
            console.write_line(if interrupts::is_initialized() { "IDT INITIALIZATION: PASS" } else { "IDT INITIALIZATION: FAIL" });
            if !interrupts::is_initialized() { failed = true; }
            console.write_line(if interrupts::hardware_is_initialized() { "PIC / PIT INITIALIZATION: PASS" } else { "PIC / PIT INITIALIZATION: FAIL" });
            if !interrupts::hardware_is_initialized() { failed = true; }
            console.write_line(if interrupts::keyboard_is_initialized() { "PS/2 KEYBOARD IRQ SETUP: PASS" } else { "PS/2 KEYBOARD IRQ SETUP: FAIL" });
            if !interrupts::keyboard_is_initialized() { failed = true; }
            console.write_line(if interrupts::timer_ticks() > 0 { "PIT TICK DELIVERY: PASS" } else { "PIT TICK DELIVERY: FAIL" });
            if interrupts::timer_ticks() == 0 { failed = true; }
            if !failed { console.write_line("ALL SELF-TESTS: PASS"); } else { console.write_line("SELFTEST: ONE OR MORE CHECKS FAILED"); }
        },
        "REBOOT" => reboot_machine(),
        "HALT" => {
            console.write_line("CPU HALTED. CLOSE OR RESTART QEMU TO CONTINUE.");
            console.render();
            halt_forever();
        }
        _ => {
            console.write("UNKNOWN COMMAND: ");
            console.write_line(command);
            console.write_line("TYPE HELP FOR AVAILABLE COMMANDS.");
        }
    }
}

fn show_disk_info(console: &mut Console) {
    console.write_line("XAVIEROS WRITABLE STORAGE");
    console.write_line(if ata::is_ready() { "ATA PRIMARY MASTER: READY" } else { "ATA PRIMARY MASTER: UNAVAILABLE" });
    console.write_line(if diskfs::mount_status() { "XVDISKFS: MOUNTED AT /DISK" } else { "XVDISKFS: NOT MOUNTED" });
    match diskfs::info() {
        Some(info) => {
            console.write_line(if info.available { "DEVICE: AVAILABLE" } else { "DEVICE: UNAVAILABLE" });
            console.write("SECTORS: "); console.write_decimal(info.total_sectors as u64);
            console.write(" (APPROX "); console.write_decimal((info.total_sectors as u64 * 512) / (1024 * 1024));
            console.write_line(" MIB)");
            console.write("FILES: "); console.write_decimal(info.files as u64);
            console.write(" | DIRECTORIES: "); console.write_decimal(info.directories as u64);
            console.write_line("");
            console.write("FREE DIRECTORY SLOTS: "); console.write_decimal(info.free_entries as u64);
            console.write_line("");
            console.write("FILE DATA: "); console.write_decimal(info.used_bytes);
            console.write_line(" BYTES");
        }
        None => console.write_line("DISK STATISTICS UNAVAILABLE."),
    }
    if !diskfs::mount_status() {
        console.write("DETAIL: "); console.write_line(diskfs::last_error());
    }
}

fn show_fsinfo(console: &mut Console) {
    match fs::info() {
        Ok(info) => {
            console.write_line("XAVIEROS READ-ONLY INITRAMFS");
            console.write_line("FORMAT: XVRFS V1");
            console.write("IMAGE SIZE: ");
            console.write_decimal(info.image_bytes as u64);
            console.write_line(" BYTES");
            console.write("ENTRIES: ");
            console.write_decimal(info.entries as u64);
            console.write_line("");
            console.write("DIRECTORIES: ");
            console.write_decimal(info.directories as u64);
            console.write_line("");
            console.write("FILES: ");
            console.write_decimal(info.files as u64);
            console.write_line("");
            console.write("FILE DATA: ");
            console.write_decimal(info.file_bytes as u64);
            console.write_line(" BYTES");
            console.write_line("STORAGE: EMBEDDED IN KERNEL ELF");
            console.write_line("MODE: READ ONLY");
        }
        Err(error) => {
            console.write("FILESYSTEM ERROR: ");
            console.write_line(error);
        }
    }
}

fn show_banner(console: &mut Console) {
    console.write_line("");
    console.write_line("XX              XX");
    console.write_line(" XX            XX ");
    console.write_line("  XX          XX  ");
    console.write_line("   XX        XX   ");
    console.write_line("    XX      XX    ");
    console.write_line("     XX    XX     ");
    console.write_line("      XX  XX      ");
    console.write_line("       XXXX       ");
    console.write_line("      XX  XX      ");
    console.write_line("     XX    XX     ");
    console.write_line("    XX      XX    ");
    console.write_line("   XX        XX   ");
    console.write_line("  XX          XX  ");
    console.write_line(" XX            XX ");
    console.write_line("XX              XX");
    console.write_line("");
    console.write_line("XAVIEROS");
    console.write_line("A SMALL OS. BUILT FROM SCRATCH IN RUST.");
    console.write_line("VERSION 0.1.2 | NO DESKTOP. NO SHORTCUTS.");
    console.write_line("");
}

fn calculate(console: &mut Console, arguments: &str) {
    let mut parts = arguments.split_whitespace();
    let left = parts.next().and_then(|value| value.parse::<i64>().ok());
    let operator = parts.next();
    let right = parts.next().and_then(|value| value.parse::<i64>().ok());
    if parts.next().is_some() || left.is_none() || right.is_none() || operator.is_none() {
        console.write_line("USAGE: CALC NUMBER OP NUMBER");
        console.write_line("EXAMPLE: CALC 12 * 4");
        return;
    }

    let left = left.unwrap_or(0);
    let right = right.unwrap_or(0);
    let result = match operator.unwrap_or("") {
        "+" => left.checked_add(right),
        "-" => left.checked_sub(right),
        "*" => left.checked_mul(right),
        "/" if right != 0 => left.checked_div(right),
        "/" => {
            console.write_line("ERROR: DIVISION BY ZERO.");
            return;
        }
        _ => {
            console.write_line("ERROR: SUPPORTED OPERATORS ARE + - * /");
            return;
        }
    };

    match result {
        Some(value) => {
            console.write("RESULT: ");
            if value < 0 {
                console.write("-");
                console.write_decimal(value.unsigned_abs());
            } else {
                console.write_decimal(value as u64);
            }
            console.write_line("");
        }
        None => console.write_line("ERROR: INTEGER OVERFLOW."),
    }
}

fn show_cpu(console: &mut Console) {
    let cpu = detect_cpu();
    console.write("CPU VENDOR: ");
    console.write_line(core::str::from_utf8(&cpu.vendor).unwrap_or("UNKNOWN"));
    console.write("CPU MODEL: ");
    if cpu.brand_len > 0 {
        console.write_line(core::str::from_utf8(&cpu.brand[..cpu.brand_len]).unwrap_or("UNKNOWN"));
    } else {
        console.write_line("NOT REPORTED BY CPUID");
    }
    console.write_line("NOTE: THIS IS THE CPU MODEL EXPOSED TO XAVIEROS.");
}

fn show_pmm(console: &mut Console, memory_summary: Option<(MemoryStats, bool)>) {
    match memory_summary {
        Some((stats, pmm_ready)) => {
            let free_pages = memory::free_page_count() as u64;
            console.write_line("PHYSICAL MEMORY MANAGER");
            console.write_line(if pmm_ready { "STATUS: READY" } else { "STATUS: SELF-TEST FAILED" });
            console.write("PAGE SIZE: ");
            console.write_decimal(4096);
            console.write_line(" BYTES");
            console.write("MANAGED PAGES: ");
            console.write_decimal(stats.managed_pages as u64);
            console.write_line("");
            console.write("FREE PAGES: ");
            console.write_decimal(free_pages);
            console.write_line("");
            console.write("FREE MANAGED MEMORY: ");
            console.write_decimal((free_pages * 4096) / (1024 * 1024));
            console.write_line(" MIB");
            console.write_line("RANGE: PHYSICAL ADDRESSES BELOW 4 GIB");
            console.write_line("MODE: SINGLE-CORE BITMAP ALLOCATOR");
        }
        None => console.write_line("PMM IS NOT INITIALIZED."),
    }
}

fn show_hhdm(console: &mut Console, memory_summary: Option<(MemoryStats, bool)>) {
    match memory_summary {
        Some((stats, _)) if stats.hhdm_offset != 0 => {
            console.write_line("HIGHER HALF DIRECT MAP");
            console.write_line("STATUS: READY");
            console.write("OFFSET: 0X");
            console.write_hex(stats.hhdm_offset);
            console.write_line("");
            console.write_line("PHYSICAL ADDRESS + OFFSET = VIRTUAL ADDRESS");
            console.write_line("TRANSLATION IS CHECKED FOR OVERFLOW.");
        }
        _ => {
            console.write_line("HHDM IS NOT AVAILABLE.");
            console.write_line("CHECK LIMINE MEMORY-MAP INITIALIZATION.");
        }
    }
}

fn show_heap(console: &mut Console) {
    console.write_line("XAVIEROS KERNEL HEAP");
    match heap::stats() {
        Some(stats) => {
            console.write_line("STATUS: READY");
            console.write("CAPACITY: ");
            console.write_decimal((stats.capacity_bytes / 1024) as u64);
            console.write_line(" KIB");
            console.write("FREE: ");
            console.write_decimal(stats.free_bytes as u64);
            console.write_line(" BYTES");
            console.write("USED / RESERVED: ");
            console.write_decimal(stats.capacity_bytes.saturating_sub(stats.free_bytes) as u64);
            console.write_line(" BYTES");
            console.write_line("BACKING: 256 CONTIGUOUS PMM PAGES");
            console.write_line("ALLOCATOR: FIRST-FIT FREE LIST");
        }
        None => console.write_line("STATUS: UNAVAILABLE (CHECK PMM / HHDM)"),
    }
}

fn show_idt(console: &mut Console) {
    console.write_line("XAVIEROS INTERRUPT DESCRIPTOR TABLE");
    console.write_line(if interrupts::is_initialized() { "STATUS: READY" } else { "STATUS: NOT INITIALIZED" });
    console.write_line("GATES: 256 PRESENT");
    console.write_line("CPU EXCEPTIONS: VECTORS 0-31");
    console.write_line("PIT TIMER: VECTOR 32 / IRQ0");
    console.write_line("PS/2 KEYBOARD: VECTOR 33 / IRQ1");
    console.write_line("PAGE FAULT: CR2 ADDRESS + REGISTER DUMP");
    console.write_line(if interrupts::hardware_is_initialized() { "LEGACY PIC: REMAPPED / IRQ0 ENABLED" } else { "LEGACY PIC: NOT INITIALIZED" });
    console.write_line(if interrupts::keyboard_is_initialized() { "PS/2 KEYBOARD IRQ1: READY" } else { "PS/2 KEYBOARD IRQ1: UNAVAILABLE" });
}

fn show_time(console: &mut Console) {
    let ticks = interrupts::timer_ticks();
    console.write_line("XAVIEROS PIT CLOCK");
    console.write("TIMER FREQUENCY: 100 HZ | TICKS: ");
    console.write_decimal(ticks);
    console.write_line("");
    console.write("UPTIME: ");
    console.write_decimal(ticks / 100);
    console.write(".");
    let fractional = ticks % 100;
    if fractional < 10 { console.write("0"); }
    console.write_decimal(fractional);
    console.write_line(" SECONDS (APPROXIMATE)");
}

fn show_irq(console: &mut Console) {
    console.write_line("XAVIEROS HARDWARE INTERRUPTS");
    console.write_line(if interrupts::hardware_is_initialized() { "PIC: READY (MASTER 0X20 / SLAVE 0X28)" } else { "PIC: NOT INITIALIZED" });
    console.write_line("PIT IRQ0: ENABLED");
    console.write_line(if interrupts::keyboard_is_initialized() { "PS/2 KEYBOARD IRQ1: ENABLED" } else { "PS/2 KEYBOARD IRQ1: MASKED / SETUP FAILED" });
    console.write("PIT TICKS: ");
    console.write_decimal(interrupts::timer_ticks());
    console.write_line("");
    console.write("DROPPED KEYBOARD SCANCODES: ");
    console.write_decimal(interrupts::keyboard_dropped_scancodes());
    console.write_line("");
}

fn run_diagnostics(console: &mut Console, memory_summary: Option<(MemoryStats, bool)>) {
    let framebuffer_width = console.framebuffer.width;
    let framebuffer_height = console.framebuffer.height;
    console.write_line("XAVIEROS SYSTEM DIAGNOSTICS");
    console.write_line("KERNEL: RUNNING");
    console.write("FRAMEBUFFER: ");
    console.write_decimal(framebuffer_width);
    console.write(" X ");
    console.write_decimal(framebuffer_height);
    console.write_line(" @ 32 BPP");
    console.write_line("INPUT: PS/2 KEYBOARD IRQ + RING BUFFER");
    console.write_line(if ata::is_ready() { "ATA PIO PRIMARY MASTER: READY" } else { "ATA PIO DISK: UNAVAILABLE" });
    console.write_line(if diskfs::mount_status() { "XVDISKFS MOUNT: /DISK READY" } else { "XVDISKFS MOUNT: UNAVAILABLE" });
    let (task_total, task_runnable, context_switches) = task::stats();
    console.write("COOPERATIVE TASKS: "); console.write_decimal(task_total as u64);
    console.write(" | RUNNABLE: "); console.write_decimal(task_runnable as u64);
    console.write(" | SWITCHES: "); console.write_decimal(context_switches); console.write_line("");
    console.write_line(if interrupts::hardware_is_initialized() { "PIC / PIT: READY" } else { "PIC / PIT: UNAVAILABLE" });
    console.write_line("TIMER: 100 HZ PIT");
    console.write_line("BOOTLOADER: LIMINE");
    console.write_line(if interrupts::is_initialized() { "IDT: READY (CPU EXCEPTIONS INSTALLED)" } else { "IDT: UNAVAILABLE" });
    console.write_line(if heap::is_initialized() { "KERNEL HEAP: READY" } else { "KERNEL HEAP: UNAVAILABLE" });
    match memory_summary {
        Some((stats, pmm_ready)) => {
            console.write_line(if stats.hhdm_offset != 0 { "HHDM: READY" } else { "HHDM: FAILED" });
            console.write_line(if pmm_ready { "PMM INIT SELF-TEST: PASS" } else { "PMM INIT SELF-TEST: FAIL" });
            if pmm_ready {
                match memory::self_test() {
                    Ok(()) => console.write_line("PMM LIVE SELF-TEST: PASS"),
                    Err(_) => console.write_line("PMM LIVE SELF-TEST: FAIL"),
                }
            } else {
                console.write_line("PMM LIVE SELF-TEST: SKIPPED (INIT TEST FAILED)");
            }
        }
        None => console.write_line("MEMORY SUBSYSTEM: UNAVAILABLE"),
    }
    console.write_line("DIAGNOSTICS COMPLETE.");
}

#[derive(Clone, Copy)]
struct CpuInfo {
    vendor: [u8; 12],
    brand: [u8; 48],
    brand_len: usize,
}

fn detect_cpu() -> CpuInfo {
    use core::arch::x86_64::__cpuid;

    let mut info = CpuInfo {
        vendor: [b' '; 12],
        brand: [b' '; 48],
        brand_len: 0,
    };

    // CPUID is available on x86_64 CPUs. The brand string, when supplied by the
    // processor or hypervisor, describes the CPU model exposed to this guest OS.
    unsafe {
        let basic = __cpuid(0);
        info.vendor[0..4].copy_from_slice(&basic.ebx.to_le_bytes());
        info.vendor[4..8].copy_from_slice(&basic.edx.to_le_bytes());
        info.vendor[8..12].copy_from_slice(&basic.ecx.to_le_bytes());

        let max_extended = __cpuid(0x8000_0000).eax;
        if max_extended >= 0x8000_0004 {
            let mut offset = 0usize;
            for leaf in 0x8000_0002..=0x8000_0004 {
                let result = __cpuid(leaf);
                for register in [result.eax, result.ebx, result.ecx, result.edx] {
                    let bytes = register.to_le_bytes();
                    info.brand[offset..offset + 4].copy_from_slice(&bytes);
                    offset += 4;
                }
            }
            info.brand_len = 48;
            while info.brand_len > 0
                && (info.brand[info.brand_len - 1] == 0 || info.brand[info.brand_len - 1] == b' ')
            {
                info.brand_len -= 1;
            }
            for byte in &mut info.brand[..info.brand_len] {
                if !byte.is_ascii_graphic() && *byte != b' ' {
                    *byte = b'?';
                }
            }
        }
    }
    info
}

fn show_sysinfo(console: &mut Console, memory_summary: Option<(MemoryStats, bool)>) {
    let cpu = detect_cpu();
    let vendor = core::str::from_utf8(&cpu.vendor).unwrap_or("UNKNOWN");
    let cpu_brand = if cpu.brand_len > 0 {
        core::str::from_utf8(&cpu.brand[..cpu.brand_len]).unwrap_or("UNKNOWN CPU")
    } else {
        "UNKNOWN CPU MODEL"
    };

    console.write_line("");

    // At typical 1024x768 QEMU resolution, keep an X-shaped logo on the left and
    // aligned facts on the right. On narrower framebuffers, stack the info below it.
    if console.max_cols >= 74 {
        sysinfo_row(console, "XX              XX", "XAVIEROS V0.1.2");
        sysinfo_row(console, " XX            XX ", "----------------");
        sysinfo_row(console, "  XX          XX  ", "OS: XAVIEROS V0.1.2");
        sysinfo_row(console, "   XX        XX   ", "KERNEL: RUST NO_STD");
        sysinfo_row(console, "    XX      XX    ", "ARCH: X86_64");
        sysinfo_row(console, "     XX    XX     ", "BOOTLOADER: LIMINE");

        sysinfo_prefix(console, "      XX  XX      ");
        console.write("CPU VENDOR: ");
        console.write_line(vendor);

        sysinfo_prefix(console, "       XXXX       ");
        console.write("CPU: ");
        console.write_line(cpu_brand);

        match memory_summary {
            Some((stats, pmm_ready)) => {
                sysinfo_prefix(console, "      XX  XX      ");
                console.write("RAM: ");
                console.write_decimal(stats.total_usable_bytes / (1024 * 1024));
                console.write_line(" MIB USABLE");

                sysinfo_prefix(console, "     XX    XX     ");
                console.write("PMM MANAGED: ");
                console.write_decimal(stats.managed_pages as u64);
                console.write_line(" PAGES");

                sysinfo_prefix(console, "    XX      XX    ");
                console.write("PMM FREE: ");
                console.write_decimal(memory::free_page_count() as u64);
                console.write_line(" PAGES");

                sysinfo_row(console, "   XX        XX   ", if stats.hhdm_offset != 0 { "HHDM: READY" } else { "HHDM: UNAVAILABLE" });
                sysinfo_row(console, "  XX          XX  ", if pmm_ready { "PMM SELF-TEST: PASS" } else { "PMM SELF-TEST: FAIL" });
            }
            None => {
                sysinfo_row(console, "      XX  XX      ", "RAM: MEMORY MAP UNAVAILABLE");
                sysinfo_row(console, "     XX    XX     ", "PMM: UNAVAILABLE");
                sysinfo_row(console, "    XX      XX    ", "HHDM: UNAVAILABLE");
                sysinfo_row(console, "   XX        XX   ", "PMM SELF-TEST: NOT RUN");
            }
        }
        sysinfo_row(console, " XX            XX ", if diskfs::mount_status() { "DISK: XVDISKFS READY" } else { "DISK: UNAVAILABLE" });
        sysinfo_row(console, "XX              XX", "SCHEDULER: COOPERATIVE");
    } else {
        console.write_line("      XX  XX");
        console.write_line("     XX    XX");
        console.write_line("    XX      XX");
        console.write_line("   XX        XX");
        console.write_line("  XX          XX");
        console.write_line(" XX            XX");
        console.write_line("XX              XX");
        console.write_line("XAVIEROS V0.1.2");
        console.write_line("----------------");
        console.write_line("KERNEL: RUST NO_STD");
        console.write_line("ARCH: X86_64");
        console.write_line("BOOTLOADER: LIMINE");
        console.write_line(if diskfs::mount_status() { "DISK: XVDISKFS READY" } else { "DISK: UNAVAILABLE" });
        console.write_line("SCHEDULER: COOPERATIVE");
        console.write("CPU VENDOR: ");
        console.write_line(vendor);
        console.write("CPU: ");
        let max_cpu_chars = console.max_cols.saturating_sub(5);
        console.write_line(&cpu_brand[..cpu_brand.len().min(max_cpu_chars)]);
        match memory_summary {
            Some((stats, pmm_ready)) => {
                console.write("RAM: ");
                console.write_decimal(stats.total_usable_bytes / (1024 * 1024));
                console.write_line(" MIB USABLE");
                console.write("PMM FREE: ");
                console.write_decimal(memory::free_page_count() as u64);
                console.write_line(" PAGES");
                console.write_line(if pmm_ready { "PMM SELF-TEST: PASS" } else { "PMM SELF-TEST: FAIL" });
            }
            None => console.write_line("MEMORY / PMM: UNAVAILABLE"),
        }
    }
    console.write_line("");
}

fn sysinfo_prefix(console: &mut Console, logo: &str) {
    console.write(logo);
    console.write("   ");
}

fn sysinfo_row(console: &mut Console, logo: &str, info: &str) {
    sysinfo_prefix(console, logo);
    console.write_line(info);
}

fn reboot_machine() -> ! {
    serial_write("Reboot requested by shell.\r\n");
    // The 8042 keyboard controller reset command is supported by QEMU's PC chipset.
    for _ in 0..100_000 {
        if unsafe { inb(0x64) } & 0x02 == 0 {
            unsafe { outb(0x64, 0xFE); }
            break;
        }
        core::hint::spin_loop();
    }
    halt_forever()
}

fn rgb(fb: &Framebuffer, red: u8, green: u8, blue: u8) -> u32 {
    channel(red, fb.red_mask_size, fb.red_mask_shift)
        | channel(green, fb.green_mask_size, fb.green_mask_shift)
        | channel(blue, fb.blue_mask_size, fb.blue_mask_shift)
}

fn channel(value: u8, size: u8, shift: u8) -> u32 {
    if size == 0 || size > 8 || shift >= 32 {
        return 0;
    }
    let max = (1u32 << size) - 1;
    (((value as u32 * max + 127) / 255) << shift) & (max << shift)
}

fn fill_rect(fb: &Framebuffer, x: usize, y: usize, w: usize, h: usize, color: u32) {
    if x >= fb.width as usize || y >= fb.height as usize || w == 0 || h == 0 {
        return;
    }
    let end_x = x.saturating_add(w).min(fb.width as usize);
    let end_y = y.saturating_add(h).min(fb.height as usize);
    for py in y..end_y {
        let row = unsafe { fb.address.add(py * fb.pitch as usize) as *mut u32 };
        for px in x..end_x {
            unsafe { write_volatile(row.add(px), color); }
        }
    }
}

fn draw_text(fb: &Framebuffer, x: usize, y: usize, text: &str, scale: usize, color: u32) {
    if scale == 0 { return; }
    for (char_index, byte) in text.bytes().enumerate() {
        let rows = glyph(byte.to_ascii_uppercase());
        for (row_index, bits) in rows.iter().enumerate() {
            for col in 0..5 {
                if bits & (1 << (4 - col)) == 0 { continue; }
                let px = x.saturating_add((char_index * 6 + col) * scale);
                let py = y.saturating_add(row_index * scale);
                fill_rect(fb, px, py, scale, scale, color);
            }
        }
    }
}

// Compact 5x7 bitmap font for the ASCII characters used by the framebuffer console.
fn glyph(c: u8) -> [u8; 7] {
    match c {
        b'A' => [14, 17, 17, 31, 17, 17, 17],
        b'B' => [30, 17, 17, 30, 17, 17, 30],
        b'C' => [14, 17, 16, 16, 16, 17, 14],
        b'D' => [30, 17, 17, 17, 17, 17, 30],
        b'E' => [31, 16, 16, 30, 16, 16, 31],
        b'F' => [31, 16, 16, 30, 16, 16, 16],
        b'G' => [14, 17, 16, 23, 17, 17, 15],
        b'H' => [17, 17, 17, 31, 17, 17, 17],
        b'I' => [14, 4, 4, 4, 4, 4, 14],
        b'J' => [7, 2, 2, 2, 2, 18, 12],
        b'K' => [17, 18, 20, 24, 20, 18, 17],
        b'L' => [16, 16, 16, 16, 16, 16, 31],
        b'M' => [17, 27, 21, 21, 17, 17, 17],
        b'N' => [17, 25, 21, 19, 17, 17, 17],
        b'O' => [14, 17, 17, 17, 17, 17, 14],
        b'P' => [30, 17, 17, 30, 16, 16, 16],
        b'Q' => [14, 17, 17, 17, 21, 18, 13],
        b'R' => [30, 17, 17, 30, 20, 18, 17],
        b'S' => [15, 16, 16, 14, 1, 1, 30],
        b'T' => [31, 4, 4, 4, 4, 4, 4],
        b'U' => [17, 17, 17, 17, 17, 17, 14],
        b'V' => [17, 17, 17, 17, 17, 10, 4],
        b'W' => [17, 17, 17, 21, 21, 21, 10],
        b'X' => [17, 17, 10, 4, 10, 17, 17],
        b'Y' => [17, 17, 10, 4, 4, 4, 4],
        b'Z' => [31, 1, 2, 4, 8, 16, 31],
        b'0' => [14, 17, 19, 21, 25, 17, 14],
        b'1' => [4, 12, 4, 4, 4, 4, 14],
        b'2' => [14, 17, 1, 2, 4, 8, 31],
        b'3' => [30, 1, 1, 14, 1, 1, 30],
        b'4' => [2, 6, 10, 18, 31, 2, 2],
        b'5' => [31, 16, 16, 30, 1, 1, 30],
        b'6' => [14, 16, 16, 30, 17, 17, 14],
        b'7' => [31, 1, 2, 4, 8, 8, 8],
        b'8' => [14, 17, 17, 14, 17, 17, 14],
        b'9' => [14, 17, 17, 15, 1, 1, 14],
        b':' => [0, 4, 4, 0, 4, 4, 0],
        b'-' => [0, 0, 0, 31, 0, 0, 0],
        b'_' => [0, 0, 0, 0, 0, 0, 31],
        b'/' => [1, 2, 2, 4, 8, 8, 16],
        b'.' => [0, 0, 0, 0, 0, 12, 12],
        b',' => [0, 0, 0, 0, 0, 4, 8],
        b'!' => [4, 4, 4, 4, 4, 0, 4],
        b'?' => [14, 17, 1, 2, 4, 0, 4],
        b'>' => [16, 8, 4, 8, 16, 0, 0],
        b'<' => [2, 4, 8, 4, 2, 0, 0],
        b'=' => [0, 31, 0, 31, 0, 0, 0],
        b'+' => [0, 4, 4, 31, 4, 4, 0],
        b'*' => [0, 21, 14, 31, 14, 21, 0],
        b'(' => [2, 4, 8, 8, 8, 4, 2],
        b')' => [8, 4, 2, 2, 2, 4, 8],
        b'[' => [14, 8, 8, 8, 8, 8, 14],
        b']' => [14, 2, 2, 2, 2, 2, 14],
        b'|' => [4, 4, 4, 4, 4, 4, 4],
        b';' => [0, 4, 4, 0, 4, 4, 8],
        b'\\' => [16, 8, 8, 4, 2, 2, 1],
        b'\'' => [4, 4, 2, 0, 0, 0, 0],
        b' ' => [0; 7],
        _ => [0; 7],
    }
}


fn decimal_text<'a>(mut value: u64, buffer: &'a mut [u8; 20]) -> &'a str {
    let mut cursor = buffer.len();
    loop {
        cursor -= 1;
        buffer[cursor] = b'0' + (value % 10) as u8;
        value /= 10;
        if value == 0 {
            break;
        }
    }
    // The buffer contains only ASCII decimal digits.
    core::str::from_utf8(&buffer[cursor..]).unwrap_or("0")
}

fn serial_write_u64(value: u64) {
    let mut buffer = [0u8; 20];
    serial_write(decimal_text(value, &mut buffer));
}

fn serial_write_u64_hex(value: u64) {
    const HEX: &[u8; 16] = b"0123456789ABCDEF";
    for shift in (0..16).rev() {
        let digit = ((value >> (shift * 4)) & 0xF) as usize;
        serial_write_byte(HEX[digit]);
    }
}

fn serial_init() {
    unsafe {
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x80);
        outb(COM1, 0x01);
        outb(COM1 + 1, 0x00);
        outb(COM1 + 3, 0x03);
        outb(COM1 + 2, 0xC7);
        outb(COM1 + 4, 0x0B);
    }
}

fn serial_write(text: &str) {
    for byte in text.bytes() { serial_write_byte(byte); }
}

fn serial_write_byte(byte: u8) {
    for _ in 0..100_000 {
        if unsafe { inb(COM1 + 5) & 0x20 != 0 } {
            unsafe { outb(COM1, byte); }
            return;
        }
        core::hint::spin_loop();
    }
}

unsafe fn outb(port: u16, value: u8) {
    unsafe {
        asm!("out dx, al", in("dx") port, in("al") value,
             options(nomem, nostack, preserves_flags));
    }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe {
        asm!("in al, dx", in("dx") port, out("al") value,
             options(nomem, nostack, preserves_flags));
    }
    value
}

#[panic_handler]
fn panic(_info: &PanicInfo) -> ! {
    serial_write("\r\nKERNEL PANIC\r\n");
    halt_forever()
}
