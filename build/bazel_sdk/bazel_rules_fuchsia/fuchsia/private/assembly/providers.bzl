# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Assembly related Providers."""

load("//fuchsia/private:providers.bzl", _FuchsiaProductBundleInfo = "FuchsiaProductBundleInfo")

FuchsiaAssembledPackageInfo = provider(
    "Packages that can be included into a product. It consists of the package and the corresponding config data.",
    fields = {
        "package": "The base package",
        "configs": "A list of configs that is attached to packages",
        "files": "Files needed by package and config files.",
    },
)

FuchsiaConfigDataInfo = provider(
    "The  config data which is used in assembly.",
    fields = {
        "source": "Config file on host",
        "destination": "A String indicating the path to find the file in the package on the target",
    },
)

FuchsiaProductConfigInfo = provider(
    doc = "A product-info used to containing the product_config.json and deps.",
    fields = {
        "product_config": "The JSON product configuration file.",
    },
)

FuchsiaBoardConfigInfo = provider(
    doc = "A board-info used to containing the board_config.json and deps.",
    fields = {
        "board_config": "The JSON board configuration file.",
    },
)

FuchsiaAssemblyConfigInfo = provider(
    doc = "Private provider that includes a single JSON configuration file.",
    fields = {
        "config": "JSON configuration file",
    },
)

FuchsiaSizeCheckerInfo = provider(
    doc = """Size reports created by size checker tool.""",
    fields = {
        "size_report": "size_report.json file",
        "verbose_output": "verbose version of size report file",
    },
)

FuchsiaVirtualDeviceInfo = provider(
    doc = "A virtual device spec file which is a single JSON configuration file.",
    fields = {
        "device_name": "Name of the virtual device",
        "config": "JSON configuration file",
        "template": "QEMU start up arguments template",
    },
)

FuchsiaProductAssemblyBundleInfo = provider(
    doc = """
A bundle of files used by product assembly.
This should only be provided by the single exported target of a
fuchsia_product_assembly_bundle repository.
""",
    fields = {
        "dir": "Path to the bundle directory",
        "files": "All files contained in the bundle",
    },
)

FuchsiaProductImageInfo = provider(
    doc = "Info needed to pave a Fuchsia image",
    fields = {
        "images_out": "images out directory",
        "product_assembly_out": "product assembly out directory",
        "platform_aibs": "platform aibs file listing path to platform AIBS",
    },
)

FuchsiaUpdatePackageInfo = provider(
    doc = "Info for created update package",
    fields = {
        "update_out": "update out directory",
    },
)

FuchsiaProductAssemblyInfo = provider(
    doc = "Info populated by product assembly",
    fields = {
        "product_assembly_out": "product assembly out directory",
        "platform_aibs": "platform aibs file listing path to platform AIBS",
    },
)

FuchsiaProductBundleInfo = _FuchsiaProductBundleInfo

FuchsiaPartitionInfo = provider(
    doc = "Mapping of images to partitions.",
    fields = {
        "partition": "partition in dict",
    },
)

FuchsiaScrutinyConfigInfo = provider(
    doc = "A set of scrutiny configs.",
    fields = {
        "bootfs_files": "Set of files expected in bootfs",
        "bootfs_packages": "Set of packages expected in bootfs",
        "kernel_cmdline": "Set of cmdline args expected to be passed to the kernel",
        "component_tree_config": "Tree of expected component routes",
        "routes_config_golden": "Config file for route resources validation",
        "component_resolver_allowlist": "Allowlist of components that can be resolved using privileged component resolvers",
        "component_route_exceptions": "Allowlist of all capability routes that are exempt from route checking",
        "base_packages": "Set of base packages expected in the fvm",
        "structured_config_policy": "File describing the policy of structured config",
    },
)

FuchsiaRepositoryKeysInfo = provider(
    doc = "A directory containing Fuchsia TUF repository keys.",
    fields = {"dir": "Path to the directory"},
)
