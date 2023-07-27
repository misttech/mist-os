#!/usr/bin/env python3.8

# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import difflib
import hashlib
import json
import json5
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
    if "filesystems" in config:
        filesystems = config["filesystems"]

        if "vbmeta" in filesystems:
            vbmeta = filesystems["vbmeta"]
            for key in ("key", "key_metadata"):
                if key in vbmeta:
                    replace_with_file_hash(
                        vbmeta, key, root_dir, extra_files_read)

        if "zbi" in filesystems:
            zbi = filesystems["zbi"]
            if "postprocessing_script" in zbi:
                replace_with_file_hash(
                    zbi["postprocessing_script"],
                    "path",
                    root_dir,
                    extra_files_read,
                )


def main():
    parser = argparse.ArgumentParser(
        description="Compares generated board configurations with a golden")
    parser.add_argument(
        "--generated_board_config", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--generated_root_dir",
        help="Directory where paths in --generated_board_config are relative to",
        required=True,
    )
    parser.add_argument(
        "--golden_json5", type=argparse.FileType("r"), required=True)
    parser.add_argument(
        "--golden_root_dir",
        help="Directory where paths in --golden_json5 are relative to",
        required=True,
    )
    parser.add_argument("--depfile", type=argparse.FileType("w"), required=True)
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)
    args = parser.parse_args()

    generated = json.load(args.generated_board_config)
    golden = json5.load(args.golden_json5)

    extra_files_read = []
    normalize(generated, args.generated_root_dir, extra_files_read)
    normalize(golden, args.golden_root_dir, extra_files_read)

    generated_str = json.dumps(generated, sort_keys=True, indent=2).splitlines()
    golden_str = json.dumps(golden, sort_keys=True, indent=2).splitlines()

    diff = difflib.unified_diff(
        generated_str,
        golden_str,
        fromfile=args.generated_board_config.name,
        tofile=args.golden_json5.name,
        lineterm="",
    )

    diffstr = "\n".join(diff)
    args.output.write(diffstr)

    args.depfile.write(
        "{}: {}".format(args.output.name, " ".join(extra_files_read)))

    if len(diffstr) != 0:
        print(
            "Error: non-empty diff between board configurations"
            f" representations:\n{diffstr}",
            file=sys.stderr)
        return 1

    return 0


if __name__ == "__main__":
    sys.exit(main())
