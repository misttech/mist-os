#!/usr/bin/env python3
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import shutil


def parse_args():
    """Parses arguments."""
    parser = argparse.ArgumentParser()

    parser.add_argument(
        "--config-file",
        type=str,
        help="A path to the board configuration file.",
        required=True,
    )
    parser.add_argument(
        "--output-dir",
        type=str,
        help="The directory of output board configuration",
        required=True,
    )
    parser.add_argument(
        "--script-inputs",
        type=str,
        help="string formed script inputs dictionary which map from source to destination inside board configuration.",
        default="{}",
    )

    return parser.parse_args()


def main():
    args = parse_args()
    output_dir = args.output_dir
    with open(args.config_file, "r") as f:
        config = json.load(f)

    def copy_artifacts(sub_config, update_field, dest_dir):
        src = os.path.join(
            os.path.dirname(args.config_file), sub_config.get(update_field)
        )
        relative_dest = os.path.join(dest_dir, os.path.basename(src))
        dest = os.path.join(output_dir, relative_dest)
        dest_dir = os.path.dirname(dest)

        if not os.path.exists(dest_dir):
            os.makedirs(dest_dir)

        shutil.copyfile(src, dest)
        sub_config[update_field] = relative_dest

    # Copy the devicetree
    if "devicetree" in config:
        copy_artifacts(config, "devicetree", "devicetree")
    if "devicetree_overlay" in config:
        copy_artifacts(config, "devicetree_overlay", "devicetree")

    # Copy the vbmeta
    if "filesystems" in config and "vbmeta" in config["filesystems"]:
        copy_artifacts(config["filesystems"]["vbmeta"], "key", "vbmeta")
        copy_artifacts(
            config["filesystems"]["vbmeta"], "key_metadata", "vbmeta"
        )

    # Copy the BIBs
    if "input_bundles" in config:
        input_bundles = []
        input_bundles_dir = os.path.join(output_dir, "input_bundles")
        if not os.path.exists(input_bundles_dir):
            os.makedirs(input_bundles_dir)

        for input_bundle in config["input_bundles"]:
            src = os.path.join(os.path.dirname(args.config_file), input_bundle)
            relative_dest = os.path.join(
                "input_bundles", os.path.basename(input_bundle)
            )
            dest = os.path.join(output_dir, relative_dest)
            shutil.copytree(src, dest)
            input_bundles.append(relative_dest)

        config["input_bundles"] = input_bundles

    # Copy the post processing script inputs
    script_maps = json.loads(args.script_inputs)
    for source, relative_dest in script_maps.items():
        dest = os.path.join(output_dir, "scripts", relative_dest)
        dest_dir = os.path.dirname(dest)

        if not os.path.exists(dest_dir):
            os.makedirs(dest_dir)
        shutil.copyfile(source, dest)

    # Write the board configuration json
    board_config_path = os.path.join(output_dir, "board_configuration.json")
    with open(board_config_path, "w") as f:
        json.dump(config, f, indent=4)


if __name__ == "__main__":
    main()
