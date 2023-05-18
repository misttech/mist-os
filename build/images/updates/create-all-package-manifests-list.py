#!/usr/bin/env python3.8
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os.path
import sys
import tempfile


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('output', help='Write to this file')
    parser.add_argument('paths', nargs='+', help='package manifest lists')
    args = parser.parse_args()

    out = tempfile.NamedTemporaryFile(
        'w', dir=os.path.dirname(args.output), delete=False)
    try:
        for path in args.paths:
            with open(path) as f:
                for line in f:
                    out.write(line)

        out.close()

        os.replace(out.name, args.output)
    finally:
        try:
            os.unlink(out.name)
        except FileNotFoundError:
            pass


if __name__ == "__main__":
    sys.exit(main())
