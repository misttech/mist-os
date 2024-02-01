#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
This script unzips the contents of a mypy.pyz file. This workaround is
necessary because Mypy checks cannot run using mypy.pyz archives
directly, leading to errors during type checking.
"""

import argparse
import shutil
import sys
import zipfile

from pathlib import Path

def main():
    parser = argparse.ArgumentParser(
        "Sets up MyPy by unzipping to the unzip_dir_path"
    )
    parser.add_argument(
        "--pyz_path",
        help="Path to MyPy pyz archive",
        type=Path,
        required=True,
    )
    parser.add_argument(
        "--unzip_dir_path",
        help="Directory to unzip the .pyz file into",
        type=Path,
        required=True,
    )
    args = parser.parse_args()

    try:
        # This action is only run once per-build.
        # Cleans up the unzipped directory from previous builds.
        if args.unzip_dir_path.exists():
            shutil.rmtree(args.unzip_dir_path, ignore_errors=True)
        args.unzip_dir_path.mkdir(parents=True, exist_ok=True)

        with zipfile.ZipFile(args.pyz_path) as zip_ref:
            zip_ref.extractall(args.unzip_dir_path)

    except Exception as e:
        raise RuntimeError(f"MyPy setup failed: {e}") from e

if __name__ == '__main__':
    sys.exit(main())
