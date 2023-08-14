#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
'''Transpose a Fuchsia paving script, replacing SRC_DIR with DST_DIR in
its commands.'''

import argparse
import os
import sys


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument(
        '--input',
        type=argparse.FileType('r'),
        required=True,
        help='Path to input script.')
    parser.add_argument(
        '--output', required=True, help='Path to output script.')
    parser.add_argument(
        '--src-dir', required=True, help='Name of source directory.')
    parser.add_argument(
        '--dst-dir',
        default='product_bundle',
        help='Name of destination directory.')

    args = parser.parse_args()

    input_data = args.input.read()

    # Replace {dir}/<src_dir>/ with {dir}/<dst_dir>/
    output_data = input_data.replace(
        '$dir/' + args.src_dir, '$dir/' + args.dst_dir)

    with open(args.output, 'w') as f:
        f.write(output_data)

    # Make the output executable
    os.chmod(args.output, 0o755)
    return 0


if __name__ == "__main__":
    sys.exit(main())
