# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Preprocesses version_history.json before including it in the IDK."""

import argparse
import pathlib
import json
from typing import Any

import generate_version_history


def main() -> None:
    parser = argparse.ArgumentParser(__doc__, allow_abbrev=False)
    parser.add_argument(
        "--input",
        type=pathlib.Path,
        required=True,
        help="Unprocessed version of version_history.json",
    )
    parser.add_argument(
        "--output",
        type=pathlib.Path,
        required=True,
        help="Generated version of version_history.json",
    )

    args = parser.parse_args()

    with args.input.open() as f:
        version_history = json.load(f)

    generate_version_history.replace_special_abi_revisions(version_history)

    with args.output.open("w") as f:
        json.dump(version_history, f, indent=2)


if __name__ == "__main__":
    main()
