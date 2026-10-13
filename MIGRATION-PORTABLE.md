# Portable tooling and 0.1.1 source update

The project uses Python scripts as its documented build/run interface. Windows remains supported, but PowerShell is not required. macOS and Linux can use the same scripts when Python, Rust, QEMU, xorriso, and the required compiler are installed.

## Build interface

```text
python setup.py
python build.py --clean
python run.py
```

Create release assets after a verified ISO build:

```text
python package_release.py
```

## Updating the GitHub repository through the browser

1. Download and extract the full `XavierOS-0.1.1-Source.zip` archive.
2. In `kiru0real/XavierOS`, open the `main` branch and choose **Add file → Upload files**.
3. Upload the archive's project files and directories while preserving their paths. Do not upload local `target/`, `build/`, `tools/`, `dist/`, or `data/xavieros-disk.img`.
4. Delete old `build.ps1`, `run.ps1`, and `setup.ps1` files from `main` in the browser if they are still present. Uploading new files does not remove old files automatically.
5. Commit the change with a message such as `Prepare 0.1.1 maintenance update`.
6. Wait for the new CI workflow to run and fix any failures before drafting a release.

The published `v0.1.0` tag/release should not be edited. For the next release, use tag `v0.1.1`, title `XavierOS 0.1.1`, and leave the pre-release checkbox off. Publish only after a clean build and QEMU checklist pass.
