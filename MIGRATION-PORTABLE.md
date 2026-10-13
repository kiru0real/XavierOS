# Moving XavierOS development tooling off PowerShell

This source snapshot replaces `setup.ps1`, `build.ps1`, and `run.ps1` with Python scripts. It does **not** publish a release or modify the existing `v0.1.0` / Beta 1 release assets.

## Local Windows workflow

1. Back up your current XavierOS project folder.
2. Extract this archive into the project folder, replacing matching source and documentation files.
3. Keep `Cargo.toml`, `Cargo.lock`, `.cargo/`, `src/`, `limine.conf`, and `linker.ld` together.
4. Open a terminal in the project root and run:

   ```text
   python setup.py
   python build.py --clean
   python run.py
   ```

5. In the XavierOS shell, run `IDT`, `IRQ`, `TIME`, `SELFTEST`, and `DIAG`. Run `TRAP` only last because it is intentionally fatal.

If your Python command is `py`, use `py -3 setup.py`, `py -3 build.py --clean`, and `py -3 run.py` instead.

## Removing PowerShell from the GitHub language statistics

The archive no longer contains the old `.ps1` scripts, but uploading files in GitHub's browser does not automatically delete files that were already in the repository. After validating this snapshot, delete these three obsolete files from the current branch in the GitHub web UI:

- `setup.ps1`
- `build.ps1`
- `run.ps1`

Open each file, choose **Delete this file**, then commit the deletion. Upload the new `setup.py`, `build.py`, `run.py`, updated source, and docs through **Add file → Upload files**. Do not create a release or move/delete the existing `v0.1.0` tag unless you explicitly decide to.

## Cross-platform notes

The kernel remains x86_64 and still boots through Limine. Python removes PowerShell as the build-system dependency; it does not remove the bootloader, ISO creation, Rust toolchain, or QEMU dependencies. Windows uses the official Limine executable and may download the Windows xorriso bundle. macOS/Linux compile the official Limine host utility from `limine.c` included in Limine's archive, requiring a C compiler.
