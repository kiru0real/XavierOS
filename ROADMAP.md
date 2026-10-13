# XavierOS Roadmap

The roadmap is intentionally staged. A feature is complete only after the relevant tests exist and the behavior is verified in QEMU.

## Current maintenance focus — 0.1.1

- [x] Add host-side tests for build helpers, archive extraction, initramfs generation, and release packaging.
- [x] Add CI for host tests and bare-metal Rust compilation.
- [x] Harden disk extent replacement, mount validation, heap deallocation, ATA transfer completion checks, and tool archive extraction.
- [ ] Run the full CI workflow against the GitHub source branch.
- [ ] Run the clean ISO build and QEMU regression checklist.
- [ ] Publish `v0.1.1` only after the maintainer approves it.

## Next kernel milestones

1. **Reliability:** serial logging levels, explicit error handling, fault-injection tests, and recovery documentation for the custom disk filesystem.
2. **Interrupt safety:** define which paths may disable interrupts, specify a locking policy, and keep disk/heap APIs out of IRQ context.
3. **Preemptive scheduling:** design interrupt-frame context switching separately from the current cooperative ABI, add task states/time slices, and test starvation/fairness.
4. **Virtual memory:** page-table mapping/unmapping, permissions, and address-space tests built on top of the existing PMM/HHDM.
5. **User mode and syscalls:** establish process isolation before attempting to load untrusted user programs.
6. **Storage evolution:** checksummed metadata and a documented recovery strategy, then additional block-device drivers only when the current one is well tested.
7. **Desktop:** graphical shell, mouse, and windows after memory, scheduling, and userspace interfaces are ready.

## Deliberately out of scope for the next patch

- A production-ready or crash-safe filesystem.
- Physical hardware support beyond documented, repeatable tests.
- Networking, a desktop, or a POSIX compatibility promise.
- Claiming preemptive multitasking before timer-driven context switching exists.
