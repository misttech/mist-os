#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import difflib
import json
import sys


def main():
  parser = argparse.ArgumentParser(
      description="Compares drivers lists and outputs diffs"
  )
  parser.add_argument(
      "--drivers-list1", type=argparse.FileType("r"), required=True
  )
  parser.add_argument(
      "--drivers-list2", type=argparse.FileType("r"), required=True
  )
  parser.add_argument("--output", type=argparse.FileType("w"), required=True)
  args = parser.parse_args()

  drivers_list1 = sorted(args.drivers_list1.readlines())
  drivers_list2 = sorted(args.drivers_list2.readlines())
  diff = difflib.unified_diff(
      drivers_list1,
      drivers_list2,
      args.drivers_list1.name,
      args.drivers_list2.name,
      lineterm="",
  )
  diffstr = "\n".join(diff)
  args.output.write(diffstr)

  if len(diffstr) != 0:
    print(f"Error: non-empty diff\n\n{diffstr}", file=sys.stderr)
    return 1


if __name__ == "__main__":
  sys.exit(main())
