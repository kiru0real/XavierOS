# Security Policy

XavierOS is an experimental hobby operating system and is not suitable for sensitive data or production workloads. It has no user/kernel isolation, no general-purpose permissions model, and a custom filesystem that is not crash-safe.

## Reporting a concern

Do not publish exploit details, private disk images, or personal data in a public issue. If GitHub private vulnerability reporting is enabled for this repository, use the **Security** tab to report privately. Otherwise contact the repository maintainer directly and provide a minimal reproducible description.

## Safe testing

- Test only in QEMU or another disposable virtual machine.
- Use only the test disk image created by `run.py`.
- `FORMAT DISK` and `python run.py --reset-disk` are destructive.
- Do not attach a physical or valuable disk to the current ATA PIO driver.
- Build-tool downloads are retrieved from upstream project release/source URLs. Archive extraction rejects traversal, links, special files, and case-insensitive duplicates, but downloads are not yet pinned by cryptographic hashes; verify upstream sources and host security before use.

## Scope

The maintainers welcome reports about reproducible memory-safety bugs, invalid on-disk metadata handling, build-tool path traversal, and unexpected kernel faults. This policy does not imply a supported response-time guarantee.
