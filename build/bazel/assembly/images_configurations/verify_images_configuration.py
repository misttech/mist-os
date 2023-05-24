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


def replace_with_file_hash(dict, key, root_dir, extra_files_read):
    p = os.path.join(root_dir, dict[key])
    dict[key] = file_sha1(p)
    # Follow links for depfile entry. See https://fxbug.dev/122513.
    p = os.path.relpath(os.path.realpath(p))
    extra_files_read.append(p)


def normalize(config, root_dir, extra_files_read):
    for image in config["images"]:
        for key in ("key", "key_metadata"):
            if key in image:
                replace_with_file_hash(image, key, root_dir, extra_files_read)

        if "postprocessing_script" in image:
            replace_with_file_hash(
                image["postprocessing_script"],
                "path",
                root_dir,
                extra_files_read,
            )

        # `filesystems` is a unordered list of all filesystems used.
        if "filesystems" in image:
            image["filesystems"].sort(key=lambda fs: fs["name"])
            for fs in image["filesystems"]:
                # Explicitly set defaults for compress for different filesystem
                # for consistency between GN and Bazel generated images configs.
                if fs["type"] == "blobfs" and "compress" not in fs:
                    fs["compress"] = True

        if "outputs" in image:
            for output in image["outputs"]:
                # Explicitly set defaults for compress for different filesystem
                # for consistency between GN and Bazel generated images configs.
                if output["type"] in {"standard", "nand"
                                     } and "compress" not in output:
                    output["compress"] = False


def main():
    parser = argparse.ArgumentParser(
        description="Compares assembly images configurations")
    parser.add_argument(
        "--images_config1", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--root_dir1",
        help="Directory where paths in --images_config1 are relative to",
        required=True,
    )
    parser.add_argument(
        "--images_config2", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--root_dir2",
        help="Directory where paths in --images_config2 are relative to",
        required=True,
    )
    parser.add_argument("--depfile", type=argparse.FileType("w"), required=True)
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)
    args = parser.parse_args()

    images_config1 = json.load(args.images_config1)
    images_config2 = json.load(args.images_config2)

    extra_files_read = []
    normalize(images_config1, args.root_dir1, extra_files_read)
    normalize(images_config2, args.root_dir2, extra_files_read)

    images_config1_str = json.dumps(
        images_config1, sort_keys=True, indent=2).splitlines()
    images_config2_str = json.dumps(
        images_config2, sort_keys=True, indent=2).splitlines()

    diff = difflib.unified_diff(
        images_config1_str,
        images_config2_str,
        fromfile=args.images_config1.name,
        tofile=args.images_config2.name,
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
