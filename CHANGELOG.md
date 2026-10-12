# Changelog

Notable changes to XavierOS are documented here.

## 0.1.0 — First public beta

- Boots a freestanding Rust `no_std` kernel through Limine.
- Native Windows PowerShell build and QEMU run scripts.
- Limine framebuffer console with polled PS/2 keyboard input.
- Interactive command shell and XavierOS-branded system summary.
- Limine memory-map parsing, HHDM address translation, and bitmap PMM for pages below 4 GiB.
- PMM/HHDM self-tests and zeroed-page allocation.
- Commands for system information, diagnostics, a checked integer calculator, reboot, and halt.

### Known limitations

- Experimental; intended for QEMU and learning, not daily use.
- No desktop, filesystem, networking, userspace, process scheduler, or kernel heap.
- Keyboard input polls the emulated PS/2 controller.
- PMM only manages physical pages below 4 GiB and is not SMP-safe.
