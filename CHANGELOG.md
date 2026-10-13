# Changelog

## 0.1.0 — 2026-10-14

First public release of XavierOS.

- Freestanding Rust `no_std` kernel for x86_64 booted with Limine.
- Framebuffer text console and interactive built-in shell.
- Limine memory-map parsing, HHDM, bitmap PMM for usable physical pages below 4 GiB, and a first-fit kernel heap.
- x86_64 IDT and diagnostic handlers for CPU exceptions.
- Legacy PIC remapping, PIT timer, and PS/2 keyboard input through IRQs.
- Read-only XVRFS v1 initramfs generated from `rootfs/` and embedded in the kernel.
- Legacy ATA PIO sector I/O for the QEMU primary IDE master.
- Writable XVDISKFS v1 mounted at `/DISK`, with create/read/write/remove operations and data persistence in a virtual disk image between QEMU runs.
- VFS dispatcher for the embedded root filesystem and writable disk.
- Cooperative task-context switching prototype and self-test commands.
- Cross-platform Python helpers for preparing the Rust target, building the ISO, and running QEMU on Windows, macOS, and Linux.

## Known limitations

- Intended for QEMU and learning; physical-hardware support is not established.
- Disk filesystem is experimental and not crash-safe. Use only disposable virtual disk images.
- Storage driver currently targets a legacy PC/PIIX IDE setup in QEMU.
- Task scheduling is cooperative; no timer preemption, user mode, or process isolation.
- No network stack or desktop environment.
- PMM handles usable pages below 4 GiB and is not SMP-safe.
