#!/usr/bin/env fuchsia-vendored-python
"""Formats an input list of paths as extraPaths for pyrightconfig.json"""

# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import dataclasses
import json
import os
import sys


@dataclasses.dataclass
class Args:
    fidl_python_paths_file: str
    output_file: str
    build_directory_path: str
    top_level_pyright_file: str


def main(args: Args) -> int:
    if not os.path.isfile(args.fidl_python_paths_file):
        print(
            f"FIDL paths file '{args.fidl_python_paths_file}' does not exist."
        )
        return 1

    if not os.path.isfile(args.top_level_pyright_file):
        print(f"Input file '{args.top_level_pyright_file}' does not exist.")
        return 1

    paths: set[str] = set()

    with open(args.fidl_python_paths_file, "r") as input_file:
        input = json.load(input_file)
        if not isinstance(input, list):
            print(
                f"Input file {args.fidl_python_paths_file} must contain a list."
            )
            return 1
        # FIDL paths are relative to the build directory, so we need to prepend that path.
        paths.update(
            [os.path.join(args.build_directory_path, path) for path in input],
        )

    with open(args.top_level_pyright_file, "r") as input_file:
        # Strip comments. Pyright handles them fine, but Python json does not like them.
        lines = [
            l for l in input_file.readlines() if not l.lstrip().startswith("//")
        ]
        input = json.loads("\n".join(lines))
        if not isinstance(input, dict):
            print(
                f"Input file {args.top_level_pyright_file} must contain a dict."
            )
            return 1
        if "fuchsiaExtraPaths" in input:
            paths.update(input["fuchsiaExtraPaths"])

    with open(args.output_file, "w") as output_file:
        output = {"extraPaths": sorted(paths)}
        json.dump(output, output_file, indent=2)

    return 0


def get_args() -> Args:
    parser = argparse.ArgumentParser(
        "create_pyright_base_config",
        description="Create a base config for pyright-based IDE integrations.",
    )

    parser.add_argument(
        "--fidl_python_paths_file",
        type=str,
        required=True,
        help="Path to read fidl binding file paths from, which must be a JSON file containing a list of strings.",
    )
    parser.add_argument(
        "--top_level_pyright_file",
        type=str,
        required=True,
        help="Path to the top-level pyrightconfig.json; used to merge extraPaths into one destination.",
    )
    parser.add_argument(
        "--output_file",
        type=str,
        required=True,
        help="Path to write the output. Will be overwritten.",
    )
    parser.add_argument(
        "--build_directory_path",
        type=str,
        required=True,
        help="Path to the build directory, used to ensure the final symlinked path is correct.",
    )

    return parser.parse_args(namespace=Args("", "", "", ""))


if __name__ == "__main__":
    sys.exit(main(get_args()))
