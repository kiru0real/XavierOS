# XavierOS Roadmap

A feature is considered complete only after the relevant checks exist and runtime behavior is verified in QEMU.

## Current foundation — 0.1.2

- [x] Rust `no_std` x86_64 kernel booted by Limine.
- [x] Framebuffer shell, IDT exception diagnostics, PIC/PIT, PS/2 keyboard input.
- [x] PMM, HHDM, kernel heap and cooperative task prototype.
- [x] Read-only XVRFS initramfs and writable XVDISKFS test-disk mount.
- [x] Experimental local accounts, login/logout, home directories and shell-level path restrictions.
- [x] Up/Down command history and Tab completion.

## Next kernel milestones

1. **Reliability:** fault-injection tests, stronger disk metadata consistency checks, and clearer errors/recovery behavior.
2. **Interrupt safety:** document critical sections and locking rules before adding concurrent kernel paths.
3. **Preemptive scheduling:** design interrupt-frame context switching separately from the cooperative task ABI; test fairness and starvation.
4. **Virtual memory:** page-table mapping/unmapping, permissions, and address-space tests built on PMM/HHDM.
5. **User mode and syscalls:** establish real process isolation before running untrusted user programs.
6. **Storage:** improve crash recovery and metadata integrity; add more block-device drivers only after current storage is well tested.
7. **Drivers:** introduce device discovery and abstractions as supported devices are added.
8. **Desktop:** graphical shell, mouse and windows after memory, scheduling and userspace interfaces are ready.

## Not promised

- A production-ready or crash-safe filesystem.
- General-purpose hardware compatibility.
- Networking, desktop, POSIX compatibility or a stable application ABI.
- Preemptive multitasking before timer-driven context switching is implemented and tested.
