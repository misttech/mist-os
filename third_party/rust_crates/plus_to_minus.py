#!/usr/bin/env fuchsia-vendored-python
#
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os
import sys
import shutil


def plus_to_minus(dir):
    os.chdir(dir)
    subdirs = sorted(os.listdir(os.getcwd()))
    for subdir in subdirs:
        if '+' in subdir:
            shutil.move(subdir, subdir.replace('+', '-'))


def main():
    parser = argparse.ArgumentParser(
        "Rename direcotries with plus signs in their names",
    )
    parser.add_argument(
        "--vendor-dir",
        help="Directory containing the vendored crates",
        default=os.getcwd(),
    )
    args = parser.parse_args()
    plus_to_minus(args.vendor_dir)


if __name__ == "__main__":
    main()
