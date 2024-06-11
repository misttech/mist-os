#!/usr/bin/env fuchsia-vendored-python
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import sys
from typing import Any, TextIO

from depfile import DepFile


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Create a flat list of files included in the images. This is used to inform infrastructure what files to upload"
    )
    parser.add_argument(
        "--product-config", type=argparse.FileType("r"), nargs="+"
    )
    parser.add_argument(
        "--assembly-input-bundles", type=argparse.FileType("r"), required=True
    )
    parser.add_argument("--sources", type=str, nargs="*")
    parser.add_argument("--output", type=argparse.FileType("w"), required=True)
    parser.add_argument("--depfile", type=argparse.FileType("w"), required=True)
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

    # Add a package and all the included blobs.
    manifests_for_depfile: list[str] = []

    def add_package(entry: dict[str, str | list[dict[str, str]]]) -> None:
        if isinstance(entry, str):
            manifest = entry["manifest"]
            manifests_for_depfile.append(manifest)
            add_source(manifest)
            with open(manifest, "r") as f:
                package_manifest = json.load(f)
                for blob in package_manifest.get("blobs", []):
                    add_source(blob["source_path"])
        for config in entry.get("config_data", []):
            add_source(config["source"])  # type: ignore[index]

    def add_product_config(product_config: TextIO) -> None:
        add_source(product_config.name)
        product_config_data: dict[str, Any] = json.load(product_config)
        if "product" in product_config_data:
            product = product_config_data["product"]
            if "packages" in product:
                packages = product["packages"]
                for package in packages.get("base", []):
                    add_package(package)
                for package in packages.get("cache", []):
                    add_package(package)

    if args.product_config:
        for product_config_file in args.product_config:
            add_product_config(product_config_file)

    # Add the assembly input bundles
    assembly_input_bundles = json.load(args.assembly_input_bundles)
    for bundle_entry in assembly_input_bundles:
        dirname, basename = os.path.split(bundle_entry["path"])
        if basename.endswith(".tgz"):
            basename = basename[:-4]
        add_source(os.path.join(dirname, basename))

    # Add any additional sources to copy.
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

    # Write a depfile with any opened package manifests.
    if manifests_for_depfile:
        depfile = DepFile(args.output.name)
        depfile.update(manifests_for_depfile)
        depfile.write_to(args.depfile)

    # Write the list.
    json.dump(files, args.output, indent=2)

    return 0


if __name__ == "__main__":
    sys.exit(main())
