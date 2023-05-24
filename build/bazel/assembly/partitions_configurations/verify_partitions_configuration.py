#!/usr/bin/env fuchsia-vendored-python

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import difflib
import hashlib
import json
import os
import sys


def file_sha1(path):
    sha1 = hashlib.sha1()
    with open(path, "rb") as f:
        sha1.update(f.read())
    return sha1.hexdigest()


def rewrite_legacy_ninja_build_outputs_path(p):
    return p.replace(
        'workspace/external/legacy_ninja_build_outputs',
        'output_base/external/legacy_ninja_build_outputs',
    )


def replace_with_file_hash(dict, key, root_dir, extra_files_read):
    p = os.path.join(root_dir, dict[key])
    # Note this is only necessary in partitions config verification because it
    # references file from legacy_ninja_build_outputs. A find-and-replace is
    # sufficnet since these verifications are meant to be temporary until we
    # finish GN->Bazel migration for assembly.
    p = rewrite_legacy_ninja_build_outputs_path(p)
    # Follow links for depfile entry. See https://fxbug.dev/122513.
    p = os.path.relpath(os.path.realpath(p))
    dict[key] = file_sha1(p)
    extra_files_read.append(p)


def normalize(config, root_dir, extra_files_read):
    for section in ("bootloader_partitions", "bootstrap_partitions"):
        if section not in config:
            continue
        for part in config[section]:
            replace_with_file_hash(part, "image", root_dir, extra_files_read)

    if "unlock_credentials" in config:
        real_paths = [
            os.path.join(root_dir, p) for p in config["unlock_credentials"]
        ]
        config["unlock_credentials"] = [file_sha1(p) for p in real_paths]
        extra_files_read += real_paths


def main():
    parser = argparse.ArgumentParser(
        description="Compares assembly partitions configurations")
    parser.add_argument(
        "--partitions_config1", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--root_dir1",
        help="Directory where paths in --partitions_config1 are relative to",
        required=True,
    )
    parser.add_argument(
        "--partitions_config2", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--root_dir2",
        help="Directory where paths in --partitions_config2 are relative to",
        required=True,
    )
    parser.add_argument("--depfile", type=argparse.FileType("w"), required=True)
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)
    args = parser.parse_args()

    partitions_config1 = json.load(args.partitions_config1)
    partitions_config2 = json.load(args.partitions_config2)

    extra_files_read = []
    normalize(partitions_config1, args.root_dir1, extra_files_read)
    normalize(partitions_config2, args.root_dir2, extra_files_read)

    partitions_config1_str = json.dumps(
        partitions_config1, sort_keys=True, indent=2).splitlines()
    partitions_config2_str = json.dumps(
        partitions_config2, sort_keys=True, indent=2).splitlines()

    diff = difflib.unified_diff(
        partitions_config1_str,
        partitions_config2_str,
        fromfile=args.partitions_config1.name,
        tofile=args.partitions_config2.name,
        lineterm="",
    )

    diffstr = "\n".join(diff)
    args.output.write(diffstr)

    args.depfile.write(
        "{}: {}".format(args.output.name, " ".join(extra_files_read)))

    if len(diffstr) != 0:
        print(
            "Error: non-empty diff between canonical json"
            f" representations:\n{diffstr}")
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
