#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import shutil
import subprocess
import sys
from os import path
from typing import Any


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Runs clidoc for the commands found in the sdk molecule manifest.",
    )
    parser.add_argument(
        "--clidoc",
        type=str,
        required=True,
        help="clidoc executable",
    )
    parser.add_argument(
        "--input",
        type=str,
        required=True,
        help="SDK molecule manifest",
    )
    parser.add_argument(
        "--output",
        type=str,
        required=True,
        help="Output location where the contents should be listed.",
    )

    parser.add_argument(
        "--depfile",
        type=str,
        required=True,
        help="Depfile location.",
    )
    parser.add_argument(
        "--sdk-root",
        type=str,
        required=True,
    )
    parser.add_argument(
        "--sdk-manifest",
        type=str,
        required=True,
    )
    parser.add_argument(
        "--isolate-dir",
        type=str,
        required=True,
    )
    parser.add_argument(
        "--subtool-manifest",
        type=str,
        required=True,
    )
    parser.add_argument("--excludes", type=str, nargs="*")

    args = parser.parse_args()

    if not os.path.exists(args.input):
        raise ValueError(f"{args.input} does not exist.")

    contents = read_sdk_molecule(args.input)

    run_clidoc(args, contents)
    return 0


def run_clidoc(args, cmd_list):
    outdir = path.join(args.isolate_dir, "docs")
    if path.exists(outdir):
        shutil.rmtree(outdir)
    os.makedirs(outdir)

    cmds = [c.split("/")[-1] for c in cmd_list]
    cmds = [c for c in cmds if c not in args.excludes]

    clidoc_args = [
        args.clidoc,
        "--quiet",
        "--out-dir",
        outdir,
        "--archive-path",
        args.output,
        "--depfile",
        args.depfile,
        "--sdk-manifest",
        args.sdk_manifest,
        "--isolate-dir",
        args.isolate_dir,
        "--subtool-manifest",
        args.subtool_manifest,
    ] + cmds
    subprocess.run(clidoc_args)


def read_sdk_molecule(manifest: str) -> list[Any]:
    with open(manifest) as f:
        data = json.load(f)

    cmds = []
    for atom in data["atoms"]:
        if atom["type"] == "host_tool":
            for f in atom["files"]:
                if not f["source"].endswith(".json"):
                    cmds += [f["source"]]

    return cmds


if __name__ == "__main__":
    sys.exit(main())
