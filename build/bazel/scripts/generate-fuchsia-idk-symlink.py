#!/usr/bin/env python3
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Run a bazel command to build a final IDK repository, and create a symlink to it."""

import argparse
import os
import subprocess
import sys
from pathlib import Path


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        "--bazel-launcher",
        required=True,
        type=Path,
        help="Path to Bazel launcher script.",
    )
    parser.add_argument(
        "--target-label",
        required=True,
        help="Bazel target label that creates the IDK directory",
    ),
    parser.add_argument(
        "--output-symlink",
        required=True,
        type=Path,
        help="Symlink to create pointing to the IDK directory",
    )

    args = parser.parse_args()

    if not args.bazel_launcher.exists():
        return parser.error(
            "Bazel launcher does not exist: %s" % args.bazel_launcher
        )

    # Build the final IDK directory.
    ret = subprocess.run(
        [
            str(args.bazel_launcher),
            "build",
            "--config=quiet",
            args.target_label,
        ],
        capture_output=True,
        text=True,
    )
    ret.check_returncode()

    # Get its location, relative to the execroot, e.g. "bazel-out/k8-fastbuild/bin/external/fuchsia_idk/final_idk
    execroot_location = subprocess.check_output(
        [
            str(args.bazel_launcher),
            "cquery",
            "--config=quiet",
            "--output=files",
            args.target_label,
        ],
        text=True,
    ).strip()

    output_base = args.bazel_launcher.parent / "output_base"
    input_dir = output_base / "execroot" / "main" / execroot_location

    if not input_dir.is_dir():
        print(
            f"ERROR: Cannot find input directory: {input_dir_location}",
            file=sys.stderr,
        )
        return 1

    # Re-generate the symlink if its path has changed, or if it does not exist.
    link_path = args.output_symlink
    link_dir = link_path.parent

    # Compute new link target, relative to the link's parent directory.
    new_target_path = os.path.relpath(input_dir.resolve(), link_dir.resolve())

    if link_path.exists():
        if link_path.is_symlink():
            # If a symlink already exists, compare its target to the new one
            # If they are the same, do not do anything.
            cur_target_path = link_path.readlink()
            if cur_target_path == new_target_path:
                return 0

        link_path.unlink()

    os.symlink(new_target_path, link_path)

    # Done!
    return 0


if __name__ == "__main__":
    sys.exit(main())
