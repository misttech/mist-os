#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import shutil
import sys


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "manifest",
        type=argparse.FileType("r"),
        help="JSON manifest file",
        nargs=1,
    )
    parser.add_argument(
        "output",
        help="Output directory",
        nargs=1,
    )
    parser.add_argument(
        "--depfile",
        type=argparse.FileType("w"),
        help="Depfile",
    )
    args = parser.parse_args()
    [manifest_file] = args.manifest
    [output_dir_name] = args.output

    files = {
        os.path.join(output_dir_name, entry["destination"]): entry["source"]
        for entry in json.load(manifest_file)
    }
    manifest_file.close()

    if os.path.exists(output_dir_name):
        shutil.rmtree(output_dir_name)

    for dst, src in files.items():
        os.makedirs(os.path.dirname(dst), exist_ok=True)
        os.link(src, dst)

    inputs = " ".join(list(files.values()) + [manifest_file.name])
    args.depfile.write(f"{output_dir_name}: {inputs}\n")
    args.depfile.close()

    return 0


if __name__ == "__main__":
    sys.exit(main())
