# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Rebase paths in flash.json to be relative to product bundle."""

import argparse
import json
import os


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--product-bundle',
        help='Path to product_bundle dir.',
        required=True,
    )
    parser.add_argument(
        '--flash-manifest-path',
        help='path to created flash manifest.',
        required=True,
    )
    return parser.parse_args()


def main():
    args = parse_args()
    pb_dir = args.product_bundle
    with open(os.path.join(pb_dir, "flash.json")) as f:
        flash_manifest = json.load(f)

    def rebase_path(path: str):
        return os.path.relpath(path, os.path.dirname(pb_dir))

    def rebase_file_paths(obj):
        if isinstance(obj, dict):
            for key in obj:
                if key == "path":
                    obj[key] = rebase_path(obj[key])
                elif key == "credentials":
                    obj[key] = [rebase_path(path) for path in obj[key]]
                else:
                    rebase_file_paths(obj[key])
        if isinstance(obj, list):
            for item in obj:
                rebase_file_paths(item)

    rebase_file_paths(flash_manifest)
    with open(args.flash_manifest_path, "w") as f:
        json.dump(flash_manifest, f, indent=4)


if __name__ == '__main__':
    main()
