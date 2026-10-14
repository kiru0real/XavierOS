# Contributing

Thanks for taking an interest in XavierOS. The project is experimental and favors small, understandable, testable changes.

## Before changing code

1. Read `README.md` and the relevant design/format document.
2. Open an issue for large changes so scope and compatibility can be discussed.
3. Keep changes focused. Update `CHANGELOG.md` and the README when behavior or commands change.

## Local checks

```text
python -m compileall -q build.py run.py setup.py mkinitramfs.py package_release.py tests
python -m unittest discover -s tests -v
cargo build --release --target x86_64-unknown-none
python build.py --clean
python run.py
```

For kernel changes, boot the resulting ISO in QEMU, run `SELFTEST`, and exercise the changed code path. Host-side tests do not replace QEMU testing.

## Safety rules

- Use disposable QEMU disk images only. Never test destructive storage commands on a real drive.
- Do not perform heap allocation or synchronous storage operations in IRQ handlers.
- Validate on-disk data before interpreting it as indexes, lengths, pointers, or sector ranges.
- Do not commit `target/`, `build/`, `tools/`, `dist/`, or `data/xavieros-disk.img`.
- Do not claim a build or test passed unless it was actually run.

## Pull requests

Explain what changed, why, how it was tested, and any known limitations. Prefer smaller, testable changes over large unrelated rewrites.
