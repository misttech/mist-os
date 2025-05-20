#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

HELP = """
This script returns the additional elements of PYTHONPATH that are needed to run Honeydew locally.
"""

import argparse
import dataclasses
import json
import os
import sys


def get_default_paths(fuchsia_dir: str, build_dir: str) -> list[str]:
    return [
        f"{build_dir}/host_x64",
        f"{fuchsia_dir}/src/developer/ffx/lib/fuchsia-controller/python",
        f"{fuchsia_dir}/src/lib/diagnostics/python",
        f"{build_dir}/host_x64/gen/src/developer/ffx/lib/fuchsia-controller/cpp",
    ]


@dataclasses.dataclass
class Args:
    python_path_json: str
    fuchsia_dir: str
    build_dir: str


def main(args: Args) -> int:
    paths: list[str] = get_default_paths(args.fuchsia_dir, args.build_dir)

    if not os.path.exists(args.python_path_json):
        print(
            "Failed to find extra Python paths. Run `fx build` and re-run this script.",
            file=sys.stderr,
        )
        return 1

    with open(args.python_path_json, "r") as f:
        paths.extend(json.load(f))

    python_path_dir = os.path.dirname(args.python_path_json)

    print(":".join(map(lambda p: os.path.join(python_path_dir, p), paths)))

    return 0


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=HELP)

    parser.add_argument(
        "--python-path-json",
        type=str,
        required=True,
        help="Path to a JSON file containing paths of libraries that should be added to PYTHONPATH",
    )
    parser.add_argument(
        "--fuchsia-dir",
        type=str,
        required=True,
        help="Path to the Fuchsia directory",
    )
    parser.add_argument(
        "--build-dir",
        type=str,
        required=True,
        help="Path to output build directory",
    )

    args = Args(**parser.parse_args().__dict__)

    sys.exit(main(args))
