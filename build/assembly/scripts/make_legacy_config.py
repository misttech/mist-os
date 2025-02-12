#!/usr/bin/env fuchsia-vendored-python
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Make the legacy configuration set

Create an ImageAssembly configuration based on the GN-generated config, after
removing any other configuration sets from it.
"""

import argparse
import json
import logging
import sys
from collections import defaultdict
from dataclasses import dataclass

import serialization
from assembly import (
    AIBCreator,
    AssemblyInputBundle,
    AssemblyInputBundleCreationException,
    FileEntry,
    FilePath,
    KernelInfo,
    PackageManifest,
)
from assembly.assembly_input_bundle import (
    CompiledComponentDefinition,
    CompiledPackageDefinitionFromGN,
    DuplicatePackageException,
    PackageDetails,
    PackageManifestParsingException,
)
from depfile import DepFile
from serialization import instance_from_dict, json_load

logger = logging.getLogger()

# Some type annotations for clarity
PackageManifestlist = list[FilePath]
Merkle = str
Bloblist = list[tuple[Merkle, FilePath]]
FileEntrylist = list[FileEntry]
FileEntrySet = set[FileEntry]
DepSet = set[FilePath]


@dataclass
@serialization.serialize_dict
class ConfigDataEntryFromGN:
    """The GN metadata written for config-data entries"""

    label: str
    source: FilePath
    destination: FilePath

    def as_file_entry(self) -> FileEntry:
        return FileEntry(self.source, self.destination)


def copy_to_assembly_input_bundle(
    base: list[FilePath],
    cache: list[FilePath],
    bootfs_packages: list[FilePath],
    kernel: KernelInfo,
    boot_args: list[str],
    config_data_entries: FileEntrylist,
    outdir: FilePath,
    shell_commands: dict[str, list[str]],
    core_realm_shards: list[FilePath],
    bootfs_files_package: FilePath | None,
) -> tuple[AssemblyInputBundle, FilePath, DepSet]:
    """
    Copy all the artifacts into an AssemblyInputBundle that is in outdir,
    tracking all copy operations in a DepFile that is returned with the
    resultant bundle.

    Some notes on operation:
        - <outdir> is removed and recreated anew when called.
        - hardlinks are used for performance
        - the return value contains a list of all files read/written by the
        copying operation (ie. depfile contents)
    """
    aib_creator = AIBCreator(outdir)

    aib_creator.packages.update(
        [PackageDetails(m, "bootfs") for m in bootfs_packages]
    )
    aib_creator.kernel = kernel
    aib_creator.boot_args.update(boot_args)

    if bootfs_files_package:
        aib_creator.bootfs_files_package = bootfs_files_package

    # Order is important here, because there can be the same package in multiple
    # of base, base_drivers, cache, and cache_drivers, so we need to remove the
    # duplicates.

    # Remove base_drivers and base from cache
    cache = list(set(cache).difference(base))

    # Now we can update the aib_creator
    aib_creator.packages.update([PackageDetails(m, "base") for m in base])
    aib_creator.packages.update([PackageDetails(m, "cache") for m in cache])

    aib_creator.config_data = config_data_entries
    aib_creator.shell_commands = shell_commands

    if core_realm_shards:
        # Pass the compiled_package_shards
        package = CompiledPackageDefinitionFromGN(
            name="core",
            components=[
                CompiledComponentDefinition("core", set(core_realm_shards))
            ],
        )
        aib_creator.compiled_packages.append(package)

    return aib_creator.build()


def main() -> None:
    parser = argparse.ArgumentParser(
        description="Create an image assembly configuration"
    )

    parser.add_argument("--base-packages-list", type=argparse.FileType("r"))
    parser.add_argument("--cache-packages-list", type=argparse.FileType("r"))
    parser.add_argument(
        "--kernel-cmdline", type=argparse.FileType("r"), required=True
    )
    parser.add_argument("--boot-args", type=argparse.FileType("r"))
    parser.add_argument("--bootfs-packages-list", type=argparse.FileType("r"))

    parser.add_argument("--config-data-entries", type=argparse.FileType("r"))
    parser.add_argument("--outdir", required=True)
    parser.add_argument("--depfile")
    parser.add_argument("--export-manifest")
    parser.add_argument(
        "--shell-commands-packages-list", type=argparse.FileType("r")
    )
    parser.add_argument("--core-realm-shards-list", type=argparse.FileType("r"))
    parser.add_argument("--bootfs-files-package", required=True)
    args = parser.parse_args()

    # Read in the config_data entries if available.
    if args.config_data_entries:
        config_data_entries = [
            instance_from_dict(ConfigDataEntryFromGN, entry)
            for entry in json.load(args.config_data_entries)
        ]
    else:
        config_data_entries = []

    shell_commands: dict[str, set[str]] = dict()
    shell_deps = set()
    if args.shell_commands_packages_list:
        shell_commands = defaultdict(set)
        for package in json.load(args.shell_commands_packages_list):
            manifest_path, package_name = (
                package["manifest_path"],
                package["package_name"],
            )
            with open(manifest_path, "r") as fname:
                package_aib = json_load(PackageManifest, fname)
                shell_deps.add(manifest_path)
                shell_commands[package_name].update(
                    {
                        blob.path
                        for blob in package_aib.blobs
                        if blob.path.startswith("bin/")
                    }
                )

    core_realm_shards: list[FilePath] = []
    if args.core_realm_shards_list:
        for shard in json.load(args.core_realm_shards_list):
            core_realm_shards.append(shard)

    base = []
    if args.base_packages_list is not None:
        base_packages_list = json.load(args.base_packages_list)
        base = base_packages_list

    cache = []
    if args.cache_packages_list is not None:
        cache_packages_list = json.load(args.cache_packages_list)

        # Strip all base pkgs from the cache pkgs set, so there are no
        # duplicates across the two sets.
        if base:
            cache_packages_set = set(cache_packages_list)
            cache_packages_set.difference_update(base)
            cache_packages_list = list(sorted(cache_packages_set))

        cache = cache_packages_list

    kernel = KernelInfo()
    kernel.args.update(json.load(args.kernel_cmdline))

    boot_args = []
    if args.boot_args is not None:
        boot_args = sorted(json.load(args.boot_args))

    bootfs_packages = []
    if args.bootfs_packages_list is not None:
        bootfs_packages_list = json.load(args.bootfs_packages_list)
        bootfs_packages = bootfs_packages_list

    # Only include a bootfs files package if it's not empty.
    bootfs_files_package = None
    if args.bootfs_files_package:
        with open(args.bootfs_files_package, "r") as fname:
            bootfs_files_package_manifest = json_load(PackageManifest, fname)
            if [
                path
                for path in bootfs_files_package_manifest.blobs_by_path().keys()
                if path != "meta/"
            ]:
                # there are bootfs files, so include the package:
                bootfs_files_package = args.bootfs_files_package

    # Create an Assembly Input Bundle from the remaining contents
    (
        assembly_input_bundle,
        assembly_config_manifest_path,
        deps,
    ) = copy_to_assembly_input_bundle(
        base,
        cache,
        bootfs_packages,
        kernel,
        boot_args,
        [entry.as_file_entry() for entry in config_data_entries],
        args.outdir,
        {
            package: sorted(list(components))
            for (package, components) in shell_commands.items()
        },
        core_realm_shards,
        bootfs_files_package,
    )

    deps.update(shell_deps)
    # Write out a fini manifest of the files that have been copied, to create a
    # package or archive that contains all of the files in the bundle.
    if args.export_manifest:
        with open(args.export_manifest, "w") as export_manifest:
            assembly_input_bundle.write_fini_manifest(
                export_manifest, base_dir=args.outdir
            )

    # Write out a depfile.
    if args.depfile:
        with open(args.depfile, "w") as depfile:
            DepFile.from_deps(assembly_config_manifest_path, deps).write_to(
                depfile
            )


if __name__ == "__main__":
    try:
        main()
    except DuplicatePackageException:
        logger.exception(
            "The Legacy Assembly Input Bundle could not be constructed due to \
        a duplicate package declaration in the build"
        )
    except PackageManifestParsingException:
        logger.exception(
            "A problem occurred attempting to load a PackageManifest"
        )
    except AssemblyInputBundleCreationException:
        logger.exception("A problem occured building the legacy bundle")
    finally:
        sys.exit()
