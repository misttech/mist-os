#!/usr/bin/env fuchsia-vendored-python
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import sys


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Create a flat list of files included in the images. This is used to inform infrastructure what files to upload"
    )
    parser.add_argument(
        "--assembly-input-bundles", type=argparse.FileType("r"), required=True
    )
    parser.add_argument("--sources", type=str, nargs="*")
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)
    args = parser.parse_args()

    # The files to put in the output with source mapped to destination.
    file_mapping = {}

    # Add a file or directory path to one of the lists, relative to CWD.
    # The destination is the path when placed inside "built/artifacts".
    # If the path is prefixed with ../../, the prefix is removed.
    def add_source(source: str) -> None:
        # Absolute paths are not portable out-of-tree, therefore if a file is
        # using an absolute path we throw an error.
        if os.path.isabs(source):
            raise Exception("Absolute paths are not allowed", source)

        source = os.path.relpath(source, os.getcwd())
        prefix = "../../"
        if source.startswith(prefix):
            destination = source[len(prefix) :]
        else:
            destination = os.path.join("built/artifacts", source)
        file_mapping[source] = destination

    # Add the assembly input bundles
    assembly_input_bundles = json.load(args.assembly_input_bundles)
    for bundle_entry in assembly_input_bundles:
        dirname, basename = os.path.split(bundle_entry["path"])
        if basename.endswith(".tgz"):
            basename = basename[:-4]
        add_source(os.path.join(dirname, basename))

    # Add any additional sources to copy.
    if args.sources:
        for source in args.sources:
            add_source(source)

    # Convert the map into a list of maps.
    files = []
    for src, dest in file_mapping.items():
        files.append(
            {
                "source": src,
                "destination": dest,
            }
        )

    # Write the list.
    json.dump(files, args.output, indent=2)

    return 0


if __name__ == "__main__":
    sys.exit(main())
