# Testing the storage + cooperative-task development snapshot

These changes are development work, not a new release. The published `XavierOS Beta 1` (`v0.1.0`) assets are unchanged. Do not publish a new release until local build and QEMU tests succeed.

## Build and run

Requires Python 3.10+, Rust/rustup, xorriso, Limine boot tools, and QEMU. From the repository root:

```text
python build.py --clean
python run.py
```

`run.py` creates a blank 16 MiB local disk at `data/xavieros-disk.img` the first time. It preserves this file when rebuilding. Use `python run.py --reset-disk` only when you intentionally want to erase that test disk.

## Boot and filesystem checks

Run these in the XavierOS shell:

```text
DISK
LS /
CAT /ETC/ISSUE
LS /DISK
MKDIR /DISK/HOME
WRITE /DISK/HOME/HELLO.TXT Hello from XavierOS!
CAT /DISK/HOME/HELLO.TXT
STAT /DISK/HOME/HELLO.TXT
LS /DISK/HOME
```

Expected: `DISK` reports ATA/XVDISKFS ready, the embedded `/` and disk-backed `/DISK` are independently listable, and the just-written text can be read back. Now close QEMU completely, start `python run.py` again without `--reset-disk`, and run:

```text
CAT /DISK/HOME/HELLO.TXT
```

The text should persist. Also check:

```text
TOUCH /DISK/HOME/EMPTY.TXT
STAT /DISK/HOME/EMPTY.TXT
RM /DISK/HOME/EMPTY.TXT
RM /DISK/HOME/HELLO.TXT
RM /DISK/HOME
```

`FORMAT DISK` intentionally erases all contents; do not run it when you want to keep test data.

## Scheduler and existing subsystem checks

```text
TASKTEST
TASKS
IDT
IRQ
TIME
HEAP
FSTEST
SELFTEST
DIAG
```

Expected: `TASKTEST` reports `COOPERATIVE SCHEDULER SELF-TEST: PASS`; after it runs, the three test tasks should be listed as `FINISHED`. `TIME` keeps increasing and keyboard input stays responsive. `SELFTEST` should report PMM/HHDM, heap, initramfs, VFS mount routing, disk metadata (if mounted), interrupt setup, timer delivery, and cooperative scheduler checks passing.

Run `TRAP` last. It intentionally executes `ud2`; the guest should halt after the serial log prints `INVALID OPCODE (#6)` and the register diagnostics. Restart QEMU after the test.

## Important limitations

- This snapshot has not been compiled or booted in the authoring environment because no Rust toolchain or QEMU is installed there.
- ATA PIO is for the primary master on the legacy PC/PIIX IDE controller and is not a generic PCI/SATA/AHCI driver.
- XVDISKFS has a fixed table and contiguous extents; it has no journal or recovery from sudden power loss. Use disposable virtual disks only.
- `CAT` has a 4 KiB buffer; shell `WRITE` is limited by the command input buffer.
- The scheduler is cooperative only. It switches contexts when a task calls `yield_now()`; PIT does not preempt tasks. Do not yield while holding locks.
- The PMM and scheduler are single-core and not SMP-safe. This is not a release candidate.
