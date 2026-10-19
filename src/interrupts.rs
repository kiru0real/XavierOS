//! x86_64 IDT, fatal CPU exception diagnostics, legacy PIC/PIT, and IRQ dispatch.
//!
//! The current hardware path targets the legacy 8259 PIC + PIT + PS/2 keyboard.
//! IRQ handlers only update atomics or enqueue scancodes; they must not allocate,
//! block, or draw into the framebuffer.

use core::mem::size_of;
use core::ptr::{addr_of, addr_of_mut, write_volatile};
use core::sync::atomic::{AtomicBool, AtomicU8, AtomicU64, AtomicUsize, Ordering};

static INITIALIZED: AtomicBool = AtomicBool::new(false);
static HARDWARE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static KEYBOARD_INITIALIZED: AtomicBool = AtomicBool::new(false);
static TIMER_TICKS: AtomicU64 = AtomicU64::new(0);
static KEYBOARD_HEAD: AtomicUsize = AtomicUsize::new(0);
static KEYBOARD_TAIL: AtomicUsize = AtomicUsize::new(0);
static KEYBOARD_DROPPED: AtomicU64 = AtomicU64::new(0);
static MOUSE_INITIALIZED: AtomicBool = AtomicBool::new(false);
static MOUSE_PACKET_INDEX: AtomicU8 = AtomicU8::new(0);
static MOUSE_PACKET: [AtomicU8; 3] = [const { AtomicU8::new(0) }; 3];
const KEYBOARD_QUEUE_CAPACITY: usize = 256;
static KEYBOARD_QUEUE: [AtomicU8; KEYBOARD_QUEUE_CAPACITY] =
    [const { AtomicU8::new(0) }; KEYBOARD_QUEUE_CAPACITY];

#[repr(C)]
#[derive(Clone, Copy)]
struct IdtEntry {
    offset_low: u16,
    selector: u16,
    ist: u8,
    attributes: u8,
    offset_mid: u16,
    offset_high: u32,
    reserved: u32,
}

impl IdtEntry {
    const MISSING: Self = Self {
        offset_low: 0,
        selector: 0,
        ist: 0,
        attributes: 0,
        offset_mid: 0,
        offset_high: 0,
        reserved: 0,
    };

    fn interrupt_gate(handler: u64, selector: u16) -> Self {
        Self {
            offset_low: handler as u16,
            selector,
            ist: 0,
            attributes: 0x8E, // Present, ring 0, 64-bit interrupt gate.
            offset_mid: (handler >> 16) as u16,
            offset_high: (handler >> 32) as u32,
            reserved: 0,
        }
    }
}

#[repr(C, packed)]
struct Idtr {
    limit: u16,
    base: u64,
}

#[repr(C)]
struct InterruptFrame {
    r15: u64,
    r14: u64,
    r13: u64,
    r12: u64,
    r11: u64,
    r10: u64,
    r9: u64,
    r8: u64,
    rbp: u64,
    rdi: u64,
    rsi: u64,
    rdx: u64,
    rcx: u64,
    rbx: u64,
    rax: u64,
    vector: u64,
    error_code: u64,
    rip: u64,
    cs: u64,
    rflags: u64,
}

static mut IDT: [IdtEntry; 256] = [IdtEntry::MISSING; 256];

unsafe extern "C" {
    fn isr_default();
    fn isr0(); fn isr1(); fn isr2(); fn isr3(); fn isr4(); fn isr5(); fn isr6(); fn isr7();
    fn isr8(); fn isr9(); fn isr10(); fn isr11(); fn isr12(); fn isr13(); fn isr14(); fn isr15();
    fn isr16(); fn isr17(); fn isr18(); fn isr19(); fn isr20(); fn isr21(); fn isr22(); fn isr23();
    fn isr24(); fn isr25(); fn isr26(); fn isr27(); fn isr28(); fn isr29(); fn isr30(); fn isr31();
    fn irq0(); fn irq1();
}

/// Install all IDT entries. CPU interrupts remain disabled until the console is ready.
pub fn initialize() {
    unsafe { core::arch::asm!("cli", options(nomem, nostack)); }

    let selector: u16;
    unsafe {
        core::arch::asm!("mov {0:x}, cs", out(reg) selector, options(nomem, nostack, preserves_flags));
    }

    let default_handler = isr_default as usize as u64;
    let handlers: [u64; 32] = [
        isr0 as usize as u64, isr1 as usize as u64, isr2 as usize as u64, isr3 as usize as u64,
        isr4 as usize as u64, isr5 as usize as u64, isr6 as usize as u64, isr7 as usize as u64,
        isr8 as usize as u64, isr9 as usize as u64, isr10 as usize as u64, isr11 as usize as u64,
        isr12 as usize as u64, isr13 as usize as u64, isr14 as usize as u64, isr15 as usize as u64,
        isr16 as usize as u64, isr17 as usize as u64, isr18 as usize as u64, isr19 as usize as u64,
        isr20 as usize as u64, isr21 as usize as u64, isr22 as usize as u64, isr23 as usize as u64,
        isr24 as usize as u64, isr25 as usize as u64, isr26 as usize as u64, isr27 as usize as u64,
        isr28 as usize as u64, isr29 as usize as u64, isr30 as usize as u64, isr31 as usize as u64,
    ];

    let idt_ptr = addr_of_mut!(IDT).cast::<IdtEntry>();
    for vector in 0..256 {
        unsafe {
            write_volatile(
                idt_ptr.add(vector),
                IdtEntry::interrupt_gate(default_handler, selector),
            );
        }
    }
    for (vector, handler) in handlers.iter().copied().enumerate() {
        unsafe {
            write_volatile(
                idt_ptr.add(vector),
                IdtEntry::interrupt_gate(handler, selector),
            );
        }
    }
    // Legacy PIC IRQ0 (PIT timer) and IRQ1 (PS/2 keyboard).
    unsafe {
        write_volatile(idt_ptr.add(32), IdtEntry::interrupt_gate(irq0 as usize as u64, selector));
        write_volatile(idt_ptr.add(33), IdtEntry::interrupt_gate(irq1 as usize as u64, selector));
    }

    let idtr = Idtr {
        limit: (size_of::<[IdtEntry; 256]>() - 1) as u16,
        base: addr_of!(IDT) as u64,
    };
    unsafe {
        core::arch::asm!("lidt [{}]", in(reg) &idtr, options(readonly, nostack, preserves_flags));
    }
    INITIALIZED.store(true, Ordering::Release);
    crate::serial_write("IDT initialized: CPU exceptions plus IRQ0/IRQ1 gates ready.\r\n");
}


/// Remap the legacy PIC, enable IRQ0/IRQ1, and program the PIT at 100 Hz.
/// CPU interrupts remain disabled until the console is ready to consume input.
pub fn initialize_hardware() {
    unsafe {
        // Mask both PICs while they are remapped.
        outb(0x21, 0xFF);
        outb(0xA1, 0xFF);

        // ICW1: initialize + expect ICW4.
        outb(0x20, 0x11); io_wait();
        outb(0xA0, 0x11); io_wait();
        // ICW2: remap master to 0x20..0x27 and slave to 0x28..0x2F.
        outb(0x21, 0x20); io_wait();
        outb(0xA1, 0x28); io_wait();
        // ICW3: slave on master IRQ2; slave identity 2.
        outb(0x21, 0x04); io_wait();
        outb(0xA1, 0x02); io_wait();
        // ICW4: 8086 mode.
        outb(0x21, 0x01); io_wait();
        outb(0xA1, 0x01); io_wait();
        // Keep all IRQs masked until the PIT and keyboard controller are ready.
        outb(0x21, 0xFF);
        outb(0xA1, 0xFF);

        // PIT channel 0, lo/hi byte access, mode 3, binary counter.
        let divisor: u16 = (1_193_182u32 / 100) as u16;
        outb(0x43, 0x36);
        outb(0x40, divisor as u8);
        outb(0x40, (divisor >> 8) as u8);
    }

    let keyboard_ready = initialize_ps2_keyboard();
    KEYBOARD_INITIALIZED.store(keyboard_ready, Ordering::Release);
    unsafe {
        // Always enable PIT IRQ0; only enable IRQ1 if the PS/2 controller setup succeeded.
        outb(0x21, if keyboard_ready { 0xFC } else { 0xFE });
        outb(0xA1, 0xFF);
    }
    HARDWARE_INITIALIZED.store(true, Ordering::Release);
    crate::serial_write("PIC remapped; PIT configured for 100 Hz.\r\n");
    crate::serial_write(if keyboard_ready {
        "PS/2 controller configured for IRQ1 keyboard input.\r\n"
    } else {
        "WARNING: PS/2 controller setup failed; keyboard IRQ1 remains masked.\r\n"
    });
}

pub fn hardware_is_initialized() -> bool {
    HARDWARE_INITIALIZED.load(Ordering::Acquire)
}

pub fn keyboard_is_initialized() -> bool {
    KEYBOARD_INITIALIZED.load(Ordering::Acquire)
}

/// Enable the first 8042/PS/2 port and the controller's IRQ1 output. Every wait
/// is bounded so broken or absent legacy hardware cannot hang kernel startup.
fn initialize_ps2_keyboard() -> bool {
    unsafe {
        // Drain stale controller output before asking for its configuration byte.
        for _ in 0..100_000 {
            if inb(0x64) & 0x01 == 0 { break; }
            let _ = inb(0x60);
        }
        if !wait_ps2_input_empty() { return false; }
        outb(0x64, 0x20); // Read 8042 configuration byte.
        if !wait_ps2_output_full() { return false; }
        let mut config = inb(0x60);

        // Enable IRQ1 and the first PS/2 port; preserve the firmware's scancode
        // translation setting because the shell currently decodes Set 1 scancodes.
        config |= 0x01;
        config &= !0x10;
        if !wait_ps2_input_empty() { return false; }
        outb(0x64, 0x60); // Write configuration byte.
        if !wait_ps2_input_empty() { return false; }
        outb(0x60, config);
        if !wait_ps2_input_empty() { return false; }
        outb(0x64, 0xAE); // Enable first PS/2 port.
        wait_ps2_input_empty()
    }
}

unsafe fn wait_ps2_input_empty() -> bool {
    for _ in 0..1_000_000 {
        if unsafe { inb(0x64) } & 0x02 == 0 { return true; }
        core::hint::spin_loop();
    }
    false
}

unsafe fn wait_ps2_output_full() -> bool {
    for _ in 0..1_000_000 {
        if unsafe { inb(0x64) } & 0x01 != 0 { return true; }
        core::hint::spin_loop();
    }
    false
}


#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct MousePacket {
    /// Signed horizontal motion since the previous packet.
    pub dx: i16,
    /// Signed PS/2 vertical motion; positive values mean upward motion.
    pub dy: i16,
    /// Button bitfield: bit 0 left, bit 1 right, bit 2 middle.
    pub buttons: u8,
}

/// Initialize a PS/2 auxiliary mouse in polling mode. IRQ12 remains masked, so
/// mouse packets cannot be mistaken for keyboard scancodes by the IRQ dispatcher.
pub fn initialize_ps2_mouse() -> bool {
    unsafe {
        // Drain stale output while global interrupts are still disabled during boot.
        for _ in 0..100_000 {
            let status = inb(0x64);
            if status & 0x01 == 0 { break; }
            let _ = inb(0x60);
        }
        if !wait_ps2_input_empty() { return false; }
        outb(0x64, 0xA8); // Enable auxiliary PS/2 port.
        if !wait_ps2_input_empty() { return false; }
        outb(0x64, 0x20); // Read controller configuration.
        if !wait_ps2_output_full() { return false; }
        let mut config = inb(0x60);
        config &= !0x20; // Enable auxiliary clock.
        config &= !0x02; // Keep IRQ12 disabled; desktop polls AUX output bytes.
        if !wait_ps2_input_empty() { return false; }
        outb(0x64, 0x60);
        if !wait_ps2_input_empty() { return false; }
        outb(0x60, config);
    }
    if !send_mouse_command(0xF6) || !send_mouse_command(0xF4) {
        MOUSE_INITIALIZED.store(false, Ordering::Release);
        return false;
    }
    MOUSE_PACKET_INDEX.store(0, Ordering::Relaxed);
    MOUSE_INITIALIZED.store(true, Ordering::Release);
    true
}

pub fn mouse_is_initialized() -> bool {
    MOUSE_INITIALIZED.load(Ordering::Acquire)
}

/// Poll at most 8 AUX bytes and return a complete 3-byte standard PS/2 packet.
/// Keyboard data is left to IRQ1 and is never read by this function.
pub fn poll_mouse_packet() -> Option<MousePacket> {
    if !MOUSE_INITIALIZED.load(Ordering::Acquire) { return None; }
    for _ in 0..8 {
        let status = unsafe { inb(0x64) };
        if status & 0x01 == 0 || status & 0x20 == 0 { return None; }
        let byte = unsafe { inb(0x60) };
        let index = MOUSE_PACKET_INDEX.load(Ordering::Relaxed) as usize;
        if index == 0 {
            // Standard PS/2 packet headers always have bit 3 set; use it for resync.
            if byte & 0x08 == 0 { continue; }
            MOUSE_PACKET[0].store(byte, Ordering::Relaxed);
            MOUSE_PACKET_INDEX.store(1, Ordering::Relaxed);
            continue;
        }
        MOUSE_PACKET[index].store(byte, Ordering::Relaxed);
        if index < 2 {
            MOUSE_PACKET_INDEX.store((index + 1) as u8, Ordering::Relaxed);
            continue;
        }
        MOUSE_PACKET_INDEX.store(0, Ordering::Relaxed);
        let header = MOUSE_PACKET[0].load(Ordering::Relaxed);
        let raw_x = MOUSE_PACKET[1].load(Ordering::Relaxed) as i8 as i16;
        let raw_y = MOUSE_PACKET[2].load(Ordering::Relaxed) as i8 as i16;
        let overflow = header & 0xC0 != 0;
        return Some(MousePacket {
            dx: if overflow { 0 } else { raw_x },
            dy: if overflow { 0 } else { raw_y },
            buttons: header & 0x07,
        });
    }
    None
}

fn send_mouse_command(command: u8) -> bool {
    unsafe {
        if !wait_ps2_input_empty() { return false; }
        outb(0x64, 0xD4); // Next byte is for the auxiliary device.
        if !wait_ps2_input_empty() { return false; }
        outb(0x60, command);
        for _ in 0..1_000_000 {
            let status = inb(0x64);
            if status & 0x01 != 0 {
                let response = inb(0x60);
                if status & 0x20 != 0 { return response == 0xFA; }
            }
            core::hint::spin_loop();
        }
    }
    false
}

pub fn enable_interrupts() {
    unsafe { core::arch::asm!("sti", options(nomem, nostack)); }
}

/// Idle until the next maskable interrupt. STI's interrupt shadow makes the
/// following HLT safe against the usual check-then-sleep race.
pub fn wait_for_interrupt() {
    unsafe { core::arch::asm!("sti; hlt", options(nomem, nostack)); }
}

pub fn timer_ticks() -> u64 {
    TIMER_TICKS.load(Ordering::Acquire)
}

pub fn keyboard_pop_scancode() -> Option<u8> {
    let tail = KEYBOARD_TAIL.load(Ordering::Relaxed);
    let head = KEYBOARD_HEAD.load(Ordering::Acquire);
    if tail == head {
        return None;
    }
    let value = KEYBOARD_QUEUE[tail & (KEYBOARD_QUEUE_CAPACITY - 1)].load(Ordering::Relaxed);
    KEYBOARD_TAIL.store(tail.wrapping_add(1), Ordering::Release);
    Some(value)
}

pub fn keyboard_dropped_scancodes() -> u64 {
    KEYBOARD_DROPPED.load(Ordering::Acquire)
}

#[unsafe(no_mangle)]
extern "C" fn irq_dispatch(frame_ptr: *const InterruptFrame) {
    if frame_ptr.is_null() {
        unsafe { core::arch::asm!("cli", options(nomem, nostack)); }
        crate::halt_forever();
    }
    let vector = unsafe { (*frame_ptr).vector };
    match vector {
        32 => { TIMER_TICKS.fetch_add(1, Ordering::Relaxed); }
        33 => {
            // IRQ1: read the controller data register before acknowledging the PIC.
            let scancode = unsafe { inb(0x60) };
            let head = KEYBOARD_HEAD.load(Ordering::Relaxed);
            let tail = KEYBOARD_TAIL.load(Ordering::Acquire);
            if head.wrapping_sub(tail) >= KEYBOARD_QUEUE_CAPACITY {
                KEYBOARD_DROPPED.fetch_add(1, Ordering::Relaxed);
            } else {
                KEYBOARD_QUEUE[head & (KEYBOARD_QUEUE_CAPACITY - 1)]
                    .store(scancode, Ordering::Relaxed);
                KEYBOARD_HEAD.store(head.wrapping_add(1), Ordering::Release);
            }
        }
        _ => { /* No other legacy IRQ is unmasked at this stage. */ }
    }
    unsafe { outb(0x20, 0x20); } // EOI to master PIC.
}

unsafe fn outb(port: u16, value: u8) {
    unsafe { core::arch::asm!("out dx, al", in("dx") port, in("al") value, options(nomem, nostack, preserves_flags)); }
}

unsafe fn inb(port: u16) -> u8 {
    let value: u8;
    unsafe { core::arch::asm!("in al, dx", in("dx") port, out("al") value, options(nomem, nostack, preserves_flags)); }
    value
}

unsafe fn io_wait() {
    unsafe { outb(0x80, 0); }
}

pub fn is_initialized() -> bool {
    INITIALIZED.load(Ordering::Acquire)
}

#[unsafe(no_mangle)]
extern "C" fn exception_dispatch(frame_ptr: *const InterruptFrame) -> ! {
    unsafe { core::arch::asm!("cli", options(nomem, nostack)); }
    // Capture CR2 before printing anything: a secondary page fault could overwrite it.
    let page_fault_address = if !frame_ptr.is_null() {
        let vector = unsafe { (*frame_ptr).vector };
        if vector == 14 {
            let value: u64;
            unsafe { core::arch::asm!("mov {}, cr2", out(reg) value, options(nomem, nostack, preserves_flags)); }
            value
        } else { 0 }
    } else { 0 };
    crate::serial_write("\r\n========== XAVIEROS KERNEL EXCEPTION ==========\r\n");
    if frame_ptr.is_null() {
        crate::serial_write("Exception frame is null.\r\n");
        crate::halt_forever();
    }
    let frame = unsafe { &*frame_ptr };
    crate::serial_write("EXCEPTION: ");
    crate::serial_write(exception_name(frame.vector));
    crate::serial_write(" (#");
    crate::serial_write_u64(frame.vector);
    crate::serial_write(")\r\nERROR CODE: 0x");
    crate::serial_write_u64_hex(frame.error_code);
    crate::serial_write("\r\nRIP: 0x");
    crate::serial_write_u64_hex(frame.rip);
    crate::serial_write("  CS: 0x");
    crate::serial_write_u64_hex(frame.cs);
    crate::serial_write("  RFLAGS: 0x");
    crate::serial_write_u64_hex(frame.rflags);
    crate::serial_write("\r\nRAX: 0x"); crate::serial_write_u64_hex(frame.rax);
    crate::serial_write("  RBX: 0x"); crate::serial_write_u64_hex(frame.rbx);
    crate::serial_write("  RCX: 0x"); crate::serial_write_u64_hex(frame.rcx);
    crate::serial_write("  RDX: 0x"); crate::serial_write_u64_hex(frame.rdx);
    crate::serial_write("\r\nRSI: 0x"); crate::serial_write_u64_hex(frame.rsi);
    crate::serial_write("  RDI: 0x"); crate::serial_write_u64_hex(frame.rdi);
    crate::serial_write("  RBP: 0x"); crate::serial_write_u64_hex(frame.rbp);
    crate::serial_write("\r\nR8:  0x"); crate::serial_write_u64_hex(frame.r8);
    crate::serial_write("  R9:  0x"); crate::serial_write_u64_hex(frame.r9);
    crate::serial_write("  R10: 0x"); crate::serial_write_u64_hex(frame.r10);
    crate::serial_write("  R11: 0x"); crate::serial_write_u64_hex(frame.r11);
    crate::serial_write("\r\nR12: 0x"); crate::serial_write_u64_hex(frame.r12);
    crate::serial_write("  R13: 0x"); crate::serial_write_u64_hex(frame.r13);
    crate::serial_write("  R14: 0x"); crate::serial_write_u64_hex(frame.r14);
    crate::serial_write("  R15: 0x"); crate::serial_write_u64_hex(frame.r15);
    if frame.vector == 14 {
        crate::serial_write("\r\nCR2 (FAULT ADDRESS): 0x");
        crate::serial_write_u64_hex(page_fault_address);
    }
    crate::serial_write("\r\nKernel halted. Reboot the virtual machine to continue.\r\n");
    crate::halt_forever();
}

fn exception_name(vector: u64) -> &'static str {
    match vector {
        0 => "DIVIDE ERROR",
        1 => "DEBUG",
        2 => "NON-MASKABLE INTERRUPT",
        3 => "BREAKPOINT",
        4 => "OVERFLOW",
        5 => "BOUND RANGE EXCEEDED",
        6 => "INVALID OPCODE",
        7 => "DEVICE NOT AVAILABLE",
        8 => "DOUBLE FAULT",
        9 => "COPROCESSOR SEGMENT OVERRUN",
        10 => "INVALID TSS",
        11 => "SEGMENT NOT PRESENT",
        12 => "STACK-SEGMENT FAULT",
        13 => "GENERAL PROTECTION FAULT",
        14 => "PAGE FAULT",
        16 => "X87 FLOATING-POINT EXCEPTION",
        17 => "ALIGNMENT CHECK",
        18 => "MACHINE CHECK",
        19 => "SIMD FLOATING-POINT EXCEPTION",
        20 => "VIRTUALIZATION EXCEPTION",
        21 => "CONTROL PROTECTION EXCEPTION",
        29 => "VMM COMMUNICATION EXCEPTION",
        30 => "SECURITY EXCEPTION",
        255 => "UNEXPECTED VECTOR",
        _ => "CPU EXCEPTION",
    }
}

core::arch::global_asm!(r#"
.intel_syntax noprefix
.section .text

.global isr_default
isr_default:
    push 0
    push 255
    jmp isr_common

"#);

// Emit the small vector stubs separately. Exceptions that push an error code are
// #DF, #TS, #NP, #SS, #GP, #PF, #AC, #CP, #VC and #SX.
core::arch::global_asm!(r#"
.intel_syntax noprefix
.section .text
.global isr0
isr0:
    push 0
    push 0
    jmp isr_common
.global isr1
isr1:
    push 0
    push 1
    jmp isr_common
.global isr2
isr2:
    push 0
    push 2
    jmp isr_common
.global isr3
isr3:
    push 0
    push 3
    jmp isr_common
.global isr4
isr4:
    push 0
    push 4
    jmp isr_common
.global isr5
isr5:
    push 0
    push 5
    jmp isr_common
.global isr6
isr6:
    push 0
    push 6
    jmp isr_common
.global isr7
isr7:
    push 0
    push 7
    jmp isr_common
.global isr8
isr8:
    push 8
    jmp isr_common
.global isr9
isr9:
    push 0
    push 9
    jmp isr_common
.global isr10
isr10:
    push 10
    jmp isr_common
.global isr11
isr11:
    push 11
    jmp isr_common
.global isr12
isr12:
    push 12
    jmp isr_common
.global isr13
isr13:
    push 13
    jmp isr_common
.global isr14
isr14:
    push 14
    jmp isr_common
.global isr15
isr15:
    push 0
    push 15
    jmp isr_common
.global isr16
isr16:
    push 0
    push 16
    jmp isr_common
.global isr17
isr17:
    push 17
    jmp isr_common
.global isr18
isr18:
    push 0
    push 18
    jmp isr_common
.global isr19
isr19:
    push 0
    push 19
    jmp isr_common
.global isr20
isr20:
    push 0
    push 20
    jmp isr_common
.global isr21
isr21:
    push 21
    jmp isr_common
.global isr22
isr22:
    push 0
    push 22
    jmp isr_common
.global isr23
isr23:
    push 0
    push 23
    jmp isr_common
.global isr24
isr24:
    push 0
    push 24
    jmp isr_common
.global isr25
isr25:
    push 0
    push 25
    jmp isr_common
.global isr26
isr26:
    push 0
    push 26
    jmp isr_common
.global isr27
isr27:
    push 0
    push 27
    jmp isr_common
.global isr28
isr28:
    push 0
    push 28
    jmp isr_common
.global isr29
isr29:
    push 29
    jmp isr_common
.global isr30
isr30:
    push 30
    jmp isr_common
.global isr31
isr31:
    push 0
    push 31
    jmp isr_common

.global isr_common
isr_common:
    cld
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    and rsp, -16
    call exception_dispatch
1:
    cli
    hlt
    jmp 1b
"#);


core::arch::global_asm!(r#"
.intel_syntax noprefix
.section .text

.global irq0
irq0:
    push 0
    push 32
    jmp irq_common

.global irq1
irq1:
    push 0
    push 33
    jmp irq_common

.global irq_common
irq_common:
    cld
    push rax
    push rbx
    push rcx
    push rdx
    push rsi
    push rdi
    push rbp
    push r8
    push r9
    push r10
    push r11
    push r12
    push r13
    push r14
    push r15
    mov rdi, rsp
    mov rbx, rsp
    and rsp, -16
    call irq_dispatch
    mov rsp, rbx
    pop r15
    pop r14
    pop r13
    pop r12
    pop r11
    pop r10
    pop r9
    pop r8
    pop rbp
    pop rdi
    pop rsi
    pop rdx
    pop rcx
    pop rbx
    pop rax
    add rsp, 16
    iretq
"#);
