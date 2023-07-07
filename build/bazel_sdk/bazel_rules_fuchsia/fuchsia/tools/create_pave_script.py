# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Create pave.sh script based on generated product bundle."""

import argparse
import json
import os

PAVE_SCRIPT_TEMPLATE = """#!/bin/sh
dir="$(dirname "$0")"
set -x

exec {bootserver} --board_name {board_name} -w 10 {additional_args} "$@"

"""


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--product-bundle',
        help='Path to product_bundle.json file.',
        required=True,
    )
    parser.add_argument(
        '--pave-script-path',
        help='path to created pave script.',
        required=True,
    )
    return parser.parse_args()


def main():
    args = parse_args()
    with open(args.product_bundle) as f:
        product_bundle = json.load(f)

    pb_dir = os.path.basename(os.path.dirname(args.product_bundle))

    def rel_path(path):
        return "${dir}/" + pb_dir + "/" + path

    bootserver = rel_path("bootserver")
    board_name = product_bundle["partitions"]["hardware_revision"]

    parameters = []
    for bootloader in product_bundle["partitions"]["bootloader_partitions"]:
        if bootloader["type"] == "bl2":
            parameters += ["--firmware-bl2", rel_path(bootloader["image"])]
        else:
            parameters += ["--firmware", rel_path(bootloader["image"])]

    for image in product_bundle["system_a"]:
        if image["type"] == "blk" and os.path.basename(
                image["path"]) == "fvm.sparse.blk":
            parameters += ["--fvm", rel_path(image["path"])]
        if image["type"] == "blk" and os.path.basename(
                image["path"]) == "fxfs.sparse.blk":
            parameters += ["--fxfs", rel_path(image["path"])]
        if image["type"] == "zbi":
            parameters += ["--zircona", rel_path(image["path"])]
        if image["type"] == "vbmeta":
            parameters += ["--vbmetaa", rel_path(image["path"])]

    if "system_r" in product_bundle and product_bundle["system_r"] != None:
        for image in product_bundle["system_r"]:
            if image["type"] == "zbi":
                parameters += ["--zirconr", rel_path(image["path"])]
            if image["type"] == "vbmeta":
                parameters += ["--vbmetar", rel_path(image["path"])]

    pave_script = PAVE_SCRIPT_TEMPLATE.format(
        bootserver=bootserver,
        board_name=board_name,
        additional_args=" ".join(parameters))

    with open(args.pave_script_path, "w") as f:
        f.write(pave_script)


if __name__ == '__main__':
    main()
