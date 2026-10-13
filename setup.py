#!/usr/bin/env python3
"""Prepare the Rust bare-metal target used by XavierOS."""
from __future__ import annotations

import shutil
import subprocess
import sys


def main() -> int:
    if not shutil.which("rustup"):
        print("error: rustup was not found. Install Rust from https://rustup.rs/", file=sys.stderr)
        return 1
    return subprocess.run(["rustup", "target", "add", "x86_64-unknown-none"], check=False).returncode


if __name__ == "__main__":
    raise SystemExit(main())
