#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import os
import shutil
import sys
from dataclasses import dataclass, field
from typing import Dict, List, Optional

from assembly import PackageCopier, PackageDetails
from assembly.assembly_input_bundle import CompiledPackageDefinition, DepSet
from depfile import DepFile
from serialization import json_dump, json_load

# See //src/lib/assembly/config_schema/src/developer_overrides.rs for documentation.
# These must be kept in sync with that file.


@dataclass
class ShellCommandEntryFromGN:
    package: str
    components: List[str]


@dataclass
class DeveloperOverridesFromGN:
    """This is the schema used to parse the developer overrides that are written by the GN template."""

    target_name: Optional[str]

    # The following are opaque dictionaries to this script, and don't need to be specified in any
    # further detail, because they are written out just as they are read in.
    developer_only_options: dict = field(default_factory=dict)  # type: ignore
    kernel: dict = field(default_factory=dict)  # type: ignore
    platform: dict = field(default_factory=dict)  # type: ignore

    # Packages we need to copy, so we'll need real types for those
    packages: List[PackageDetails] = field(default_factory=list)
    packages_to_compile: List[CompiledPackageDefinition] = field(
        default_factory=list
    )

    # The type that's deserialized from what GN writes is different from that which will be written
    # out for Assembly to use.
    shell_commands: List[ShellCommandEntryFromGN] = field(default_factory=list)


ShellCommandsForAssembly = Dict[str, List[str]]


@dataclass
class DeveloperOverridesForAssembly:
    """This is the schema used to write out the overrides file for Assembly to use."""

    target_name: Optional[str]

    # The following are opaque dictionaries to this script, and don't need to be specified in any
    # further detail, because they are written out just as they are read in.
    developer_only_options: dict = field(default_factory=dict)  # type: ignore
    kernel: dict = field(default_factory=dict)  # type: ignore
    platform: dict = field(default_factory=dict)  # type: ignore

    # Packages we need to copy, so we'll need real types for those
    packages: List[PackageDetails] = field(default_factory=list)
    packages_to_compile: List[CompiledPackageDefinition] = field(
        default_factory=list
    )

    # The type that's written out for Assembly to use is different from that which is read from GN.
    shell_commands: ShellCommandsForAssembly = field(default_factory=dict)


def main() -> int:
    parser = argparse.ArgumentParser(
        description="Tool for creating the file for Assembly developer overrides in-tree"
    )
    parser.add_argument(
        "--input",
        required=True,
        type=argparse.FileType("r"),
        help="Path to a json file containing the intermediate assembly developer overrides",
    )
    parser.add_argument(
        "--outdir",
        required=True,
        help="Path to the output dir that will contain the developer overrides",
    )
    parser.add_argument(
        "--depfile",
        help="Path to an optional depfile to write of all files used to construct the developer overrides",
    )
    args = parser.parse_args()
    deps: DepSet = set()

    # Remove the existing <outdir>, and recreate it and the "subpackages"
    # subdirectory.
    if os.path.exists(args.outdir):
        shutil.rmtree(args.outdir)

    overrides_from_gn = json_load(DeveloperOverridesFromGN, args.input)
    deps.add(args.input.name)

    # Prep the result.
    overrides_for_assembly = DeveloperOverridesForAssembly(
        overrides_from_gn.target_name,
        developer_only_options=overrides_from_gn.developer_only_options,
        kernel=overrides_from_gn.kernel,
        platform=overrides_from_gn.platform,
    )

    overrides_for_assembly.shell_commands = {}
    for entry in overrides_from_gn.shell_commands:
        overrides_for_assembly.shell_commands[entry.package] = [
            f"bin/{name}" for name in entry.components
        ]

    if overrides_from_gn.packages:
        # Set up the package copier to copy all the packages
        package_copier = PackageCopier(args.outdir)

        # For each package details entry from GN, add the package to the set of packages to copy
        # and then create a new package details entry for assembly that uses the new path of the
        # copied package.
        for package_entry in overrides_from_gn.packages:
            destination_path, _ = package_copier.add_package(
                package_entry.package
            )
            overrides_for_assembly.packages.append(
                PackageDetails(destination_path, package_entry.set)
            )

        _, copy_deps = package_copier.perform_copy()
        deps.update(copy_deps)

    outfile_path = os.path.join(args.outdir, "product_assembly_overrides.json")

    if args.depfile:
        with open(args.depfile, "w") as depfile:
            DepFile.from_deps(outfile_path, deps).write_to(depfile)

    with open(outfile_path, "w") as output:
        json_dump(overrides_for_assembly, output, indent=2)

    return 0


if __name__ == "__main__":
    sys.exit(main())
