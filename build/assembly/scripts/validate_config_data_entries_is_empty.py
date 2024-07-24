#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import sys
from dataclasses import dataclass, field
from logging import warn
from typing import Any

import serialization
from serialization import instance_from_dict


@dataclass
class RawConfigDataEntry:
    destination: str = field()  # Destination path in the config_data package
    label: str = field()  # label defining the entry
    source: str = field()  # source file for the entry

    def for_pkg(self) -> str:
        parts = self.destination.split("/")
        return parts[2]


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Parse config_data package entries file and validate it's empty"
    )
    parser.add_argument(
        "--metadata-walk-results",
        type=argparse.FileType("r"),
        required=True,
        help="Path to generated_file() output.",
    )
    parser.add_argument(
        "--package-set",
        required=True,
        help="Name of the package set being validated.",
    )
    parser.add_argument(
        "--package-allowlist",
        type=argparse.FileType("r"),
        help="Path to json list of packages names that are allowed to use config_data",
    )
    parser.add_argument(
        "--output",
        required=True,
        help="Path for the script to write its output to.",
    )
    args = parser.parse_args()

    allowlist: list[str] = []
    if args.package_allowlist:
        allowlist = json.load(args.package_allowlist)

    raw_entries: list[dict[str, Any]] = json.load(args.metadata_walk_results)

    parsed_entries: dict[str, list[RawConfigDataEntry]] = {}
    for entry in raw_entries:
        parsed: RawConfigDataEntry = instance_from_dict(
            RawConfigDataEntry, entry
        )
        pkg = parsed.for_pkg()
        parsed_entries.setdefault(pkg, []).append(parsed)

    fail_entries = {}
    warn_entries = {}

    for pkg, entries in parsed_entries.items():
        if pkg in allowlist:
            warn_entries[pkg] = entries
        else:
            fail_entries[pkg] = entries

    def print_entries(
        banner: str,
        pkg_entries: dict[str, list[RawConfigDataEntry]],
        list_entry_details: bool = True,
    ) -> None:
        print(banner)
        sorted_keys = sorted(pkg_entries.keys())
        for pkg in sorted_keys:
            print(f"  {pkg}")
        if list_entry_details:
            print("all entries:")
            for pkg in sorted_keys:
                print(f"  {pkg}")
                for entry in sorted(
                    pkg_entries[pkg], key=lambda entry: entry.label
                ):
                    print(
                        f"    {entry.label}  {entry.destination} <- {entry.source}"
                    )

    if warn_entries:
        print_entries(
            f"WARNING: Found config_data entries in '{args.package_set} for the following allow-listed packages:",
            warn_entries,
            list_entry_details=False,
        )

    if fail_entries:
        print_entries(
            "The follow non-allowlisted packages have config_data set:",
            fail_entries,
        )
        return -1

    with open(args.output, "w") as output:
        print("validated", file=output)
    return 0


if __name__ == "__main__":
    sys.exit(main())
