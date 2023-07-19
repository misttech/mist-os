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


# Print all the packages in sorted order, one per line.
def print_packages(packages_by_source: Dict[str, List[str]]) -> None:
    packages = set()
    for ps in packages_by_source.values():
        packages.update(ps)
    for p in sorted(packages):
        print(p)


# Print all the packages in sorted order, along with which package set(s) they
# are included in, one package per line.
def print_packages_verbose(packages_by_source: Dict[str, List[str]]) -> None:
    packages = collections.defaultdict(list)
    for source, ps in packages_by_source.items():
        for p in ps:
            packages[p].append(source)
    for p in sorted(packages.keys()):
        print(p, ' [', ' '.join(sorted(packages[p])), ']', sep='')


# Extracts the list of package names that are accepted by filter_ from a
# decoded package list manifest.
def extract_packages_from_listing(listing, filter_: Callable[[str],
                                                             bool]) -> [str]:
    return list(
        filter(
            filter_,
            (path.split("/")[-1] for path in listing["content"]["manifests"])))


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(
        description="""
list-packages lists the packages that the build is aware of. These are
packages that can be rebuilt and/or pushed to a device.
If no package sets are specified, all package sets will be checked.
Note: list-packages DOES NOT list all packages that *could* be built, only
those that are included in the current build configuration.
""",
        epilog="""
See https://fuchsia.dev/fuchsia-src/development/build/boards_and_products
for more information about using these package sets.
""")
    parser.add_argument(
        "--base", action="store_true", help="list packages in base")
    parser.add_argument(
        "--cache", action="store_true", help="list packages in cache")
    parser.add_argument(
        "--universe", action="store_true", help="list packages in universe")
    parser.add_argument(
        "--verbose",
        action="store_true",
        help="print package set for each listed package")
    parser.add_argument(
        "pattern",
        nargs='?',
        help="list only packages that full match this regular expression")
    return parser.parse_args()


def main() -> None:
    args = parse_args()

    if args.pattern:
        regex = re.compile(args.pattern)
        filter_ = lambda s: bool(regex.fullmatch(s))
    else:
        filter_ = lambda s: True

    packages_by_source = {}

    fuchsia_build_dir = os.environ.get('FUCHSIA_BUILD_DIR')
    if fuchsia_build_dir is None:
        raise RuntimError('Environment variable "FUCHSIA_BUILD_DIR" is not set.')

    def add_listing(listing, filename):
        with open(f"{fuchsia_build_dir}/{filename}") as f:
            packages_by_source[listing] = extract_packages_from_listing(
                json.load(f), filter_)

    none_specified = not any([args.base, args.cache, args.universe])
    if args.base or none_specified:
        add_listing("base", "base_packages.list")
    if args.cache or none_specified:
        add_listing("cache", "cache_packages.list")
    if args.universe or none_specified:
        add_listing("universe", "universe_packages.list")

    if args.verbose:
        print_packages_verbose(packages_by_source)
    else:
        print_packages(packages_by_source)


if __name__ == "__main__":
    sys.exit(main())
