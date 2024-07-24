# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import gzip
from argparse import ArgumentParser

parser = ArgumentParser()
parser.add_argument(
    "--input", help="path to the file to compress", required=True
)
parser.add_argument(
    "--output",
    help="Path to the compressed file",
    required=True,
)


def main():
    args = parser.parse_args()
    with open(args.input, "rb") as F:
        with gzip.open(args.output, "wb") as G:
            G.write(F.read())


if __name__ == "__main__":
    main()
