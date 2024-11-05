# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Collects partitions_config.json and referenced files into a portable directory.
"""

import argparse
import json
import os
import sys
from pathlib import Path
from typing import List, Optional, Tuple


def parse_args() -> argparse.Namespace:
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--partitions-config",
        type=argparse.FileType("r"),
        help="cwd-relative partitions_config.json file path",
        required=True,
    )
    parser.add_argument(
        "--output",
        help="the output directory to collect the partitions config",
        required=True,
    )
    parser.add_argument(
        "--depfile",
        help="If specified, writes a depfile",
        required=True,
    )
    return parser.parse_args()


def collect_partitions_config(
    root_dir: str, partitions_config: argparse.FileType
) -> Tuple[List[str], List[str]]:
    config = json.load(partitions_config)
    inputs = [partitions_config.name]
    outputs = []

    def add_symlink(category: str, file: str) -> str:
        rel_dest = os.path.join(category, os.path.basename(file))
        dest = os.path.join(root_dir, rel_dest)
        target = os.path.realpath(file)

        # If the existing destination file isn't a symlink pointing to
        # the origin file's real path, then remove the existing
        # destination file
        if os.path.lexists(dest) and os.path.realpath(dest) != target:
            os.unlink(dest)

        # If the destination file doesn't exist (or has been deleted)
        # then make it a symlink to the the origin file.
        if not os.path.exists(dest):
            os.makedirs(os.path.dirname(dest), exist_ok=True)
            os.symlink(target, dest)
        inputs.append(file)
        outputs.append(dest)
        return rel_dest

    for partition in config["bootloader_partitions"]:
        partition["image"] = add_symlink("bootloaders", partition["image"])
    for partition in config["bootstrap_partitions"]:
        partition["image"] = add_symlink("bootstrap", partition["image"])
    config["unlock_credentials"] = [
        add_symlink("credentials", cred)
        for cred in config["unlock_credentials"]
    ]
    with open(os.path.join(root_dir, "partitions_config.json"), "w") as f:
        json.dump(config, f)
    return inputs, outputs


def main() -> Optional[int]:
    args = parse_args()

    inputs, outputs = collect_partitions_config(
        args.output, args.partitions_config
    )
    Path(args.depfile).write_text(f"{' '.join(outputs)}: {' '.join(inputs)}")


if __name__ == "__main__":
    sys.exit(main())
