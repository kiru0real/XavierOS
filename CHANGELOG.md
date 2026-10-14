# Changelog

## 0.1.2 — User accounts and shell usability

- Added persistent local accounts with masked password entry, login/logout, `WHOAMI`, `USERS`, `PASSWD`, and `HOME`.
- Stored passwords as salted PBKDF2-HMAC-SHA256 hashes rather than plaintext.
- Added per-account home directories and basic shell-level restrictions for writable-disk paths.
- Added a 16-command in-memory history navigated with Up/Down.
- Added Tab completion for built-in commands and accessible VFS file/directory paths.
- Improved host-side validation tests and fixed input-completion/build issues.

## 0.1.0 — Initial public release

- Initial public version of the freestanding Rust `no_std` x86_64 kernel.
- Limine boot, framebuffer console, memory management, IDT/PIC/PIT interrupts, ATA PIO storage, XVRFS initramfs, XVDISKFS, VFS dispatch, and a cooperative task prototype.

See [README.md](README.md) for current features and known limitations.
