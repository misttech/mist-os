#!/usr/bin/env python3

# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Generate an OOT workspace that can be used for testing
things in-tree as if it lives in an OOT repository.
"""

import argparse
import sys
import shutil
import os
import json
from pathlib import Path


def symlink(src, dest: str):
    outs = []
    ins = []

    src = os.path.normpath(src)
    dest = os.path.normpath(dest)

    target_is_directory = dest[-1] == "/"
    dest = dest.removesuffix("/")

    if not os.path.exists(dest) and not os.path.islink(dest):
        os.symlink(src, dest, target_is_directory=target_is_directory)
    return [src], [dest]


def copy(src, dest):
    if os.path.isdir(src):
        return copy_dir(src, dest)
    return copy_file(src, dest)


def copy_file(src, dest):
    src = os.path.normpath(src)
    dest = os.path.normpath(dest)
    if os.path.exists(dest) or os.path.lexists(dest):
        return [dest], [src]

    parent = os.path.dirname(dest)
    if not os.path.exists(parent):
        os.makedirs(parent)

    shutil.copyfile(src, dest, follow_symlinks=False)

    if dest.endswith((".py", ".sh", "bazel")) and os.path.exists(dest):
        os.chmod(dest, 0o755)
    return [dest], [src]


def copy_dir(src, dest):
    outputs = []
    inputs = []

    for root, dirnames, filenames in os.walk(src, followlinks=True):
        dirnames[:] = [d for d in dirnames if not d.startswith(".")]

        for filename in filenames:
            in_file = os.path.join(root, filename)
            rel_path = os.path.join(
                root.removeprefix(src).removeprefix("/"), filename
            )
            out_file = os.path.join(dest, rel_path)
            outs, ins = copy_file(in_file, out_file)
            outputs += outs
            inputs += ins

    return outputs, inputs


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--copy-list", required=True)
    parser.add_argument("--symlink-list", required=True)
    parser.add_argument("--depfile", required=True)
    args = parser.parse_args()

    outputs = []
    inputs = []

    with open(args.copy_list) as j:
        for group in json.load(j):
            for infile, outfile in zip(group["inputs"], group["outputs"]):
                ins, outs = copy(infile, outfile)
                inputs += ins
                outputs += outs

    with open(args.symlink_list) as j:
        for group in json.load(j):
            for infile, outfile in zip(group["inputs"], group["outputs"]):
                ins, outs = symlink(infile, outfile)
                outputs += outs
                inputs += ins

    Path(args.depfile).write_text(f"{' '.join(outputs)}: {' '.join(inputs)}")

    return 0


if __name__ == "__main__":
    sys.exit(main())
