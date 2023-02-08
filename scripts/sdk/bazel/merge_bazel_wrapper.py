# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Recursively copy all files from --source-dir to the dest-dir therein.
If a meta/manifest.json is found, the arch target will be merged into that file
(rather than replacing it).
"""


import argparse
import filecmp
import os
import shutil
import json
import sys


def merge_new_manifest(manifest_json, new_manifest):
    if manifest_json is None:
        manifest_json = new_manifest
    else:
        manifest_json["arch"]["target"].extend(new_manifest["arch"]["target"])

    return manifest_json


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("--source-dir")
    parser.add_argument("--dest-dir")
    args = parser.parse_args()
    manifest_json = None
    unmergable_files = []

    for directory in os.listdir(args.source_dir):
        directory = os.path.join(args.source_dir, directory)
        for root, _, files in os.walk(directory):
            for f in files:
                source_file = os.path.join(root, f)
                relpath = os.path.relpath(source_file, directory)
                dest_file = os.path.join(args.dest_dir, relpath)
                if relpath == "meta/manifest.json":
                    with open(source_file, "r") as manifest_file:
                        new_manifest = json.load(manifest_file)
                    manifest_json = merge_new_manifest(
                        manifest_json, new_manifest)
                    continue
                if os.path.exists(dest_file):
                    # Coherence check: Ensure that files with the same path have equal content.
                    if not filecmp.cmp(source_file, dest_file):
                        unmergable_files.append(relpath)
                    continue
                if not os.path.exists(os.path.dirname(dest_file)):
                    os.makedirs(os.path.dirname(dest_file))
                shutil.copy(source_file, dest_file)
    assert not unmergable_files, f"The following files cannot be merged because of conflicting content:{'\n'.join(unmergable_files)}"

    # Write meta/manifest.json
    dest_file = os.path.join(dest_dir, "meta/manifest.json")
    if not os.path.exists(os.path.dirname(dest_file)):
        os.makedirs(os.path.dirname(dest_file))
    with open(dest_file, "w") as f:
        json.dump(manifest_json, f, indent=2)


if __name__ == "__main__":
    main()
