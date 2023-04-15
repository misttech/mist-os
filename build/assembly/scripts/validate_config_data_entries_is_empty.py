#!/usr/bin/env python3.8
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import sys


def main():
    parser = argparse.ArgumentParser(
        description=
        "Parse config_data package entries file and validate it's empty")
    parser.add_argument(
        "--metadata-walk-results",
        type=argparse.FileType('r'),
        required=True,
        help="Path to generated_file() output.")
    parser.add_argument(
        "--output",
        required=True,
        help="Path for the script to write its output to.")
    args = parser.parse_args()

    entries: List[Any] = json.load(args.metadata_walk_results)

    if entries:
        for entry in entries:
            print(f"{entry}", file=sys.stderr)
        return -1
    with open(args.output, "w") as output:
        print("validated", file=output)
    return 0


if __name__ == '__main__':
    sys.exit(main())