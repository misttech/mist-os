#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Generates the metadata file and sources file for including Python E2E tests
in the IDK. For use in sdk_python_mobly_test() template.

Defines the IDK directory structure of a Python E2E test as follows:
```
sdk://python
        └── <test_name>
            ├── meta.json
            └── <api_level>
                ├── <test_name>_bin
                └── params.yaml
```
"""

import argparse
import json
import pathlib
import sys


def _get_sources_list(
    versioned_root: str,
    test_sources_list_path: pathlib.Path,
) -> list[str]:
    """Returns a list of all required sources for IDK bundling.

    Args:
        versioned_root: The path to store versioned Python E2E files relative to
          the SDK root.
        test_sources_list_path: The path to the test sources file.

    Return:
        List of "dest=source" mappings
    """
    files: list[str] = []

    # Process required test sources from file.
    with test_sources_list_path.open() as f:
        test_sources_json = json.load(f)
        for entry in test_sources_json:
            files.append(f"{versioned_root}/{entry['name']}={entry['path']}")

    return files


def main() -> int:
    parser = argparse.ArgumentParser("Builds metadata file and sources file")
    parser.add_argument(
        "--out-metadata",
        type=pathlib.Path,
        help="Path to the output `meta.json` Python test metadata file",
        required=True,
    )
    parser.add_argument(
        "--out-sources",
        type=pathlib.Path,
        help="Path to the output sources list file",
        required=True,
    )
    parser.add_argument(
        "--test-sources-list",
        type=pathlib.Path,
        help="Path to the file containing all test sources required by the test",
        required=True,
    )
    parser.add_argument(
        "--name", help="Name of the Python E2E test", required=True
    )
    parser.add_argument(
        "--root", help="Root of the Python E2E test in the SDK", required=True
    )
    parser.add_argument(
        "--api-level",
        help="The API level the test is versioned at",
        required=False,
        default="unversioned",
    )
    args = parser.parse_args()

    files: list[str] = _get_sources_list(
        versioned_root=f"{args.root}/{args.api_level}",
        test_sources_list_path=args.test_sources_list,
    )

    # Write file list.
    with args.out_sources.open(mode="w") as sources_file:
        for file in files:
            sources_file.write(file + "\n")

    metadata = {
        "type": "experimental_python_e2e_test",
        "name": args.name,
        "root": args.root,
        "files": [f.split("=")[0] for f in files],
    }

    # Write metadata.
    with args.out_metadata.open(mode="w") as metadata_file:
        json.dump(
            metadata,
            metadata_file,
            indent=2,
            sort_keys=True,
        )

    return 0


if __name__ == "__main__":
    sys.exit(main())
