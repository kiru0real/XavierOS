# Security Policy

XavierOS is an experimental hobby operating system and is not suitable for sensitive data or production workloads. It currently has no user/kernel isolation, no complete general-purpose permissions model, and a custom filesystem that is not crash-safe.

## Reporting a concern

Do not publish exploit details, private disk images, or personal data in a public issue. If private vulnerability reporting is enabled for this repository, use the GitHub **Security** tab. Otherwise contact the repository maintainer directly and provide a minimal reproducible description.

## Safe testing

- Test only in QEMU or another disposable virtual machine.
- Use only the test disk image created by `run.py`.
- `FORMAT DISK` and `python run.py --reset-disk` are destructive.
- Do not attach a physical or valuable disk to the current ATA PIO driver.
- Do not use real or reused passwords in XavierOS.
- Password salt generation prefers CPU RDRAND where available. The fallback on CPUs without RDRAND is timer/TSC-derived and is not cryptographically strong.
- Build-tool downloads are retrieved from upstream project locations. Verify external tools and their sources before running them; downloads are not all pinned by cryptographic hashes.

## Scope

The project welcomes reproducible reports about memory-safety problems, malformed on-disk metadata, host build-tool path handling, account/path restrictions, and unexpected kernel faults. This policy does not promise a response-time guarantee.
