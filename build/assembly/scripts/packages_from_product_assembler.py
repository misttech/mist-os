#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Script to extract line-separated cache package paths with image_assembly.json from `ffx assembly product`.
"""

import argparse
import json
import os
import sys

from pathlib import Path


def main():
    parser = argparse.ArgumentParser(
        description="Parse assembly product output manifest to get packages for a particular package set."
    )
    parser.add_argument(
        "--assembly-manifest",
        type=argparse.FileType("r"),
        required=True,
        help="Path to image_assembly.json created by `ffx assembly product`.",
    )
    parser.add_argument(
        "--package-set",
        required=True,
        help="Package set to get the packages for",
    )
    parser.add_argument(
        "--rebase",
        required=False,
        help="Optionally rebase the package manifest paths from the assembly manifest.",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Path to which to write desired output list.",
    )
    args = parser.parse_args()

    assert not args.rebase or os.path.isdir(
        args.rebase
    ), "--rebase needs to specify a valid directory path!"
    assembly_manifest = json.load(args.assembly_manifest)
    package_manifests = [
        (
            # Normalize the rebased path to remove any internal ../..'s.
            os.path.normpath(args.rebase.rstrip("/") + "/" + package)
            if args.rebase
            else package
        )
        for package in assembly_manifest[args.package_set]
    ]
    Path(args.output).write_text(
        json.dumps(
            {
                "content": {"manifests": package_manifests},
                "version": "1",
            },
            indent=2,
        )
    )

    return 0


if __name__ == "__main__":
    sys.exit(main())
