# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

#### CATEGORY=Build
### List which packages are built.

import argparse
import collections
import json
import os
import pathlib
import re
import sys
from typing import Callable, Dict, List

FUCHSIA_BUILD_DIR = os.environ.get("FUCHSIA_BUILD_DIR")


# Print all the packages in sorted order, one per line.
def print_packages(packages: List[str]) -> None:
    for p in sorted(packages):
        print(p)


# Extracts the list of package names that are accepted by filter_ from a
# decoded package list manifest.
def extract_packages_from_listing(
    manifest_paths, filter_: Callable[[str], bool]
) -> [str]:
    packages: list[str] = []
    for manifest in manifest_paths["content"]["manifests"]:
        packages.append(
            json.load(open(f"{FUCHSIA_BUILD_DIR}/{manifest}"))["package"][
                "name"
            ]
        )
    return filter(filter_, packages)


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="""
list-packages lists the packages that the build is aware of. These are
packages that can be rebuilt and/or pushed to a device.
Note: list-packages DOES NOT list all packages that *could* be built, only
those that are included in the current build configuration.
""",
        epilog="""
See https://fuchsia.dev/fuchsia-src/development/build/boards_and_products
for more information about using these package sets.
""",
    )
    parser.add_argument(
        "pattern",
        nargs="?",
        help="list only packages that full match this regular expression",
    )
    return parser.parse_args()


def main() -> None:
    args = parse_args()

    # If a custom regex for package names is provided, use that to filter
    # results; otherwise, return all results
    if args.pattern:
        regex = re.compile(args.pattern)
        filter_ = lambda s: bool(regex.fullmatch(s))
    else:
        filter_ = lambda s: True

    if FUCHSIA_BUILD_DIR is None:
        raise RuntimError(
            'Environment variable "FUCHSIA_BUILD_DIR" is not set.'
        )

    with open(f"{FUCHSIA_BUILD_DIR}/all_package_manifests.list") as f:
        packages = extract_packages_from_listing(json.load(f), filter_)
    print_packages(packages)


if __name__ == "__main__":
    sys.exit(main())
