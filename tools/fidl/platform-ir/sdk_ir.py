#!/usr/bin/env fuchsia-vendored-python

# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from merge import merge_irs
from depfile import DepFile

import argparse
import json
import os


def main():
    parser = argparse.ArgumentParser(
        prog="sdk-ir",
        description="Create a merged JSON IR for all SDK FIDL libraries at a given API level.",
    )

    parser.add_argument(
        "--api-level",
        "-l",
        type=int,
        help="The API level to use. If not supplied the current in-tree levels will be used.",
    )
    parser.add_argument(
        "--sdk-fidl-json",
        "-f",
        type=argparse.FileType("r", encoding="UTF8"),
        required=True,
    )
    parser.add_argument(
        "--output",
        "-o",
        type=argparse.FileType("w", encoding="UTF8"),
        required=True,
    )
    parser.add_argument(
        "--keep-location", help="Keep source location information."
    )
    parser.add_argument("--keep-documentation", help="Keep API documentation.")
    parser.add_argument("--depfile", type=argparse.FileType("w"))

    args = parser.parse_args()

    # Find the build root directory,
    root_build_dir = os.path.dirname(args.sdk_fidl_json.name)

    # Extract the IR path for each library. This is the path for the in-tree version.
    # If we want to filter by SDK category we could do it here.
    ir_paths = [
        fidl_library["ir"] for fidl_library in json.load(args.sdk_fidl_json)
    ]

    def patch_path(path: str, api_level: int) -> str:
        """Modify a path a the platform build FIDL IR to be the one for a specific API level."""
        d, f = os.path.split(path)
        new_ending = f"_compile_{api_level}.fidl.json"
        f = f.replace(".fidl.json", new_ending, 1)
        # Make sure that worked as expected
        assert f.endswith(new_ending)
        return os.path.join(d, str(api_level), f)

    # If we're targeting a stable API level, tinker with the paths
    if args.api_level:
        ir_paths = [patch_path(p, args.api_level) for p in ir_paths]

    if args.depfile:
        DepFile.from_deps(args.output.name, ir_paths).write_to(args.depfile)

    # Merge those IRs
    merge_irs(
        inputs=[
            open(os.path.join(root_build_dir, ir), encoding="UTF8")
            for ir in ir_paths
        ],
        output=args.output,
        keep_location=args.keep_location,
        keep_documentation=args.keep_documentation,
    )


if __name__ == "__main__":
    main()
