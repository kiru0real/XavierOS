<div align="center">

# XavierOS

**A tiny experimental x86_64 hobby operating system written in Rust.**

Built from scratch with `no_std`, booted by Limine, and developed natively on Windows.

[![License: MIT](https://img.shields.io/badge/License-MIT-blue.svg)](LICENSE)
[![Platform](https://img.shields.io/badge/Platform-Windows%20%2B%20QEMU-informational)](#build-and-run)
[![Status](https://img.shields.io/badge/Status-Experimental%20Beta-orange)](#status-and-limitations)

**Current version: `0.1.0` · First public pre-release: `v0.1.0-beta.1`**

[Русская версия README](README.ru.md)

</div>

---

## About

XavierOS is a small hobby operating system project focused on learning how a computer boots and how a kernel manages memory, video output, and keyboard input. The kernel is written in Rust without the standard library (`#![no_std]`) and currently targets `x86_64`.

The first public beta boots into a simple interactive text console rather than a desktop environment. The project is experimental and intended for learning and testing in a virtual machine.

## Current features

- **Rust bare-metal kernel** targeting `x86_64-unknown-none`.
- **Limine boot protocol** and a hybrid BIOS/UEFI bootable ISO.
- **Framebuffer console** with a built-in bitmap font.
- **Polled PS/2 keyboard input** for the interactive shell.
- **Serial diagnostics** through COM1, visible in the PowerShell window when using the provided QEMU script.
- **Limine memory-map parsing** and a bitmap physical memory manager (PMM) for usable pages below 4 GiB.
- **HHDM support** for physical-to-virtual address translation and zeroed-page allocation.
- **PMM/HHDM self-tests** for allocation, freeing, zeroing, and memory access.
- **Built-in console commands**, including system information, diagnostics, and a checked integer calculator.
- **Windows-native build scripts**; WSL and a Linux environment are not required.

## Build and run

### Requirements

- Windows 10 or Windows 11.
- PowerShell 5.1 or newer.
- Rust and `rustup`.
- QEMU for Windows (`qemu-system-x86_64.exe`).
- An internet connection on the first build: the build script downloads the Limine binary tools and a Windows `xorriso` bundle when they are missing.

### 1. Install Rust

In PowerShell:

```powershell
winget install --id Rustlang.Rustup -e
```

Close and reopen PowerShell after installation.

### 2. Install QEMU

Download QEMU for Windows from the [official QEMU downloads page](https://www.qemu.org/download/#windows). Make sure `qemu-system-x86_64.exe` is on `PATH`, or install QEMU in its usual Program Files directory.

### 3. Build the ISO

Open PowerShell in the repository directory and run:

```powershell
Set-ExecutionPolicy -Scope Process Bypass -Force
.\setup.ps1
.\build.ps1
```

The execution-policy command applies only to the current PowerShell process. It does not permanently change the machine-wide policy.

A successful build creates:

```text
build\xavieros.iso
```

### 4. Run in QEMU

```powershell
.\run.ps1
```

Click inside the QEMU window to give the virtual machine keyboard focus. Switch to an English keyboard layout and type:

```text
HELP
SYSINFO
DIAG
```

Press Enter after each command. The serial diagnostic log is also shown in the PowerShell window.

To clean generated build tools and outputs before rebuilding:

```powershell
.\build.ps1 -Clean
```

> **Tip:** Keeping the project in a short path such as `C:\dev\XavierOS` can make Windows toolchains easier to troubleshoot.

## Console commands

Commands are currently entered in uppercase ASCII. The shell is intentionally small; it is not a POSIX shell and does not run Linux commands.

| Command | Description |
|---|---|
| `HELP` or `?` | Show available commands |
| `ABOUT` | Describe the project and current state |
| `BANNER` | Display the XavierOS logo |
| `SYSINFO` | Show a XavierOS-branded system summary |
| `INFO` | Show kernel, architecture, bootloader, and interface information |
| `VERSION` | Print the current OS version (`0.1.0`) |
| `CPU` | Display CPU vendor/model information exposed through CPUID |
| `MEM` | Show memory-map and PMM statistics |
| `PMM` | Show physical page allocator status |
| `HHDM` | Show direct-map information |
| `DIAG` | Run a compact system diagnostic and memory self-test |
| `SELFTEST` | Run the PMM/HHDM tests again |
| `CALC A OP B` | Calculate signed 64-bit integer `+`, `-`, `*`, or `/` expressions with overflow and divide-by-zero checks |
| `ECHO TEXT` | Print text |
| `CLEAR` | Clear the console |
| `REBOOT` | Request a reboot of the virtual machine |
| `HALT` | Halt the CPU until QEMU is restarted or closed |

Examples:

```text
HELP
SYSINFO
MEM
CALC 12 * 4
CALC -7 + 3
ECHO HELLO XAVIEROS
SELFTEST
```

`SYSINFO` reports the CPU model exposed to the guest by QEMU and memory usable by XavierOS inside the virtual machine. It does **not** report the host computer's full hardware inventory or all installed physical RAM.

## Project structure

```text
XavierOS/
├── .cargo/
│   └── config.toml      # Bare-metal target and linker flags
├── src/
│   ├── main.rs          # Kernel entry point, framebuffer, keyboard, shell
│   └── memory.rs        # Limine memory map, HHDM, physical page allocator
├── limine.conf          # Limine boot configuration
├── linker.ld            # Kernel ELF layout and Limine request sections
├── setup.ps1            # Prepare the Rust target
├── build.ps1            # Compile kernel and create bootable ISO
├── run.ps1              # Launch the ISO in QEMU
├── Cargo.toml
├── LICENSE
├── CHANGELOG.md
└── README.md
```

Generated directories such as `target/`, `build/`, and `tools/` are not source files and should not be committed. `Cargo.lock` should be committed for this binary project when generated by Cargo.

## Roadmap

The project is evolving incrementally. Possible next milestones include:

- Better console editing, command history, and scrolling.
- A kernel heap and more memory-manager tests.
- CPU exceptions and interrupt handling.
- An interrupt-driven keyboard driver.
- Virtual memory management and a more complete physical memory manager.
- Storage, a filesystem, and eventually a small userspace.

These are goals, not features promised by the current beta.

## Status and limitations

XavierOS is an **experimental hobby OS**, not a general-purpose or secure operating system. The current version has no desktop environment, filesystem, networking stack, userspace, process scheduler, kernel heap, or complete interrupt/exception framework. The physical page allocator manages only pages below 4 GiB and is not SMP-safe. Keyboard input currently polls the emulated PS/2 controller and assumes conventional Set 1 scancodes.

**Run XavierOS in QEMU or another disposable virtual machine. Do not use it on your primary machine or rely on it to protect data.** The project has not been validated for general-purpose physical-hardware use.

## Contributing

Bug reports, ideas, and pull requests are welcome. Please include:

- Windows version and Rust version (`rustc --version`).
- QEMU version.
- The full output of `build.ps1` or the serial console log.
- Steps to reproduce the issue.

Please keep changes focused and update the README or changelog when user-visible behavior changes.

## License

Original XavierOS source code and documentation are distributed under the [MIT License](LICENSE). MIT is a permissive license that allows reuse and modification as long as the copyright and license notice are retained. See [Choose a License: MIT](https://choosealicense.com/licenses/mit/).

The build scripts download third-party tools such as Limine and `xorriso`. Those tools and any third-party components remain under their respective licenses; this repository's MIT license does not replace those licenses.

## Acknowledgements

- [Rust](https://www.rust-lang.org/) and its bare-metal target support.
- [Limine](https://github.com/limine-bootloader/limine) and the [Limine boot protocol](https://github.com/Limine-Bootloader/limine-protocol).
- [QEMU](https://www.qemu.org/) for virtual-machine testing.
- [xorriso](https://www.gnu.org/software/xorriso/) for ISO image creation.
