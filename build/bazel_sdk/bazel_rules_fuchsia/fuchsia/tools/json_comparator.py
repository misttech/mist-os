# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Tool to compare two json file against golden file regardless of order."""

import argparse
import json
from pathlib import Path


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--generated",
        help="Path to generated file",
        required=True,
    )
    parser.add_argument(
        "--golden",
        help="Path to the golden file",
        required=True,
    )
    return parser.parse_args()


def sorting(item):
    if isinstance(item, dict):
        return sorted((key, sorting(values)) for key, values in item.items())
    if isinstance(item, list):
        return sorted(sorting(x) for x in item)
    return item


def main():
    args = parse_args()

    with open(args.generated, "r") as f:
        gen = json.load(f)
    with open(args.golden, "r") as f:
        golden = json.load(f)
    if sorting(gen) != sorting(golden):
        print("Comparison failure!. \n Golden:\n" + str(golden) +
              "\nGenerated:\n" + str(gen))
        exit(1)


if __name__ == "__main__":
    main()
