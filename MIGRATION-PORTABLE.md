# Portable Build Tools

XavierOS uses Python scripts as its documented build/run interface. Windows is supported, but PowerShell is not required. macOS and Linux can use the same scripts when Python, Rust, QEMU, a C compiler, `xorriso`, and any required helper tools are installed.

## Build and run

```text
python setup.py
python -m unittest discover -s tests -v
python build.py --clean
python run.py
```

Create release assets after a verified build:

```text
python package_release.py
```

## Uploading through the GitHub browser

1. Extract the source archive.
2. Open the `main` branch of the XavierOS repository and choose **Add file → Upload files**.
3. Upload project files and directories while preserving their paths. Ensure hidden directories such as `.cargo/` are included.
4. Do not upload `target/`, `build/`, `tools/`, `dist/`, or `data/xavieros-disk.img`.
5. Delete obsolete PowerShell scripts from the GitHub tree manually if they still exist; uploading new files does not delete old files.
6. Commit, then build and test the exact source snapshot before tagging a release.

The release maintainer should publish only after a clean build and the QEMU checklist passes. No CI workflow is required for the documented manual workflow.
