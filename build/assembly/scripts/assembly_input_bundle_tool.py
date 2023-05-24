#!/usr/bin/env fuchsia-vendored-python
# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import json
import os
import subprocess
import sys
from typing import Dict, List, Set, Tuple
import logging
from assembly.assembly_input_bundle import CompiledPackageAdditionalShards, CompiledPackageMainDefinition
from depfile import DepFile
from assembly import AssemblyInputBundle, AIBCreator, DriverDetails, FilePath, PackageManifest, FileEntry
from serialization.serialization import instance_from_dict, json_dumps, json_load
logger = logging.getLogger()

BOOTFS_COMPILED_PACKAGE_ALLOWLIST = [
    "fshost",
    "qux"  # test package
]


def create_bundle(args: argparse.Namespace) -> None:
    """Create an Assembly Input Bundle (AIB).
    """
    aib_creator = AIBCreator(args.outdir)

    # Add the base and cache packages, if they exist.
    if args.base_pkg_list:
        add_pkg_list_from_file(aib_creator, args.base_pkg_list, "base")

    if args.cache_pkg_list:
        add_pkg_list_from_file(aib_creator, args.cache_pkg_list, "cache")

    if args.bootfs_pkg_list:
        add_pkg_list_from_file(
            aib_creator, args.bootfs_pkg_list, "bootfs_packages")

    if args.shell_cmds_list:
        add_shell_commands_from_file(aib_creator, args.shell_cmds_list)

    if args.compiled_packages:
        add_compiled_packages_from_file(aib_creator, args.compiled_packages)

    if args.drivers_pkg_list:
        add_driver_list_from_file(aib_creator, args.drivers_pkg_list)

    if args.config_data_list:
        for config_data_entry_file in args.config_data_list:
            with open(config_data_entry_file) as config_data_entry:
                add_config_data_entries_from_file(
                    aib_creator, config_data_entry)

    if args.bootfs_files_list:
        for bootfs_files_entry_file in args.bootfs_files_list:
            with open(bootfs_files_entry_file) as bootfs_files_entry:
                add_bootfs_files_from_list(aib_creator, bootfs_files_entry)

    if args.kernel_cmdline:
        add_kernel_cmdline_from_file(aib_creator, args.kernel_cmdline)

    # Add any bootloaders.
    if args.qemu_kernel:
        aib_creator.qemu_kernel = args.qemu_kernel

    # Create the AIB itself.
    (assembly_input_bundle, assembly_config, deps) = aib_creator.build()

    # Write out a dep file if one is requested.
    if args.depfile:
        DepFile.from_deps(assembly_config, deps).write_to(args.depfile)

    # Write out a fini manifest of the files that have been copied, to create a
    # package or archive that contains all of the files in the bundle.
    if args.export_manifest:
        assembly_input_bundle.write_fini_manifest(
            args.export_manifest, base_dir=args.outdir)


def add_pkg_list_from_file(
        aib_creator: AIBCreator, pkg_list_file, pkg_set_name: str):
    pkg_set: Set = getattr(aib_creator, pkg_set_name)
    pkg_list = _read_json_file(pkg_list_file)
    for pkg_manifest_path in pkg_list:
        if pkg_manifest_path in pkg_set:
            raise ValueError(
                f"duplicate pkg manifest found: {pkg_manifest_path}")
        pkg_set.add(pkg_manifest_path)


def add_kernel_cmdline_from_file(aib_creator: AIBCreator, kernel_cmdline_file):
    cmdline_list = _read_json_file(kernel_cmdline_file)
    for cmd in cmdline_list:
        if cmd in aib_creator.kernel.args:
            raise ValueError(f"duplicate kernel cmdline arg found: {cmd}")
        aib_creator.kernel.args.add(cmd)


def add_driver_list_from_file(aib_creator: AIBCreator, driver_list_file):
    pkg_set: Set = getattr(aib_creator, "base")
    driver_details_list = _read_json_file(driver_list_file)
    for driver_details in driver_details_list:
        if driver_details["package_target"] in pkg_set:
            raise ValueError(
                f"duplicate pkg manifest found: {driver_details['package_target']}"
            )
        aib_creator.provided_base_driver_details.append(
            DriverDetails(
                driver_details["package_target"],
                driver_details["driver_components"]))


def add_shell_commands_from_file(
        aib_creator: AIBCreator, shell_commands_list_file):
    """
    [
        {
            "components": [
                "ls"
            ],
            "package": "ls"
        }
    ]
    """
    loaded_file = _read_json_file(shell_commands_list_file)

    for command in loaded_file:
        package = command["package"]
        components = command["components"]
        aib_creator.shell_commands[package].extend(
            ["bin/" + component for component in components])


def add_config_data_entries_from_file(
        aib_creator: AIBCreator, config_data_entries):
    """
    config_data_entries schema:
    [
        {
            'package_name': 'example_package',
            'destination': 'foo.txt',
            'source': 'src/sys/example/configs/example.json'
        }
    ]
    """
    _config_data = _read_json_file(config_data_entries)
    for definition in _config_data:
        entry = FileEntry(
            definition['source'],
            f"meta/data/{definition['package_name']}/{definition['destination']}"
        )
        aib_creator.config_data.append(entry)


def add_compiled_packages_from_file(aib_creator: AIBCreator, compiled_packages):
    """
    compiled_packages should be
    List[CompiledPackageMainDefinition|CompiledPackageAdditionalShards]
    """

    _compiled_packages = _read_json_file(compiled_packages)
    for package_def in _compiled_packages:
        # Differentiate entries by field names
        if "component_shards" in package_def:
            # Transform from the GN format to the internal format
            shards = {}
            for definition in package_def['component_shards']:
                if definition['component_name'] in shards:
                    raise ValueError(
                        f"Unexpected repeated component name: {definition['component_name']}"
                    )

                shards[definition['component_name']] = definition['shards']
            package_def['component_shards'] = shards

            aib_creator.compiled_package_shards.append(
                instance_from_dict(
                    CompiledPackageAdditionalShards, package_def))
        else:
            shards = {}
            for definition in package_def['components']:
                if definition['component_name'] in shards:
                    raise ValueError(
                        f"Unexpected repeated component name: {definition['component_name']}"
                    )

                shards[definition['component_name']] = definition['shards']
            package_def['components'] = shards

            main_def = instance_from_dict(
                CompiledPackageMainDefinition, package_def)

            # Type unsafe; see assembly_input_bundle.py
            if (package_def.get("component_includes")):
                main_def.includes = [
                    instance_from_dict(FileEntry, x)
                    for x in package_def["component_includes"]
                ]

            if main_def.bootfs_unpackaged and main_def.name not in BOOTFS_COMPILED_PACKAGE_ALLOWLIST:
                raise ValueError(
                    f"Compiled package {main_def.name} not in bootfs allowlist!"
                )
            aib_creator.compiled_packages.append(main_def)


def add_bootfs_files_from_list(aib_creator: AIBCreator, bootfs_files):
    """
    bootfs_files schema:
    [
        {
            'destination': 'bin/bar',
            'source': 'src/sys/example/configs/example.json'
        }
    ]
    """
    _bootfs_files = _read_json_file(bootfs_files)
    for entry in _bootfs_files:
        # Not all distribution manifests have the source and destination pairs.
        # For an example see: dart_kernel.gni
        if 'source' in entry and 'destination' in entry:
            aib_creator.bootfs_files.add(
                FileEntry(entry['source'], entry['destination']))


def _read_json_file(pkg_list_file):
    try:
        return json.load(pkg_list_file)
    except:
        logger.exception(f"While parsing {pkg_list_file.name}")
        raise


def generate_package_creation_manifest(args: argparse.Namespace) -> None:
    """Generate a package creation manifest for an Assembly Input Bundle (AIB)

    Each AIB has a contents manifest that was created with it.  This file lists
    all of the files in the AIB, and their path within the build dir::

      AIB/path/to/file_1=outdir/path/to/AIB/path/to/file_1
      AIB/path/to/file_2=outdir/path/to/AIB/path/to/file_2

    This format locates all the files in the AIB, relative to the
    root_build_dir, and then gives their destination path within the AIB package
    and archive.

    To generate the package the AIB, a creation manifest is required (also in
    FINI format).  This is the same file, with the addition of the path to the
    package metadata file::

      meta/package=path/to/metadata/file

    This fn generates the package metadata file, and then generates the creation
    manifest file by appending the path to the metadata file to the entries in
    the AIB contents manifest.
    """
    meta_package_content = {'name': args.name, 'version': '0'}
    json.dump(meta_package_content, args.meta_package)
    contents_manifest = args.contents_manifest.read()
    args.output.write(contents_manifest)
    args.output.write("meta/package={}".format(args.meta_package.name))


def generate_archive(args: argparse.Namespace) -> None:
    """Generate an archive of an Assembly Input Bundle (AIB)

    Each AIB has a contents manifest that was created with it.  This file lists
    all of the files in the AIB, and their path within the build dir::

      AIB/path/to/file_1=outdir/path/to/AIB/path/to/file_1
      AIB/path/to/file_2=outdir/path/to/AIB/path/to/file_2

    This format locates all the files in the AIB, relative to the
    root_build_dir, and then gives their destination path within the AIB package
    and archive.

    To generate the archive of the AIB, a creation manifest is required (also in
    FINI format).  This is the same file, with the addition of the path to the
    package meta.far.

      meta.far=path/to/meta.far

    This fn generates the creation manifest, appending the package meta.far to
    the contents manifest, and then calling the tarmaker tool to build the
    archive itself, using the generated creation manifest.
    """
    deps: Set[str] = set()
    # Read the AIB's contents manifest, all of these files will be added to the
    # creation manifest for the archive.
    contents_manifest = args.contents_manifest.readlines()
    deps.add(args.contents_manifest.name)
    with open(args.creation_manifest, 'w') as creation_manifest:

        if args.meta_far:
            # Add the AIB's package meta.far to the creation manifest if one was
            # provided.
            creation_manifest.write("meta.far={}\n".format(args.meta_far))

        # Add all files from the AIB's contents manifest.
        for line in contents_manifest:
            # Split out the lines so that a depfile for the action can be made
            # from the contents_manifest's source paths.
            src = line.split('=', 1)[1]
            deps.add(src.strip())
            creation_manifest.write(line)

    # Build the archive itself.
    cmd_args = [
        args.tarmaker, "-manifest", args.creation_manifest, "-output",
        args.output
    ]
    subprocess.run(cmd_args, check=True)

    if args.depfile:
        DepFile.from_deps(args.output, deps).write_to(args.depfile)


def diff_bundles(args: argparse.Namespace) -> None:
    first = AssemblyInputBundle.json_load(args.first)
    second = AssemblyInputBundle.json_load(args.second)
    result = first.difference(second)
    if args.output:
        result.json_dump(args.output)
    else:
        print(result)


def intersect_bundles(args: argparse.Namespace) -> None:
    bundles = [AssemblyInputBundle.json_load(file) for file in args.bundles]
    result = bundles[0]
    for next_bundle in bundles[1:]:
        result = result.intersection(next_bundle)
    if args.output:
        result.json_dump(args.output)
    else:
        print(result)


def find_blob(args: argparse.Namespace) -> None:
    bundle = AssemblyInputBundle.json_load(args.bundle_config)
    bundle_dir = os.path.dirname(args.bundle_config.name)
    manifests: List[FilePath] = [*bundle.base, *bundle.cache, *bundle.system]
    found_at = find_blob_in_manifests(args.blob, bundle_dir, manifests)
    if found_at:
        pkg_header = "Package"
        path_header = "Path"
        pkg_column_width = max(
            len(pkg_header), *[len(entry[0]) for entry in found_at])
        path_column_width = max(
            len(path_header), *[len(entry[1]) for entry in found_at])
        formatter = f"{{0: <{pkg_column_width}}}  | {{1: <{path_column_width}}}"
        header = formatter.format(pkg_header, path_header)
        print(header)
        print("=" * len(header))
        for pkg, path in found_at:
            print(formatter.format(pkg, path))


def find_blob_in_manifests(
        blob_to_find: str, bundle_dir: str,
        manifests_to_search: List[FilePath]) -> List[Tuple[FilePath, FilePath]]:
    found_at: List[Tuple[FilePath, FilePath]] = []
    known_manifests = set(manifests_to_search)

    i = 0
    while i < len(manifests_to_search):
        pkg_manifest_path = manifests_to_search[i]
        i += 1
        with open(os.path.join(bundle_dir, pkg_manifest_path),
                  'r') as pkg_manifest_file:
            manifest = json_load(PackageManifest, pkg_manifest_file)
            if not manifest.blob_sources_relative:
                raise ValueError(
                    f"Unexpected non-relative paths in AIB package manifest: {pkg_manifest_path}"
                )
            for blob in manifest.blobs:
                if blob.merkle == blob_to_find:
                    found_at.append((pkg_manifest_path, blob.path))
            for subpackage in manifest.subpackages:
                subpackage_manifest_path = os.path.join(
                    os.path.dirname(pkg_manifest_path),
                    subpackage.manifest_path)
                # remove `<dir>/../` sequences if present
                subpackage_manifest_path = os.path.relpath(
                    subpackage_manifest_path)
                if subpackage_manifest_path not in known_manifests:
                    manifests_to_search.append(subpackage_manifest_path)
                    known_manifests.add(subpackage_manifest_path)

    return found_at


def main():
    parser = argparse.ArgumentParser(
        description=
        "Tool for creating Assembly Input Bundles in-tree, for use with out-of-tree assembly"
    )
    sub_parsers = parser.add_subparsers(
        title="Commands",
        description="Commands for working with Assembly Input Bundles")

    ###
    #
    # 'assembly_input_bundle_tool create' subcommand parser
    #
    bundle_creation_parser = sub_parsers.add_parser(
        "create", help="Create an Assembly Input Bundle")
    bundle_creation_parser.add_argument(
        "--outdir",
        required=True,
        help="Path to the outdir that will contain the AIB")
    bundle_creation_parser.add_argument(
        "--base-pkg-list",
        type=argparse.FileType('r'),
        help=
        "Path to a json list of package manifests for the 'base' package set")
    bundle_creation_parser.add_argument(
        "--bootfs-pkg-list",
        type=argparse.FileType('r'),
        help=
        "Path to a json list of package manifests for the 'bootfs' package set")
    bundle_creation_parser.add_argument(
        "--drivers-pkg-list",
        type=argparse.FileType('r'),
        help="Path to a json list of driver details for the 'base' package set")
    bundle_creation_parser.add_argument(
        "--shell-cmds-list",
        type=argparse.FileType('r'),
        help=
        "Path to a json list of dictionaries with the manifest path as key and a list of shell_command components as the value"
    )
    bundle_creation_parser.add_argument(
        "--cache-pkg-list",
        type=argparse.FileType('r'),
        help=
        "Path to a json list of package manifests for the 'cache' package set")
    bundle_creation_parser.add_argument(
        "--kernel-cmdline",
        type=argparse.FileType('r'),
        help="Path to a json list of kernel cmdline arguments")
    bundle_creation_parser.add_argument(
        "--qemu-kernel", help="Path to the qemu kernel")
    bundle_creation_parser.add_argument(
        "--depfile",
        type=argparse.FileType('w'),
        help="Path to write a dependency file to")
    bundle_creation_parser.add_argument(
        "--export-manifest",
        type=argparse.FileType('w'),
        help="Path to write a FINI manifest of the contents of the AIB")
    bundle_creation_parser.add_argument(
        "--config-data-list",
        action="append",
        help=
        "Path to a json file of config-data entries, may be specified multiple times"
    )
    bundle_creation_parser.add_argument(
        "--bootfs-files-list",
        action="append",
        help=
        "Path to a json file of bootfs-file entries, may be specified multiple times"
    )
    bundle_creation_parser.add_argument(
        "--compiled-packages",
        type=argparse.FileType('r'),
        help="Path to a json file of compiled package configuration")

    bundle_creation_parser.set_defaults(handler=create_bundle)

    ###
    #
    # 'assembly_input_bundle_tool diff' subcommand parser
    #
    diff_bundles_parser = sub_parsers.add_parser(
        "diff",
        help=
        "Calculate the difference between the first and second bundles (A-B).")
    diff_bundles_parser.add_argument(
        "first", help='The first bundle (A)', type=argparse.FileType('r'))
    diff_bundles_parser.add_argument(
        "second", help='The second bundle (B)', type=argparse.FileType('r'))
    diff_bundles_parser.add_argument(
        "--output",
        help='A file to write the output to, instead of stdout.',
        type=argparse.FileType('w'))
    diff_bundles_parser.set_defaults(handler=diff_bundles)

    ###
    #
    # 'assembly_input_bundle_tool intersect' subcommand parser
    #
    intersect_bundles_parser = sub_parsers.add_parser(
        "intersect", help="Calculate the intersection of the provided bundles.")
    intersect_bundles_parser.add_argument(
        "bundles",
        nargs="+",
        action="extend",
        help='Paths to the bundle configs.',
        type=argparse.FileType('r'))
    intersect_bundles_parser.add_argument(
        "--output",
        help='A file to write the output to, instead of stdout.',
        type=argparse.FileType('w'))
    intersect_bundles_parser.set_defaults(handler=intersect_bundles)

    ###
    #
    # 'assembly_input_bundle_tool generate-package-creation-manifest' subcommand
    #  parser
    #
    package_creation_manifest_parser = sub_parsers.add_parser(
        "generate-package-creation-manifest",
        help=
        "(build tool) Generate the creation manifest for the package that contains an Assembly Input Bundle."
    )
    package_creation_manifest_parser.add_argument(
        "--contents-manifest", type=argparse.FileType('r'), required=True)
    package_creation_manifest_parser.add_argument("--name", required=True)
    package_creation_manifest_parser.add_argument(
        "--meta-package", type=argparse.FileType('w'), required=True)
    package_creation_manifest_parser.add_argument(
        "--output", type=argparse.FileType('w'), required=True)
    package_creation_manifest_parser.set_defaults(
        handler=generate_package_creation_manifest)

    ###
    #
    # 'assembly_input_bundle_tool generate-archive' subcommand parser
    #
    archive_creation_parser = sub_parsers.add_parser(
        "generate-archive",
        help=
        "(build tool) Generate the tarmaker creation manifest for the tgz that contains an Assembly Input Bundle."
    )
    archive_creation_parser.add_argument("--tarmaker", required=True)
    archive_creation_parser.add_argument(
        "--contents-manifest", type=argparse.FileType('r'), required=True)
    archive_creation_parser.add_argument("--meta-far")
    archive_creation_parser.add_argument("--creation-manifest", required=True)
    archive_creation_parser.add_argument("--output", required=True)
    archive_creation_parser.add_argument(
        "--depfile", type=argparse.FileType('w'))
    archive_creation_parser.set_defaults(handler=generate_archive)

    ###
    #
    # 'assembly_input_bundle_tool find-blob' subcommand parser
    #
    find_blob_parser = sub_parsers.add_parser(
        "find-blob",
        help=
        "Find what causes a blob to be included in the Assembly Input Bundle.")
    find_blob_parser.add_argument(
        "--bundle-config",
        required=True,
        type=argparse.FileType('r'),
        help="Path to the assembly_config.json for the bundle")
    find_blob_parser.add_argument(
        "--blob", required=True, help="Merkle of the blob to search for.")
    find_blob_parser.set_defaults(handler=find_blob)

    args = parser.parse_args()

    if "handler" in args:
        # Dispatch to the handler fn.
        args.handler(args)
    else:
        # argparse doesn't seem to automatically catch that not subparser was
        # called, and so if there isn't a handler function (which is set by
        # having specified a subcommand), then just display usage instead of
        # a cryptic KeyError.
        parser.print_help()


if __name__ == "__main__":
    sys.exit(main())
