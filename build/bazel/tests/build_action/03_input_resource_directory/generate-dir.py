#!/usr/bin/env python3
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Populate a directory with arbitrary files."""

import argparse
import os
import shutil
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--stamp", required=True, help="Output stamp file")
    parser.add_argument("--output-dir", required=True, help="Output directory")

    args = parser.parse_args()

    output_dir = args.output_dir
    if os.path.exists(output_dir):
        shutil.rmtree(output_dir)

    os.makedirs(output_dir)

    def write_file(name, content):
        out_file = os.path.join(output_dir, name)
        with open(out_file, 'w') as f:
            f.write(content)

    write_file('a.txt', 'Hello First!')
    write_file('b.lst', 'Hello\nSecond!\n')
    write_file('c.json', '[ "Hello", "Third!" ]')

    with open(args.stamp, 'w') as f:
        f.write('done!\n')

    return 0


if __name__ == "__main__":
    sys.exit(main())
