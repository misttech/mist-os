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


def main():
    parser = argparse.ArgumentParser(
        description=
        "Parse assembly product output manifest to get cache packages.")
    parser.add_argument(
        "--assembly-manifest",
        type=argparse.FileType('r'),
        required=True,
        help="Path to image_assembly.json created by `ffx assembly product`.")
    parser.add_argument(
        "--rebase",
        required=False,
        help=
        "Optionally rebase the package manifest paths from the assembly manifest."
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Path to which to write desired output list.")
    args = parser.parse_args()

    assert not args.rebase or os.path.isdir(
        args.rebase), "--rebase needs to specify a valid directory path!"
    assembly_manifest = json.load(args.assembly_manifest)
    with open(args.output, 'w') as output:
        for cache_package in assembly_manifest["cache"]:
            if args.rebase:
                output.write(args.rebase.rstrip("/") + "/")
            output.write(cache_package)
            output.write("\n")

    return 0


if __name__ == '__main__':
    sys.exit(main())
