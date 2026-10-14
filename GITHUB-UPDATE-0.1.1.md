# GitHub update checklist for XavierOS 0.1.1

This is a source update and release draft. It does not publish anything automatically.

## Update `main`

1. Extract `XavierOS-0.1.1-Source.zip` locally.
2. In the GitHub repository, select `main` and use **Add file → Upload files**.
3. Upload all source files and documentation from the ZIP root, preserving directory names. Keep `.github/`, `.cargo/`, `src/`, `rootfs/`, and `tests/` as folders.
4. Delete the old `build.ps1`, `run.ps1`, and `setup.ps1` in the web interface if they still appear in `main`. These are not present in this source package, but GitHub does not delete files omitted from an upload.
5. Do not upload `build/`, `target/`, `tools/`, `dist/`, or `data/xavieros-disk.img`.
6. Commit with `Prepare 0.1.1 maintenance update`.
7. Run the host-side checks listed in `TESTING.md` locally. Do not publish until the tests, clean build, and QEMU regression checklist have been reviewed.

## Build and test locally

```text
python setup.py
python build.py --clean
python run.py
```

Run the commands in `TESTING.md`, including the persistent-file restart test. Then close QEMU and run:

```text
python package_release.py
```

Upload `dist/xavieros-0.1.1.iso` and `dist/SHA256SUMS.txt` as the Release Assets.

## Draft the release

- Tag: `v0.1.1`
- Target: `main`
- Title: `XavierOS 0.1.1`
- Description: contents of `RELEASE-NOTES-0.1.1.md`
- **Set as a pre-release:** unchecked
- Assets: `xavieros-0.1.1.iso`, `SHA256SUMS.txt`

Keep the old `v0.1.0` release/tag intact. Publish only when the maintainer has reviewed the build and test results.
