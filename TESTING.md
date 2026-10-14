# Testing XavierOS 0.1.2

Host-side tests are useful, but they do not replace a clean kernel build and runtime tests in QEMU.

## 1. Host checks

```text
python -m compileall -q build.py run.py setup.py mkinitramfs.py package_release.py tests
python -m unittest discover -s tests -v
python mkinitramfs.py --output build/initramfs-check.xfs
```

## 2. Clean build and boot

```text
python setup.py
python build.py --clean
python run.py
```

Expected: the scripts exit successfully and `build/xavieros.iso` boots in QEMU.

## 3. Shell and subsystem smoke tests

```text
HELP
VERSION
SYSINFO
IDT
IRQ
TIME
HEAP
FSINFO
FSTEST
DISK
LS /
LS /DISK
TASKTEST
SELFTEST
DIAG
```

Run `TRAP` last; it intentionally triggers an invalid-opcode exception and stops the virtual machine.

## 4. Users and shell

On a disposable fresh test disk:

1. Run `WHOAMI` and confirm the temporary `live` session.
2. Create the first user with `USERADD alice`, then test `LOGIN alice`, `WHOAMI`, `HOME`, `PASSWD`, `USERS`, and `LOGOUT`.
3. Test that a standard user is restricted to their own `/DISK/HOME/<name>` path by attempting to access another user's home.
4. Press Up/Down after running several commands to test history.
5. Type `ho` and press Tab; confirm it completes to `HOME`. Test Tab on a unique accessible path as well.

## 5. Persistent disk regression

Use only the disposable 16 MiB image created by `run.py`.

```text
MKDIR /DISK/REGRESSION
WRITE /DISK/REGRESSION/KEEP.TXT original
CAT /DISK/REGRESSION/KEEP.TXT
WRITE /DISK/REGRESSION/KEEP.TXT replacement
CAT /DISK/REGRESSION/KEEP.TXT
```

Close QEMU normally, start it again without `--reset-disk`, and verify that the file and account database persist. Remove test files only after collecting the result. The filesystem is not crash-safe; do not test it against a real drive.

## 6. Record results before release

- Host OS:
- Rust version:
- Python version:
- QEMU version:
- Host tests:
- Clean ISO build:
- `SELFTEST`:
- Account/path restriction checks:
- History/Tab checks:
- Persistence after restart:
- Known issues:
