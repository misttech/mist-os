# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Public definitions for Assembly related rules."""

load(
    "//fuchsia/private/assembly:fuchsia_prebuilt_package.bzl",
    _fuchsia_prebuilt_package = "fuchsia_prebuilt_package",
)
load(
    "//fuchsia/private/assembly:fuchsia_package_directory.bzl",
    _fuchsia_package_directory = "fuchsia_package_directory",
)
load(
    "//fuchsia/private/assembly:fuchsia_assemble_package.bzl",
    _fuchsia_assemble_package = "fuchsia_assemble_package",
)
load(
    "//fuchsia/private/assembly:fuchsia_product_image.bzl",
    _fuchsia_product_assembly = "fuchsia_product_assembly",
    _fuchsia_product_create_system = "fuchsia_product_create_system",
    _fuchsia_product_image = "fuchsia_product_image",
)
load(
    "//fuchsia/private/assembly:fuchsia_product_configuration.bzl",
    _BUILD_TYPES = "BUILD_TYPES",
    _INPUT_DEVICE_TYPE = "INPUT_DEVICE_TYPE",
    _fuchsia_product_configuration = "fuchsia_product_configuration",
)
load(
    "//fuchsia/private/assembly:fuchsia_virtual_device.bzl",
    _ARCH = "ARCH",
    _fuchsia_virtual_device = "fuchsia_virtual_device",
)
load(
    "//fuchsia/private/assembly:fuchsia_board_configuration.bzl",
    _fuchsia_board_configuration = "fuchsia_board_configuration",
    _fuchsia_prebuilt_board_configuration = "fuchsia_prebuilt_board_configuration",
)
load(
    "//fuchsia/private/assembly:providers.bzl",
    _FuchsiaProductAssemblyBundleInfo = "FuchsiaProductAssemblyBundleInfo",
    _FuchsiaProductAssemblyInfo = "FuchsiaProductAssemblyInfo",
    _FuchsiaProductImageInfo = "FuchsiaProductImageInfo",
    _FuchsiaScrutinyConfigInfo = "FuchsiaScrutinyConfigInfo",
)
load(
    "//fuchsia/private/assembly:assembly_bundle.bzl",
    _assembly_bundle = "assembly_bundle",
)
load(
    "//fuchsia/private/assembly:fuchsia_partitions_configuration.bzl",
    _fuchsia_partitions_configuration = "fuchsia_partitions_configuration",
)
load(
    "//fuchsia/private/assembly:fuchsia_product_bundle.bzl",
    _fuchsia_product_bundle = "fuchsia_product_bundle",
)
load(
    "//fuchsia/private/assembly:fuchsia_bootstrap_partition.bzl",
    _fuchsia_bootstrap_partition = "fuchsia_bootstrap_partition",
)
load(
    "//fuchsia/private/assembly:fuchsia_bootloader_partition.bzl",
    _fuchsia_bootloader_partition = "fuchsia_bootloader_partition",
)
load(
    "//fuchsia/private/assembly:fuchsia_partition.bzl",
    _PARTITION_TYPE = "PARTITION_TYPE",
    _SLOT = "SLOT",
    _fuchsia_partition = "fuchsia_partition",
)
load(
    "//fuchsia/private/assembly:fuchsia_scrutiny_config.bzl",
    _fuchsia_scrutiny_config = "fuchsia_scrutiny_config",
)
load(
    "//fuchsia/private/assembly:fuchsia_update_package.bzl",
    _fuchsia_update_package = "fuchsia_update_package",
)
load(
    "//fuchsia/private/assembly:fuchsia_size_checker.bzl",
    _fuchsia_size_checker = "fuchsia_size_checker",
)
load(
    "//fuchsia/private/assembly:fuchsia_elf_sizes.bzl",
    _fuchsia_elf_sizes = "fuchsia_elf_sizes",
)
load(
    "//fuchsia/private/assembly:fuchsia_repository_keys.bzl",
    _fuchsia_repository_keys = "fuchsia_repository_keys",
)
load(
    "//fuchsia/private/workflows:fuchsia_task_flash.bzl",
    _fuchsia_task_flash = "fuchsia_task_flash",
)

# Rules
assembly_bundle = _assembly_bundle
fuchsia_prebuilt_package = _fuchsia_prebuilt_package
fuchsia_package_directory = _fuchsia_package_directory
fuchsia_assemble_package = _fuchsia_assemble_package
fuchsia_product_configuration = _fuchsia_product_configuration
fuchsia_virtual_device = _fuchsia_virtual_device
fuchsia_board_configuration = _fuchsia_board_configuration
fuchsia_prebuilt_board_configuration = _fuchsia_prebuilt_board_configuration
fuchsia_product_image = _fuchsia_product_image
fuchsia_product_create_system = _fuchsia_product_create_system
fuchsia_product_assembly = _fuchsia_product_assembly
fuchsia_partitions_configuration = _fuchsia_partitions_configuration
fuchsia_product_bundle = _fuchsia_product_bundle
fuchsia_size_checker = _fuchsia_size_checker
fuchsia_elf_sizes = _fuchsia_elf_sizes
fuchsia_update_package = _fuchsia_update_package
fuchsia_repository_keys = _fuchsia_repository_keys
fuchsia_task_flash = _fuchsia_task_flash
fuchsia_scrutiny_config = _fuchsia_scrutiny_config

fuchsia_bootstrap_partition = _fuchsia_bootstrap_partition
fuchsia_bootloader_partition = _fuchsia_bootloader_partition
fuchsia_partition = _fuchsia_partition

# Providers
FuchsiaProductImageInfo = _FuchsiaProductImageInfo
FuchsiaProductAssemblyBundleInfo = _FuchsiaProductAssemblyBundleInfo
FuchsiaScrutinyConfigInfo = _FuchsiaScrutinyConfigInfo
FuchsiaProductAssemblyInfo = _FuchsiaProductAssemblyInfo

# constants
BUILD_TYPES = _BUILD_TYPES
PARTITION_TYPE = _PARTITION_TYPE
SLOT = _SLOT
ARCH = _ARCH
INPUT_DEVICE_TYPE = _INPUT_DEVICE_TYPE
