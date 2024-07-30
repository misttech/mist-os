#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import shutil
import sys
import time

import depfile


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Create a platform artifacts directory"
    )
    parser.add_argument(
        "--aib-list", type=argparse.FileType("r"), required=True
    )
    parser.add_argument("--outdir", type=str, required=True)
    parser.add_argument("--depfile", type=argparse.FileType("w"), required=True)
    args = parser.parse_args()

    shutil.rmtree(args.outdir)
    os.mkdir(args.outdir)

    artifacts = json.load(args.aib_list)
    deps = []
    for artifact in artifacts:
        path = artifact["path"]
        deps.append(os.path.join(path, "assembly_config.json"))
        src = os.path.realpath(path)
        dst = os.path.join(args.outdir, os.path.basename(path))
        try:
            os.link(src, dst)
        except OSError:
            shutil.copytree(src, dst)

    stamp = os.path.join(args.outdir, "stamp")
    with open(stamp, "w") as f:
        f.write(str(time.time()))

    depfile.DepFile.from_deps(stamp, deps).write_to(args.depfile)

    return 0


if __name__ == "__main__":
    sys.exit(main())
