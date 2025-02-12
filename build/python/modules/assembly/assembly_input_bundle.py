# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Python types for Assembly Input Bundles.

Assembly Input Bundles are a set of artifacts that need to be delivered to out-
of-tree (OOT) assembly as a unit, but whose contents should be opaque to the
delivery system itself.

"""
import functools
import os
import pathlib
import shutil
from collections import defaultdict
from dataclasses import dataclass, field
from typing import Any, Dict, List, Optional, Set, TextIO, Tuple, Union

import serialization
from assembly import package_copier
from assembly.package_manifest import PackageManifest
from serialization import json_load, serialize_json

from .common import FileEntry, FilePath, fast_copy_makedirs
from .image_assembly_config import KernelInfo
from .package_copier import PackageCopier

__all__ = [
    "AIBCreator",
    "AssemblyInputBundle",
    "AssemblyInputBundleCreationException",
    "ConfigDataEntries",
    "DriverDetails",
    "PackageDetails",
]

PackageManifestList = List[FilePath]
PackageName = str
ComponentName = str
DepSet = Set[FilePath]
FileEntryList = List[FileEntry]
FileEntrySet = Set[FileEntry]
ComponentShards = Set[FilePath]
Merkle = str
BlobList = List[Tuple[Merkle, FilePath]]
SubpackageManifests = Dict[Merkle, FilePath]
ConfigDataEntries = Dict[PackageName, Set[FileEntry]]


class AssemblyInputBundleCreationException(Exception):
    """To be raised when AIB creation fails for some reason"""

    ...


class DuplicatePackageException(AssemblyInputBundleCreationException):
    """To be raised when an attempt is made to add multiple packages with the same name to the same
    invocation of the AIBCreator"""

    ...


class PackageManifestParsingException(Exception):
    """To be raised when an attempt to parse a json file into a PackageManifest object fails"""

    ...


@dataclass(order=True)
@serialize_json
class DriverDetails:
    """Details for constructing a driver manifest fragment from a driver package"""

    package: FilePath = field()  # Path to the package manifest
    components: Set[FilePath] = field(default_factory=set)


@dataclass
@serialization.serialize_json
class PackageDetails:
    """Details for a package"""

    package: FilePath = field()  # Path to the package manifest
    set: str = field()  # Package set that includes the package

    def __hash__(self):
        """
        This intentionally only hashes the package manifest in order to
        deduplicate packages across package sets.
        """
        return hash(self.package)

    def __lt__(self, other):
        return self.package < other.package


PackageDetailsList = List[PackageDetails]


@dataclass
@serialize_json
class CompiledComponentDefinition:
    """
    The definition of component to be compiled by Assembly
    """

    # Name of the component
    component_name: str = field()
    # Component shards to compile together
    shards: Set[FilePath] = field(default_factory=set)


@dataclass
@serialize_json
class CompiledPackageDefinition:
    """Primary definition of a compiled package which is created by Assembly"""

    # Name of the package
    name: str = field()
    # Dictionary mapping components to cml files by name
    components: List[CompiledComponentDefinition] = field(default_factory=list)
    # Other files to include in the compiled package
    contents: Set[FileEntry] = field(default_factory=set)
    # CML files included by the component cml
    includes: Set[FilePath] = field(default_factory=set)
    # Whether to extract the contents of this package into bootfs
    bootfs_package: bool = field(default=False)


@dataclass
@serialize_json
class CompiledPackageDefinitionFromGN:
    """The CompilePackageDefinition which is written by GN for consuming by this tool.

    The key difference is that the 'includes' field has a different name and has
    a different type of objects (FileEntry vs source paths).  This is so that
    they can be copied to their proper path within the include dir.
    """

    # Name of the package
    name: str = field()
    # Package manifests that include files to add to `contents`
    packages: List[FilePath] = field(default_factory=list)
    # Dictionary mapping components to cml files by name
    components: List[CompiledComponentDefinition] = field(default_factory=list)
    # Other files to include in the compiled package
    contents: Set[FileEntry] = field(default_factory=set)
    # CML files included by the component cml
    component_includes: Set[FileEntry] = field(default_factory=set)
    # Whether to extract the contents of this package into bootfs
    bootfs_package: bool = field(default=False)


@dataclass
@serialize_json
class AssemblyInputBundle:
    """AssemblyInputBundle wraps a set of artifacts together for use by out-of-tree assembly, both
    the manifest of the artifacts, and the artifacts themselves.

    The archived artifacts are placed into a nominal layout that is for readability, but the
    JSON manifest itself is the arbiter of what artifacts are in what categories:

    file layout::

        ./
        assembly_config.json
        packages/
            name_of_aib.bootfs_files_package
            <package name>
        blobs/
            <merkle>
        subpackages/
            <merkle>
        bootfs/
            path/to/file/in/bootfs
        config_data/
            <package name>/
                <file name>
        compiled_packages/
            include/
                path/to/shard/file/in/tree
            <compiled package name>/
                <component_name>/
                    name.shard.cml
                files/
                    path/to/file/in/package
        memory_buckets/
            path/to/bucket.json
        kernel/
            kernel.zbi
            multiboot.bin

    Files matching the patterns `packages/*/<package name>` and
    `subpackages/<merkle>` are JSON representations of the
    `PackageManifest` type (see `package_manifest.py`). (The `<merkle>` in
    `subpackages/` is the merkle of the subpackage's metafar.) A named package
    that is also referenced as a subpackage will have a copy of its manifest in
    both directories. (This is an intentional duplication to allow the
    simplification of the creation and use of the AIBs.)

    Each `PackageManifest` contains a `blobs` list of `BlobEntry` and an
    optional `subpackages` list of `SubpackageEntry`. The `source_path` of a
    `BlobEntry` references a file containing the blob bytes, in the `blobs/`
    directory. The `manifest_path` of a `SubpackageEntry` references another
    `PackageManifest` file in `subpackages/<merkle>`.

    assembly_config.json schema::

        {
            "packages": [
                {
                    "package": "package1",
                    "set": "base",
                },
                {
                    "package": "package2",
                    "set": "base",
                },
                {
                    "package": "package3",
                    "set": "cache",
                },
                {
                    "package": "packageS1",
                    "set": "system",
                },
                {
                    "package": "packageB1",
                    "set": "bootfs",
                },
            ],
            "base_drivers": [ "packageD1", ... ],
            "boot_drivers": [ "packageD1", ... ],
            "bootfs_files_package": "packages/packageB2",
            "bootfs_files": [
                { "destination": "path/in/bootfs", source: "path/in/layout" },
                ...
            ],
            "config_data": {
                "package1": [
                    { "destination": "path/in/data/dir", "source": "path/in/layout" },
                    { "destination": "path/in/data/dir", "source": "path/in/layout" },
                    ...
                ],
                ...
            },
            "blobs": [ "blobs/<hash1>", "blobs/<hash2>", ... ],
            "kernel": {
                "path": "kernel/kernel.zbi",
                "args": [ "arg1", "arg2", ... ],
            },
            "qemu_kernel": "kernel/multiboot.bin",
            "boot_args": [ "arg1", "arg2", ... ],
            "bootfs_shell_commands": {
                "package1":
                    ["path/to/binary1", "path/to/binary2"]
            },
            "shell_commands": {
                "package1":
                    ["path/to/binary1", "path/to/binary2"]
            },
            "packages_to_compile": [
               {
                   "name": "package_name",
                   "components": {
                       "component1": "path/to/component1.cml",
                       "component2": "path/to/component2.cml",
                   },
                   "contents": [
                       {
                           "source": "path/to/source",
                           "destination": "path/to/destination",
                       }
                   ]
               },
               {
                  "name": "package_name",
                  "component_shards": {
                       "component1": [
                           "path/to/shard1.cml",
                           "path/to/shard2.cml"
                       ]
                  }
               }
           ],
           "memory_buckets": [
               "memory_buckets/path/to/bucket.json",
           ],
        }

    All items are optional.  Files for `config_data` should be in the config_data section,
    not in a package called `config_data`.
    """

    # Fields shared with ImageAssemblyConfig.
    kernel: KernelInfo = field(default_factory=KernelInfo)
    qemu_kernel: Optional[FilePath] = None
    boot_args: Set[str] = field(default_factory=set)
    bootfs_files: Set[FileEntry] = field(default_factory=set)
    bootfs_packages: Set[FilePath] = field(default_factory=set)

    packages: Set[PackageDetails] = field(default_factory=set)
    config_data: ConfigDataEntries = field(default_factory=dict)
    blobs: Set[FilePath] = field(default_factory=set)
    base_drivers: List[DriverDetails] = field(default_factory=list)
    boot_drivers: List[DriverDetails] = field(default_factory=list)
    bootfs_shell_commands: Dict[str, List[str]] = field(
        default_factory=functools.partial(defaultdict, list)
    )
    shell_commands: Dict[str, List[str]] = field(
        default_factory=functools.partial(defaultdict, list)
    )
    packages_to_compile: List[CompiledPackageDefinition] = field(
        default_factory=list
    )
    bootfs_files_package: Optional[FilePath] = None
    memory_buckets: Set[FilePath] = field(default_factory=set)

    def __repr__(self) -> str:
        """Serialize to a JSON string"""
        return serialization.json_dumps(self, indent=2)

    def add_packages(self, packages: List[PackageDetails]):
        for details in sorted(packages):
            # This 'in' check only looks at the package manifest file path and
            # ignores the package set. This is intentional in order to
            # deduplicate packages across sets.
            if details in self.packages:
                raise ValueError(f"Duplicate package {details.package}")
            self.packages.add(details)

    def all_file_paths(self) -> List[FilePath]:
        """Return a list of all files that are referenced by this AssemblyInputBundle."""
        file_paths = []
        file_paths.extend([p.package for p in self.packages])
        if self.kernel.path is not None:
            file_paths.append(self.kernel.path)
        if self.qemu_kernel is not None:
            file_paths.append(self.qemu_kernel)
        for entries in self.config_data.values():
            file_paths.extend([entry.source for entry in entries])
        if self.blobs is not None:
            file_paths.extend(self.blobs)
        if self.memory_buckets:
            file_paths.extend(self.memory_buckets)

        for package in self.packages_to_compile:
            file_paths.extend(package.includes)
            file_paths.extend([entry.source for entry in package.contents])
            for component in package.components:
                file_paths.extend(component.shards)

        return file_paths

    def write_fini_manifest(
        self,
        file: TextIO,
        base_dir: Optional[FilePath] = None,
        rebase: Optional[FilePath] = None,
    ) -> None:
        """Write a fini-style manifest of all files in the AssemblyInputBundle
        to the given |file|.

        fini manifests are in the format::

          destination/path=path/to/source/file

        As all file paths in the AssemblyInputBundle are relative to the root of
        the bundle, `destination/path` is the existing path.  However, the path
        to the source file cannot be known (absolutely) without additional
        information.

        Arguments:
        - file -- The |TextIO| file to write to.
        - base_dir -- The folder to assume that file paths are relative from.
        - rebase -- The folder to make the source paths relative to, if `base_dir` is also provided.
          By default this is the cwd.

        If `base_dir` is given, it's used to construct the path to the source files, if not, the cwd
        is assumed to be the path the files are from.

        If `rebase` is also given, the path to the source files are then made relative to it.
        """
        file_paths = self.all_file_paths()
        if base_dir is not None:
            file_path_entries = [
                FileEntry(
                    os.path.relpath(os.path.join(base_dir, path), rebase), path
                )
                for path in file_paths
            ]
            file_path_entries += [
                FileEntry(
                    os.path.join(base_dir, "assembly_config.json"),
                    "assembly_config.json",
                )
            ]
        else:
            file_path_entries = [FileEntry(path, path) for path in file_paths]

        FileEntry.write_fini_manifest(file_path_entries, file)


class AIBCreator:
    """The AIBCreator is a builder for AIBs that will copy all the files into
    the correct layout for the AIB structure, rewriting package manifests
    as needed to make them relative to the AIB manifest location.

    The AIBCreator has fields that match the AIB itself, but isn't an AIB
    because the paths it contains are not valid for an AIB.
    """

    package_url_template = "{repository}/{package_name}"

    def __init__(self, outdir: FilePath) -> None:
        # The directory to create the AIB in.  The manifest will be placed in
        # the root of this dir.
        self.outdir = outdir

        # The packages (paths to package manifests)
        self.packages: Set[PackageDetails] = set()

        # The shell command configurations
        self.shell_commands: Dict[str, List[str]] = defaultdict(list)
        self.bootfs_shell_commands: Dict[str, List[str]] = defaultdict(list)

        # The kernel info
        self.kernel = KernelInfo()
        self.boot_args: Set[str] = set()

        # The emulator kernel.
        self.qemu_kernel: Optional[FilePath] = None

        # Bootfs info
        self.bootfs_files: Set[FileEntry] = set()
        self.bootfs_files_package: Optional[FilePath] = None
        self.bootfs_packages: Set[FilePath] = set()

        # The config_data entries
        self.config_data: FileEntryList = []

        # Additional base drivers directly specified without requiring
        # us to parse GN generated files
        self.provided_base_driver_details: List[DriverDetails] = list()

        # Additional boot drivers directly specified without requiring
        # us to parse GN generated files
        self.provided_boot_driver_details: List[DriverDetails] = list()

        # A set containing all the unique packageUrls seen by the AIBCreator instance
        self.package_urls: Set[str] = set()

        # A list of CompiledPackageDefinitions from either a parsed json GN
        # scope, or directly set by the legacy AIB creator.
        self.compiled_packages: List[CompiledPackageDefinitionFromGN] = list()

        # Memory buckets to add to memory monitor.
        self.memory_buckets: Set[FilePath] = set()

        # The package copying mechanism.
        self.package_copier: PackageCopier = PackageCopier(outdir)

    def build(self) -> Tuple[AssemblyInputBundle, FilePath, DepSet]:
        """
        Copy all the artifacts from the ImageAssemblyConfig into an AssemblyInputBundle that is in
        outdir, tracking all copy operations in a DepFile that is returned with the resultant bundle.

        Some notes on operation:
            - <outdir> is removed and recreated anew when called.
            - hardlinks are used for performance
            - the return value contains a list of all files read/written by the
            copying operation (ie. depfile contents)
        """

        # Remove the existing <outdir>, and recreate it and the "subpackages"
        # subdirectory.
        if os.path.exists(self.outdir):
            shutil.rmtree(self.outdir)

        # Track all files we read
        deps: DepSet = set()

        # Init an empty resultant AssemblyInputBundle
        result = AssemblyInputBundle()

        # Copy over the boot args and zbi kernel args, unchanged, into the resultant
        # assembly bundle
        result.boot_args = self.boot_args
        kernel_args = self.kernel.args
        if kernel_args:
            result.kernel.args = kernel_args

        # Copy the manifests for the packages into the assembly bundle.
        result.add_packages(
            self._prepare_packages_for_copying(sorted(self.packages))
        )

        # Copy the base drivers' packages.
        result.base_drivers.extend(
            sorted(
                self._prepare_drivers_for_copying(
                    self.provided_base_driver_details, "base"
                )
            )
        )

        # Copy bootfs drivers' packages.
        result.boot_drivers.extend(
            sorted(
                self._prepare_drivers_for_copying(
                    self.provided_boot_driver_details, "bootfs"
                )
            )
        )

        # Copy the package that contains the bootfs files, but only if it's not empty.
        if self.bootfs_files_package:
            deps.add(self.bootfs_files_package)
            with open(self.bootfs_files_package, "r") as fname:
                manifest = json_load(PackageManifest, fname)
                if [
                    path
                    for path in manifest.blobs_by_path().keys()
                    if path != "meta/"
                ]:
                    # there are bootfs files, so include the package:
                    (
                        bootfs_pkg_manifest_path,
                        _,
                    ) = self.package_copier.add_package(
                        self.bootfs_files_package
                    )
                    result.bootfs_files_package = bootfs_pkg_manifest_path

        # Copy the memory bucket entries
        _memory_buckets_entries = [
            FileEntry(src, os.path.basename(src)) for src in self.memory_buckets
        ]
        (memory_buckets, memory_buckets_deps) = self._copy_file_entries(
            _memory_buckets_entries, "memory_buckets"
        )
        memory_buckets = [entry.source for entry in memory_buckets]
        deps.update(memory_buckets_deps)
        result.memory_buckets.update(memory_buckets)

        # Add shell_commands field to assembly_config.json field in AIBCreator
        result.bootfs_shell_commands = self.bootfs_shell_commands
        result.shell_commands = self.shell_commands

        # Copy all the blobs to their dir in the out-of-tree layout
        (all_blobs, pkg_copy_deps) = self.package_copier.perform_copy()
        deps.update(pkg_copy_deps)
        result.blobs = set(all_blobs)

        # Copy the bootfs entries
        (bootfs, bootfs_deps) = self._copy_file_entries(
            self.bootfs_files, "bootfs"
        )
        deps.update(bootfs_deps)
        result.bootfs_files.update(bootfs)

        # Rebase the path to the kernel into the out-of-tree layout
        if self.kernel.path:
            kernel_src_path: Any = self.kernel.path
            kernel_filename = os.path.basename(kernel_src_path)
            kernel_dst_path = os.path.join("kernel", kernel_filename)
            result.kernel.path = kernel_dst_path

            # Copy the kernel itself into the out-of-tree layout
            local_kernel_dst_path = os.path.join(self.outdir, kernel_dst_path)
            deps.add(fast_copy_makedirs(kernel_src_path, local_kernel_dst_path))

        # Rebase the path to the qemu kernel into the out-of-tree layout
        if self.qemu_kernel:
            kernel_src_path: Any = self.qemu_kernel
            kernel_filename = os.path.basename(kernel_src_path)
            kernel_dst_path = os.path.join("kernel", kernel_filename)
            result.qemu_kernel = kernel_dst_path

            # Copy the kernel itself into the out-of-tree layout
            local_kernel_dst_path = os.path.join(self.outdir, kernel_dst_path)
            deps.add(fast_copy_makedirs(kernel_src_path, local_kernel_dst_path))

        # Track all the FileEntries for includes, to make sure that we don't get
        # any duplicate destination paths with different source paths.
        all_copied_include_entries: Set[FileEntry] = set()
        for package in self.compiled_packages:
            components: List[CompiledComponentDefinition] = []
            for component_def in package.components:
                copied_shards, component_deps = self._copy_component_shards(
                    component_def.shards,
                    package_name=package.name,
                    component_name=component_def.component_name,
                )
                components.append(
                    CompiledComponentDefinition(
                        component_name=component_def.component_name,
                        shards=set(copied_shards),
                    )
                )
                deps.update(component_deps)

            # This assumes that package.includes has actually been passed to the
            # AIB creator as Set[FileEntry] instead of a Set[FilePath].  This is
            # not ideal, but it allows the reuse of the CompiledPackageDefinition
            # type without any other changes.
            #
            # The FileEntries are only needed because of the SDK include paths
            # used by some component shards.
            #
            # TODO(): Remove the use of the 'include' statement in component shards
            # compiled by assembly, for all included cml files that aren't in the
            # SDK itself (these can be found via a separate path to the SDK's set
            # of cml include files).  These files should either be incorporated
            # into the body of the component shards in the AIB, or added to the
            # AIBs as another shard for the same component.
            #
            # Once that's complete, this whole mechanism can be removed.
            #
            # _copy_component_includes will check for and validate inconsistent
            # include entries.
            (
                copied_include_entries,
                component_includes_deps,
            ) = self._copy_component_includes(
                package.component_includes, all_copied_include_entries
            )
            deps.update(component_includes_deps)

            # Save the copied entries so we can check for inconsistent
            # duplicate include entries in other packages in this AIB
            # in the following loop iterations
            all_copied_include_entries.update(copied_include_entries)

            # The final copied includes to add to the AIB.
            copied_includes = set(
                map(lambda x: x.destination, copied_include_entries)
            )

            # Collect all the contents include the files from the input packages.
            package_contents = []
            package_manifests = []
            for package_manifest_path in package.packages:
                with open(package_manifest_path, "r") as f:
                    package_manifest = json_load(PackageManifest, f)
                    package_manifests += [package_manifest_path]
                    for blob in package_manifest.blobs:
                        # We do not include the meta.far, because assembly will
                        # generate a new one with all the contents.
                        if blob.path != "meta/":
                            package_contents.append(
                                FileEntry(blob.source_path, blob.path)
                            )
            contents = package.contents
            contents.update(package_contents)
            deps.update(package_manifests)

            # Copy the package contents entries
            (copied_package_files, package_deps) = self._copy_file_entries(
                contents,
                os.path.join("compiled_packages", package.name, "files"),
            )

            copied_definition = CompiledPackageDefinition(
                name=package.name,
                components=components,
                includes=copied_includes,
                contents=set(copied_package_files),
                bootfs_package=package.bootfs_package,
            )
            result.packages_to_compile.append(copied_definition)

            deps.update(package_deps)

        # Copy the config_data entries into the out-of-tree layout
        (config_data, config_data_deps) = self._copy_config_data_entries()
        deps.update(config_data_deps)
        result.config_data = config_data

        # Sort the shell commands alphabetically
        result.bootfs_shell_commands = dict(
            sorted(result.bootfs_shell_commands.items())
        )
        result.shell_commands = dict(sorted(result.shell_commands.items()))

        # Write the AIB manifest
        assembly_config_path = os.path.join(self.outdir, "assembly_config.json")
        with open(assembly_config_path, "w") as file:
            result.json_dump(file, indent=2)

        return (result, assembly_config_path, deps)

    def _prepare_packages_for_copying(
        self,
        package_details_list: List[PackageDetails],
    ) -> PackageDetailsList:
        """Queue up the packages for copying, and return the list of package details, using the destination path"""

        # Resultant paths to package manifests
        package_details: PackageDetailsList = []
        manifest_path_mapping: Dict[FilePath, FilePath] = {}

        # Bail early if empty
        if not package_details_list:
            return package_details

        # Open each manifest, record the blobs, and then copy it to its destination,
        # sorted by path to the package manifest.
        for package_detail in package_details_list:
            try:
                (destination_path, manifest) = self.package_copier.add_package(
                    package_detail.package
                )
            except package_copier.DuplicatePackageException as e:
                raise DuplicatePackageException(e)

            self._validate_package_url(manifest, package_detail.set)

            # Track the package manifest in our set of packages
            package_details.append(
                PackageDetails(destination_path, package_detail.set)
            )

            # Create the mapping from the source to the copied path, so we can correct the driver
            # details entries.
            manifest_path_mapping[package_detail.package] = destination_path

        return package_details

    def _prepare_drivers_for_copying(
        self, driver_details_list: List[DriverDetails], pkg_set: str
    ) -> List[DriverDetails]:
        """Queue up the package copying for each driver, returning a DriverDetails with the new destination path"""

        driver_details: List[DriverDetails] = []

        for driver_detail in driver_details_list:
            try:
                (destination_path, manifest) = self.package_copier.add_package(
                    driver_detail.package
                )
            except package_copier.DuplicatePackageException as e:
                raise DuplicatePackageException(e)

            self._validate_package_url(manifest, pkg_set)

            driver_details.append(
                DriverDetails(
                    destination_path,
                    driver_detail.components,
                )
            )

        return driver_details

    def _validate_package_url(
        self, manifest: PackageManifest, pkg_set: str
    ) -> None:
        """Validate that a given package url only appears once in the set of packages being copied"""
        package_url = AIBCreator.package_url_template.format(
            repository=manifest.repository,
            package_name=manifest.package.name,
        )

        # Validate that there's a single source for each package url.
        if package_url not in self.package_urls:
            self.package_urls.add(package_url)
        else:
            raise DuplicatePackageException(
                f"There is a duplicate declaration of {package_url} in {pkg_set}"
            )

    def _copy_component_includes(
        self,
        component_includes: FileEntrySet,
        existing_shard_includes: FileEntrySet,
    ) -> Tuple[FileEntrySet, DepSet]:
        deps: DepSet = set()
        shard_includes: FileEntrySet = set()
        for entry in component_includes:
            rebased_destination = os.path.join(
                "compiled_packages", "include", entry.destination
            )
            copy_destination = os.path.join(self.outdir, rebased_destination)

            rebased_entry = FileEntry(entry.source, rebased_destination)

            # Check whether we have previously specified a different
            # source for the same include file
            if rebased_destination in map(
                lambda x: x.destination,
                existing_shard_includes | shard_includes,
            ):
                if rebased_entry not in existing_shard_includes:
                    raise AssemblyInputBundleCreationException(
                        f"Include file already exists with a different source: {copy_destination}"
                    )
            else:
                # Hardlink the file from the source to the destination
                deps.add(fast_copy_makedirs(entry.source, copy_destination))

            shard_includes.add(rebased_entry)

        return shard_includes, deps

    def _copy_component_shard(
        self, component_shard: FilePath, package_name: str, component_name: str
    ) -> Tuple[FilePath, DepSet]:
        deps: DepSet = set()
        # The shard is copied to a path based on the name of the package, the
        # name of the component, and the filename of the shard:
        # f"compiled_packages/{package_name}/{component_name}/{filename}"
        #
        bundle_destination = os.path.join(
            "compiled_packages",
            package_name,
            component_name,
            os.path.basename(component_shard),
        )

        # The copy destination is the above path, with the bundle's outdir.
        copy_destination = os.path.join(self.outdir, bundle_destination)

        # Hardlink the file from the source to the destination
        deps.add(fast_copy_makedirs(component_shard, copy_destination))
        return bundle_destination, deps

    def _copy_component_shards(
        self,
        component_shards: Union[ComponentShards, List[FilePath]],
        package_name: str,
        component_name: str,
    ) -> Tuple[List[FilePath], DepSet]:
        shard_file_paths: List[FilePath] = list()
        deps: DepSet = set()
        for shard in component_shards:
            destination, copy_deps = self._copy_component_shard(
                shard, package_name, component_name
            )
            deps.update(copy_deps)
            shard_file_paths.append(destination)
        return shard_file_paths, deps

    def _copy_file_entries(
        self, entries: Union[FileEntrySet, FileEntryList], subdirectory: str
    ) -> Tuple[FileEntryList, DepSet]:
        results: FileEntryList = []
        deps: DepSet = set()

        # Bail early if nothing to do
        if len(entries) == 0:
            return (results, deps)

        for entry in entries:
            rebased_destination = os.path.join(subdirectory, entry.destination)
            copy_destination = os.path.join(self.outdir, rebased_destination)

            # Hardlink the file from source to the destination, relative to the
            # directory for all entries.
            deps.add(fast_copy_makedirs(entry.source, copy_destination))

            # Make a new FileEntry, which has a source of the path within the
            # out-of-tree layout, and the same destination.
            results.append(
                FileEntry(
                    source=rebased_destination, destination=entry.destination
                )
            )

        return (results, deps)

    def _copy_config_data_entries(self) -> Tuple[ConfigDataEntries, DepSet]:
        """
        Take a list of entries for the config_data package, copy them into the
        appropriate layout for the assembly input bundle, and then return the
        config data entries and the set of DepEntries from the copying

        This expects the entries to be destined for:
        `meta/data/<package>/<path/to/file>`

        and creates a ConfigDataEntries dict of PackageName:FileEntryList.
        """
        results: ConfigDataEntries = {}
        deps: DepSet = set()

        if len(self.config_data) == 0:
            return (results, deps)

        # Make a sorted list of a deduplicated set of the entries.
        for entry in sorted(set(self.config_data)):
            # Crack the in-package path apart
            #
            # "meta" / "data" / package_name / path/to/file
            parts = pathlib.Path(entry.destination).parts
            if parts[:2] != ("meta", "data"):
                raise ValueError(
                    "Found an unexpected destination path: {}".format(parts)
                )
            package_name = parts[2]
            file_path = os.path.join(*parts[3:])

            rebased_source_path = os.path.join(
                "config_data", package_name, file_path
            )
            copy_destination = os.path.join(self.outdir, rebased_source_path)

            # Hardlink the file from source to the destination
            deps.add(fast_copy_makedirs(entry.source, copy_destination))

            # Append the entry to the set of entries for the package
            results.setdefault(package_name, set()).add(
                FileEntry(source=rebased_source_path, destination=file_path)
            )

        return (results, deps)
