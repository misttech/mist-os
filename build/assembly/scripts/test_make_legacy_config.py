#!/usr/bin/env fuchsia-vendored-python
# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

from collections import namedtuple
from contextlib import contextmanager
from functools import partial
import hashlib
import json
import os
import tempfile
import unittest
from unittest import mock

from assembly import (
    FileEntry,
    ImageAssemblyConfig,
    PackageManifest,
    BlobEntry,
    PackageMetaData,
    KernelInfo,
)
from assembly.assembly_input_bundle import (
    CompiledPackageAdditionalShards,
    CompiledPackageMainDefinition,
    DriverDetails,
    PackageDetails,
    DuplicatePackageException,
)
from fast_copy_mock import mock_fast_copy_in
import make_legacy_config
import assembly
import serialization


def make_merkle(blob_name: str) -> str:
    """Creates a "merkle" by hashing the blob_name to get a unique value."""
    m = hashlib.sha256()
    m.update(blob_name.encode("utf-8"))
    return m.hexdigest()


def _make_package_contents(package_name, blobs, source_dir) -> PackageManifest:
    manifest = PackageManifest(
        PackageMetaData(package_name), [], repository="fuchsia.com"
    )

    # Create blob entries (that don't need to fully exist)
    for blob_name in blobs:
        entry = BlobEntry(
            blob_name,
            make_merkle(package_name + blob_name),
            None,
            os.path.join(source_dir, package_name, blob_name),
        )
        manifest.blobs.append(entry)
    return manifest


def make_package_manifest(package_name, blobs, source_dir):
    manifest = _make_package_contents(package_name, blobs, source_dir)

    # Write the manifest out to the temp dir
    manifest_path = os.path.join(source_dir, f"{package_name}.json")
    with open(manifest_path, "w") as manifest_file:
        serialization.json_dump(manifest, manifest_file, indent=2)

    return manifest_path


def make_image_assembly_path(package_name):
    return "source/" + package_name + ".json"


def make_package_path(package_name):
    return "packages/" + package_name


TestSetupArgs = namedtuple(
    "TestSetupArgs",
    "base, cache, system, bootfs_packages, kernel, boot_args, shell_commands_file, driver_manifest_path, driver_component_file, bootfs",
)
SOURCE_DIR = "source"
OUTDIR = "outdir"


@contextmanager
def setup_temp_dir(*args, **kwargs):
    temp_dir = tempfile.TemporaryDirectory()
    try:
        os.chdir(temp_dir.name)
        os.mkdir(SOURCE_DIR)

        # Write out package manifests which are part of the package.
        base = set()
        cache = set()
        system = set()
        for package_set in ["base", "cache", "system"]:
            for suffix in ["a", "b"]:
                package_name = f"{package_set}_{suffix}"
                blob_suffixes = ["1", "2", "3"]
                blob_names = [
                    f"internal/path/file_{suffix}_{blob_suffix}"
                    for blob_suffix in blob_suffixes
                ]

                manifest_path = make_package_manifest(
                    package_name, blob_names, SOURCE_DIR
                )

                locals()[package_set].add(manifest_path)

        # Write out the bootfs files package.
        bootfs = make_package_manifest(
            "bootfs_files_package",
            [
                "some/file",
                "another/file",
            ],
            SOURCE_DIR,
        )

        # Write out a driver package
        package_name = "base_driver"
        blob_names = ["meta/driver.cm"]
        driver_manifest_path = make_package_manifest(
            package_name, blob_names, SOURCE_DIR
        )

        # Write out the driver component files list
        distribution_manifest_path = os.path.join(
            SOURCE_DIR, "base_driver_distribution_manifest.json"
        )
        driver_component_file = {
            "package_name": "base_driver",
            "distribution_manifest": distribution_manifest_path,
        }
        shell_commands_file = {
            "accountctl": ["accountctl"],
            "activity-ctl": ["activity_ctl"],
            "audio_listener": ["audio_listener"],
        }
        # Write out the driver component distribution manifest
        with open(
            distribution_manifest_path, "w"
        ) as distribution_manifest_file:
            # We only care about the destination field
            driver_distribution_manifest = [{"destination": "meta/driver.cm"}]
            json.dump(
                driver_distribution_manifest,
                distribution_manifest_file,
                indent=2,
            )

        # Add the rest of the fields we expect to see in an image_assembly
        # config.
        boot_args = set(["boot-arg-1", "boot-arg-2"])
        kernel = KernelInfo()
        kernel.path = os.path.join(SOURCE_DIR, "kernel.bin")
        kernel.args.update(["arg1", "arg2"])
        kernel.clock_backstop = 123456

        # Add the bootfs files that are listed in package manifests.
        package_name = "bootfs"
        blob_names = ["some/file", "another/file"]
        bootfs_package_manifest = make_package_manifest(
            package_name, blob_names, SOURCE_DIR
        )
        bootfs_packages = set([bootfs_package_manifest])

        yield TestSetupArgs(
            base,
            cache,
            system,
            bootfs_packages,
            kernel,
            boot_args,
            shell_commands_file,
            driver_manifest_path,
            driver_component_file,
            bootfs,
        )
    finally:
        temp_dir.cleanup()


class MakeLegacyConfig(unittest.TestCase):
    def test_make_legacy_config(self):
        self.maxDiff = None

        # Patch in a mock for the fast_copy() fn
        _, copies = mock_fast_copy_in(assembly.assembly_input_bundle)

        with setup_temp_dir() as setup_args:
            (
                base,
                cache,
                system,
                bootfs_packages,
                kernel,
                boot_args,
                shell_commands_file,
                driver_manifest_path,
                driver_component_file,
                bootfs,
            ) = setup_args
            # Create the outdir path, and perform the "copying" into the
            # AssemblyInputBundle.
            aib, _, deps = make_legacy_config.copy_to_assembly_input_bundle(
                base=base,
                cache=cache,
                system=system,
                bootfs_packages=bootfs_packages,
                kernel=kernel,
                boot_args=boot_args,
                config_data_entries=[],
                outdir=OUTDIR,
                base_driver_packages_list=[driver_manifest_path],
                base_driver_components_files_list=[driver_component_file],
                boot_driver_packages_list=[],
                boot_driver_components_files_list=[],
                shell_commands=shell_commands_file,
                core_realm_shards=[
                    os.path.join(SOURCE_DIR, "core/realm/shard1.cml"),
                    os.path.join(SOURCE_DIR, "core/realm/shard2.cml"),
                ],
                bootfs_files_package=bootfs,
            )
            file_paths = aib.all_file_paths()

            # Validate the contents of the AssemblyInputBundle itself
            self.assertEqual(
                aib.packages,
                set(
                    [
                        PackageDetails("packages/base_a", "base"),
                        PackageDetails("packages/base_b", "base"),
                        PackageDetails("packages/cache_a", "cache"),
                        PackageDetails("packages/cache_b", "cache"),
                        PackageDetails("packages/system_a", "system"),
                        PackageDetails("packages/system_b", "system"),
                        PackageDetails("packages/bootfs", "bootfs"),
                    ]
                ),
            )
            self.assertEqual(aib.boot_args, set(["boot-arg-1", "boot-arg-2"]))
            self.assertEqual(aib.kernel.path, "kernel/kernel.bin")
            self.assertEqual(aib.kernel.args, set(["arg1", "arg2"]))
            self.assertEqual(aib.kernel.clock_backstop, 123456)
            self.assertEqual(
                aib.bootfs_files_package,
                "packages/bootfs_files_package",
            )
            self.assertEqual(aib.bootfs_files, set())

            self.assertEqual(
                aib.base_drivers,
                [DriverDetails("packages/base_driver", ["meta/driver.cm"])],
            )
            self.assertEqual(aib.shell_commands, shell_commands_file)
            self.assertEqual(
                aib.packages_to_compile,
                [
                    CompiledPackageAdditionalShards(
                        name="core",
                        component_shards={
                            "core": set(
                                [
                                    "compiled_packages/core/core/shard1.cml",
                                    "compiled_packages/core/core/shard2.cml",
                                ]
                            )
                        },
                    ),
                ],
            )

            # Make sure all the manifests were created in the correct location.
            for package_set in ["base", "cache", "system"]:
                for suffix in ["a", "b"]:
                    package_name = f"{package_set}_{suffix}"
                    with open(
                        f"outdir/packages/{package_name}"
                    ) as manifest_file:
                        manifest = serialization.json_load(
                            PackageManifest, manifest_file
                        )
                        self.assertEqual(manifest.package.name, package_name)
                        self.assertEqual(
                            set(manifest.blobs_by_path().keys()),
                            set(
                                [
                                    f"internal/path/file_{suffix}_1",
                                    f"internal/path/file_{suffix}_2",
                                    f"internal/path/file_{suffix}_3",
                                ]
                            ),
                        )

            # Spot-check one of the manifests, that it contains the correct
            # source paths to the blobs.
            with open("outdir/packages/base_a") as manifest_file:
                manifest = serialization.json_load(
                    PackageManifest, manifest_file
                )
                self.assertEqual(manifest.package.name, "base_a")
                self.assertEqual(len(manifest.blobs), 3)
                blobs = manifest.blobs_by_path()
                self.assertEqual(
                    blobs["internal/path/file_a_1"].source_path,
                    "../blobs/efac096092f7cf879c72ac51d23d9f142e97405dec7dd9c69aeee81de083f794",
                )
                self.assertEqual(
                    blobs["internal/path/file_a_1"].merkle,
                    "efac096092f7cf879c72ac51d23d9f142e97405dec7dd9c69aeee81de083f794",
                )
                self.assertEqual(
                    blobs["internal/path/file_a_2"].source_path,
                    "../blobs/bf0c3ae1356b5863258f73a37d555cf878007b8bfe4fd780d74466ec62fe062d",
                )
                self.assertEqual(
                    blobs["internal/path/file_a_2"].merkle,
                    "bf0c3ae1356b5863258f73a37d555cf878007b8bfe4fd780d74466ec62fe062d",
                )
                self.assertEqual(
                    blobs["internal/path/file_a_3"].source_path,
                    "../blobs/a2e574ccd55c815f0a87c4f27e7a3115fe8e46d41a2e0caf2a91096a41421f78",
                )
                self.assertEqual(
                    blobs["internal/path/file_a_3"].merkle,
                    "a2e574ccd55c815f0a87c4f27e7a3115fe8e46d41a2e0caf2a91096a41421f78",
                )

            # Validate that the deps were correctly identified (all package
            # manifest paths, the blob source paths, the bootfs source paths,
            # and the kernel source path)
            self.assertEqual(
                deps,
                set(
                    [
                        "source/base_a.json",
                        "source/base_a/internal/path/file_a_1",
                        "source/base_a/internal/path/file_a_2",
                        "source/base_a/internal/path/file_a_3",
                        "source/base_b.json",
                        "source/base_b/internal/path/file_b_1",
                        "source/base_b/internal/path/file_b_2",
                        "source/base_b/internal/path/file_b_3",
                        "source/base_driver.json",
                        "source/base_driver/meta/driver.cm",
                        "source/base_driver_distribution_manifest.json",
                        "source/cache_a.json",
                        "source/cache_a/internal/path/file_a_1",
                        "source/cache_a/internal/path/file_a_2",
                        "source/cache_a/internal/path/file_a_3",
                        "source/cache_b.json",
                        "source/cache_b/internal/path/file_b_1",
                        "source/cache_b/internal/path/file_b_2",
                        "source/cache_b/internal/path/file_b_3",
                        "source/system_a.json",
                        "source/system_a/internal/path/file_a_1",
                        "source/system_a/internal/path/file_a_2",
                        "source/system_a/internal/path/file_a_3",
                        "source/system_b.json",
                        "source/system_b/internal/path/file_b_1",
                        "source/system_b/internal/path/file_b_2",
                        "source/system_b/internal/path/file_b_3",
                        "source/kernel.bin",
                        "source/bootfs_files_package.json",
                        "source/bootfs_files_package/some/file",
                        "source/bootfs_files_package/another/file",
                        "source/core/realm/shard1.cml",
                        "source/core/realm/shard2.cml",
                        "source/bootfs/some/file",
                        "source/bootfs.json",
                        "source/bootfs/another/file",
                    ]
                ),
            )

            # Validate that all the files were correctly copied to the
            # correct paths in the AIB.
            self.assertEqual(
                set(copies),
                set(
                    [
                        FileEntry(
                            source="source/base_a/internal/path/file_a_1",
                            destination="outdir/blobs/efac096092f7cf879c72ac51d23d9f142e97405dec7dd9c69aeee81de083f794",
                        ),
                        FileEntry(
                            source="source/base_a/internal/path/file_a_1",
                            destination="outdir/blobs/efac096092f7cf879c72ac51d23d9f142e97405dec7dd9c69aeee81de083f794",
                        ),
                        FileEntry(
                            source="source/base_a/internal/path/file_a_2",
                            destination="outdir/blobs/bf0c3ae1356b5863258f73a37d555cf878007b8bfe4fd780d74466ec62fe062d",
                        ),
                        FileEntry(
                            source="source/base_a/internal/path/file_a_3",
                            destination="outdir/blobs/a2e574ccd55c815f0a87c4f27e7a3115fe8e46d41a2e0caf2a91096a41421f78",
                        ),
                        FileEntry(
                            source="source/base_b/internal/path/file_b_1",
                            destination="outdir/blobs/ae9fd81e1c2fd1b084ec2c362737e812c5ef9b3aa8cb0538ec8e2269ea7fbe1a",
                        ),
                        FileEntry(
                            source="source/base_b/internal/path/file_b_2",
                            destination="outdir/blobs/d3cd38c4881c3bc31f1e2e397a548d431a6430299785446f28be10cc5b76d92b",
                        ),
                        FileEntry(
                            source="source/base_b/internal/path/file_b_3",
                            destination="outdir/blobs/6468d9d6761c8afcc97744dfd9e066f29bb697a9a0c8248b5e6eec989134a048",
                        ),
                        FileEntry(
                            source="source/base_driver/meta/driver.cm",
                            destination="outdir/blobs/38b7b79ef8e827ea8d283d4e01d61563a8feeecf95650f224c047502ea1edb4b",
                        ),
                        FileEntry(
                            source="source/cache_a/internal/path/file_a_1",
                            destination="outdir/blobs/f0601d51be1ec8c11d825b756841937706eb2805ce9b924b67b4b0dc14caba29",
                        ),
                        FileEntry(
                            source="source/cache_a/internal/path/file_a_2",
                            destination="outdir/blobs/1834109a42a5ff6501fbe05216475b2b0acc44e0d9c94924469a485d6f45dc86",
                        ),
                        FileEntry(
                            source="source/cache_a/internal/path/file_a_3",
                            destination="outdir/blobs/0f32059964674afd810001c76c2a5d783a2ce012c41303685ec1adfdb83290fd",
                        ),
                        FileEntry(
                            source="source/cache_b/internal/path/file_b_1",
                            destination="outdir/blobs/301e8584305e63f0b764daf52dcf312eecb6378b201663fcc77d7ad68aab1f23",
                        ),
                        FileEntry(
                            source="source/cache_b/internal/path/file_b_2",
                            destination="outdir/blobs/8135016519df51d386efaea9b02f50cb454b6c7afe69c77895c1d4d844c3584d",
                        ),
                        FileEntry(
                            source="source/cache_b/internal/path/file_b_3",
                            destination="outdir/blobs/b548948fd2dc40574775308a92a8330e5c5d84ddf31513d1fe69964b458479e7",
                        ),
                        FileEntry(
                            source="source/system_a/internal/path/file_a_1",
                            destination="outdir/blobs/8ca898b1389c58b6cd9a6a777e320f2756ab3437b402c61d774dd2758ad9cf06",
                        ),
                        FileEntry(
                            source="source/system_a/internal/path/file_a_2",
                            destination="outdir/blobs/ef84c6711eaba482164fe4eb08a6c45f18fe62d493e5a31a631c32937bf7229d",
                        ),
                        FileEntry(
                            source="source/system_a/internal/path/file_a_3",
                            destination="outdir/blobs/d66cb673257e25393a319fb2c3e9745ef6e0f1cfa4fb89c5576df73cd3eba586",
                        ),
                        FileEntry(
                            source="source/system_b/internal/path/file_b_1",
                            destination="outdir/blobs/fd0891d15ce65d7682f7437e441e917b8ed4bde4db07a11dc100104f25056051",
                        ),
                        FileEntry(
                            source="source/system_b/internal/path/file_b_2",
                            destination="outdir/blobs/c244c7c6ebf40a9a4c9d59e7b08a1cf54ae3d60404d1cecb417a7b55cc308d91",
                        ),
                        FileEntry(
                            source="source/system_b/internal/path/file_b_3",
                            destination="outdir/blobs/0cdbf3e4f1246ce7522e78c21bcf1c3aef2d41ac2b4de3f0ee98fc6273f62eb9",
                        ),
                        FileEntry(
                            source="source/core/realm/shard1.cml",
                            destination="outdir/compiled_packages/core/core/shard1.cml",
                        ),
                        FileEntry(
                            source="source/core/realm/shard2.cml",
                            destination="outdir/compiled_packages/core/core/shard2.cml",
                        ),
                        FileEntry(
                            source="source/kernel.bin",
                            destination="outdir/kernel/kernel.bin",
                        ),
                        FileEntry(
                            source="source/bootfs_files_package/another/file",
                            destination="outdir/blobs/809aae59e2ca524e21f93c4e5747fc718b6f61aefe9eabc46f981d5856b88de3",
                        ),
                        FileEntry(
                            source="source/bootfs_files_package/some/file",
                            destination="outdir/blobs/1be711a4235aefc04302d3a20eefafcf0ac26b573881bc7967479aff6e1e8dd7",
                        ),
                        FileEntry(
                            source="source/bootfs/another/file",
                            destination="outdir/blobs/ce856fe1e31bd6ed129db4a0145cc9670dfcbeef414e65b28f9def62679540e9",
                        ),
                        FileEntry(
                            source="source/bootfs/some/file",
                            destination="outdir/blobs/b3a484b7b9bc926298dc6ebeb12556641e21adc6708f3f59f1b43ade3f9f627c",
                        ),
                    ]
                ),
            )

            # Validate that the output manifest will have the right file paths
            self.assertEqual(
                sorted(file_paths),
                [
                    "blobs/0cdbf3e4f1246ce7522e78c21bcf1c3aef2d41ac2b4de3f0ee98fc6273f62eb9",
                    "blobs/0f32059964674afd810001c76c2a5d783a2ce012c41303685ec1adfdb83290fd",
                    "blobs/1834109a42a5ff6501fbe05216475b2b0acc44e0d9c94924469a485d6f45dc86",
                    "blobs/1be711a4235aefc04302d3a20eefafcf0ac26b573881bc7967479aff6e1e8dd7",
                    "blobs/301e8584305e63f0b764daf52dcf312eecb6378b201663fcc77d7ad68aab1f23",
                    "blobs/38b7b79ef8e827ea8d283d4e01d61563a8feeecf95650f224c047502ea1edb4b",
                    "blobs/6468d9d6761c8afcc97744dfd9e066f29bb697a9a0c8248b5e6eec989134a048",
                    "blobs/809aae59e2ca524e21f93c4e5747fc718b6f61aefe9eabc46f981d5856b88de3",
                    "blobs/8135016519df51d386efaea9b02f50cb454b6c7afe69c77895c1d4d844c3584d",
                    "blobs/8ca898b1389c58b6cd9a6a777e320f2756ab3437b402c61d774dd2758ad9cf06",
                    "blobs/a2e574ccd55c815f0a87c4f27e7a3115fe8e46d41a2e0caf2a91096a41421f78",
                    "blobs/ae9fd81e1c2fd1b084ec2c362737e812c5ef9b3aa8cb0538ec8e2269ea7fbe1a",
                    "blobs/b3a484b7b9bc926298dc6ebeb12556641e21adc6708f3f59f1b43ade3f9f627c",
                    "blobs/b548948fd2dc40574775308a92a8330e5c5d84ddf31513d1fe69964b458479e7",
                    "blobs/bf0c3ae1356b5863258f73a37d555cf878007b8bfe4fd780d74466ec62fe062d",
                    "blobs/c244c7c6ebf40a9a4c9d59e7b08a1cf54ae3d60404d1cecb417a7b55cc308d91",
                    "blobs/ce856fe1e31bd6ed129db4a0145cc9670dfcbeef414e65b28f9def62679540e9",
                    "blobs/d3cd38c4881c3bc31f1e2e397a548d431a6430299785446f28be10cc5b76d92b",
                    "blobs/d66cb673257e25393a319fb2c3e9745ef6e0f1cfa4fb89c5576df73cd3eba586",
                    "blobs/ef84c6711eaba482164fe4eb08a6c45f18fe62d493e5a31a631c32937bf7229d",
                    "blobs/efac096092f7cf879c72ac51d23d9f142e97405dec7dd9c69aeee81de083f794",
                    "blobs/f0601d51be1ec8c11d825b756841937706eb2805ce9b924b67b4b0dc14caba29",
                    "blobs/fd0891d15ce65d7682f7437e441e917b8ed4bde4db07a11dc100104f25056051",
                    "compiled_packages/core/core/shard1.cml",
                    "compiled_packages/core/core/shard2.cml",
                    "kernel/kernel.bin",
                    "packages/base_a",
                    "packages/base_b",
                    "packages/bootfs",
                    "packages/cache_a",
                    "packages/cache_b",
                    "packages/system_a",
                    "packages/system_b",
                ],
            )

    def test_package_found_in_base_and_cache(self):
        """
        Asserts that the copy_to_assembly_input_bundle function has the side effect of
        assigning packages found in both base and cache in the image assembly config to
        only be added in base
        """
        with setup_temp_dir() as setup_args:
            # Patch in a mock for the fast_copy() fn
            mock_fast_copy_in(assembly.assembly_input_bundle)
            duplicate_package = "cache_a"
            manifest_path = make_package_manifest(
                duplicate_package, [], SOURCE_DIR
            )

            # Copies legacy config into AIB
            aib, _, _ = make_legacy_config.copy_to_assembly_input_bundle(
                [manifest_path],
                [manifest_path],
                [],
                [],
                KernelInfo(),
                [],
                [],
                OUTDIR,
                [],
                [],
                [],
                [],
                dict(),
                set(),
                None,
            )

            # Asserts that the duplicate package is present in the base package set after
            # being copied to the AIB, and not cache.
            self.assertEqual(
                aib.packages,
                set(
                    [
                        PackageDetails(
                            make_package_path(duplicate_package), "base"
                        ),
                    ]
                ),
            )

    def test_driver_package_removed_from_base(self):
        """
        Asserts that the copy_to_assembly_input_bundle function has the side effect of
        removing packages from base if they are listed as driver packages, and adds them to the
        base_drivers package set in the AIB.
        """
        with setup_temp_dir() as setup_args:
            # Patch in a mock for the fast_copy() fn
            mock_fast_copy_in(assembly.assembly_input_bundle)
            duplicate_package = "base_driver_package"
            manifest_path = make_package_manifest(
                duplicate_package, [], SOURCE_DIR
            )

            # Replace the AIBCreator._get_driver_details function within the
            # context manager with a mock.
            with mock.patch.object(
                make_legacy_config.AIBCreator, "_get_driver_details"
            ) as patched_method:
                # This fixed return value should add the contents of the set in the first index
                # of the tuple to the aib.base_drivers, and remove it from the aib.base package set
                # when we copy the contents of the image assembly to the AIB
                patched_method.return_value = (
                    {make_package_path(duplicate_package)},
                    list(),
                )
                aib, _, _ = make_legacy_config.copy_to_assembly_input_bundle(
                    [manifest_path],
                    [],
                    [],
                    [],
                    KernelInfo(),
                    [],
                    [],
                    OUTDIR,
                    [manifest_path],
                    [],
                    [],
                    [],
                    dict(),
                    set(),
                    None,
                )

            self.assertEqual(aib.packages, set([]))
            self.assertIn(
                make_package_path(duplicate_package), aib.base_drivers
            )

    def test_different_manifest_same_pkg_name(self):
        """
        Asserts that when a package is found in a package set that has a different package
        manifest path, but the same package name, assembly will raise a DuplicatePackageException.
        """
        with setup_temp_dir() as setup_args:
            # Patch in a mock for the fast_copy() fn
            mock_fast_copy_in(assembly.assembly_input_bundle)
            duplicate_package = "package_a"
            manifest_path = make_package_manifest(
                duplicate_package, [], SOURCE_DIR
            )
            different_path = "different_path/"
            manifest2 = _make_package_contents(
                duplicate_package, [], SOURCE_DIR
            )

            os.mkdir(os.path.join(SOURCE_DIR, different_path))
            # Write the manifest out to the temp dir
            manifest2_path = os.path.join(
                SOURCE_DIR, different_path, f"{duplicate_package}.json"
            )
            with open(manifest2_path, "w+") as manifest_file:
                serialization.json_dump(manifest2, manifest_file, indent=2)

            # Add manifest path to both base and cache
            base = set([manifest_path, manifest2_path])

            # Copies legacy config into AIB
            self.assertRaises(
                DuplicatePackageException,
                partial(
                    make_legacy_config.copy_to_assembly_input_bundle,
                    base,
                    [],
                    [],
                    [],
                    KernelInfo(),
                    [],
                    [],
                    OUTDIR,
                    [],
                    [],
                    [],
                    [],
                    dict(),
                    set(),
                    None,
                ),
            )
