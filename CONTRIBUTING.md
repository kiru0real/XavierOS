# Contributing

Thanks for taking an interest in XavierOS. The project is experimental and prioritizes small changes that can be explained and tested.

## Before changing code

1. Read `README.md`, `ARCHITECTURE.md`, and `TESTING.md`.
2. Open an issue for substantial changes so the scope and compatibility impact can be discussed first.
3. Keep changes focused. Update the changelog and documentation when behavior or commands change.

## Required local checks

```text
python -m compileall -q build.py run.py setup.py mkinitramfs.py package_release.py tests
python -m unittest discover -s tests -v
cargo build --release --target x86_64-unknown-none
```

For kernel-facing changes, also build the ISO and boot it in QEMU. Run `SELFTEST` and the relevant section of `TESTING.md`. Host-side checks do not replace QEMU testing.

## Safety rules

- Only use disposable QEMU disk images. Never test `FORMAT DISK` or storage changes on a real drive.
- Do not call heap allocators or synchronous storage operations from IRQ handlers.
- Validate raw on-disk data before interpreting it as indexes, lengths, pointers, or sector ranges.
- Do not commit `target/`, `build/`, `tools/`, `dist/`, or `data/xavieros-disk.img`.
- Do not claim a build, hardware target, or test passed unless it was actually run.

## Pull requests

Explain what changed, why, how it was tested, and any known limitation. Use the pull-request template. Smaller, testable changes are easier to review than large unrelated rewrites.
