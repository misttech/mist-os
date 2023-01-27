# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Rebase paths in package manifest to be relative to artifact_base_path."""

import argparse
import json
import os


def parse_args():
    """Parses command-line arguments."""
    parser = argparse.ArgumentParser()
    parser.add_argument(
        '--package-manifest',
        type=argparse.FileType('r'),
        help='original package manifest file',
        required=True,
    )
    parser.add_argument(
        '--updated-package-manifest',
        type=argparse.FileType('w'),
        help='output package manifest file',
        required=True,
    )
    parser.add_argument(
        '--relative-base',
        help='Path to artifact base',
        required=True,
    )
    return parser.parse_args()


def main():
    args = parse_args()
    base_path = args.relative_base
    package_manifest_json = json.load(args.package_manifest)

    for blob in package_manifest_json['blobs']:
        blob['source_path'] = os.path.relpath(blob['source_path'], base_path)

    json.dump(package_manifest_json, args.updated_package_manifest, indent=2)


if __name__ == '__main__':
    main()
