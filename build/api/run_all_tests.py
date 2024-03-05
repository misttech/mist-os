#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Run all Python tests in source directory. A.k.a. a poor man's pytest."""

import argparse
import os
import subprocess
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--source-dir",
        type=Path,
        help="Source directory (default is current one)",
    )
    parser.add_argument(
        "--quiet",
        action="store_true",
        help="Do not print anything, except errors.",
    )
    parser.add_argument(
        "--stamp", type=Path, help="Stamp file to write on success."
    )

    args = parser.parse_args()

    if not args.source_dir:
        source_dir = Path(__file__).parent
    else:
        source_dir = args.source_dir

    test_files = sorted(
        file for file in os.listdir(source_dir) if file.endswith("_test.py")
    )
    failures = []
    for test_file in test_files:
        if not args.quiet:
            print(f"Running {test_file}", file=sys.stderr)
        ret = subprocess.run(
            [sys.executable, source_dir / test_file],
            text=True,
            capture_output=True,
        )
        if ret.returncode != 0:
            print(
                f"FAILURE: STDOUT -----\n{ret.stdout}\nSTDERR -----------\n{ret.stderr}\n"
            )
            failures.append(test_file)

    count = len(test_files)

    if failures:
        print(
            "ERROR: %s tests out of %s failed!\n%s\n"
            % (len(failures), count, "\n".join(failures)),
            file=sys.stderr,
        )
        return 1

    if not args.quiet:
        print(f"SUCCESS: {count} tests passed.")

    if args.stamp:
        args.stamp.write_text("ok")

    return 0


if __name__ == "__main__":
    sys.exit(main())
