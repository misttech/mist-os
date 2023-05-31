#!/usr/bin/env python3.8

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import difflib
import json
import json5
import os
import sys


def main():
    parser = argparse.ArgumentParser(
        description="Compares generated board configurations with a golden")
    parser.add_argument(
        "--generated_board_config", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--golden_json5", type=argparse.FileType("r"), required=True)
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)
    args = parser.parse_args()

    generated = json.load(args.generated_board_config)
    golden = json5.load(args.golden_json5)

    generated_str = json.dumps(generated, sort_keys=True, indent=2).splitlines()
    golden_str = json.dumps(golden, sort_keys=True, indent=2).splitlines()

    diff = difflib.unified_diff(
        generated_str,
        golden_str,
        fromfile=args.generated_board_config.name,
        tofile=args.golden_json5.name,
        lineterm="",
    )

    diffstr = "\n".join(diff)
    args.output.write(diffstr)

    if len(diffstr) != 0:
        print(
            "Error: non-empty diff between board configurations"
            f" representations:\n{diffstr}", file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
