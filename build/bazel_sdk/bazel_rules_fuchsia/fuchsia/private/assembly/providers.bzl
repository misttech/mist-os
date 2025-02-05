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
        "build_id_dirs": "Directories containing the debug symbols",
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
        # TODO(https://fxbug.dev/390189313): Remove this once all product
        # configs have a consistent layout.
        "config_path": "An optional path to the config inside the directory",
        "directory": "Directory of the product config container",
        "build_type": "The build type of the product.",
        "build_id_dirs": "Directories containing the debug symbols",
    },
)

FuchsiaBoardInputBundleInfo = provider(
    doc = "A board input bundle info used to containing the board input bundle directory",
    fields = {
        "config": "The config file located in the root directory containing Board Input Bundle.",
        "files": "All files belong to Board Input Bundles",
        "build_id_dirs": "Directories containing the debug symbols",
    },
)

FuchsiaBoardConfigInfo = provider(
    doc = "A board info used to containing the board_configuration.json and its associated files",
    fields = {
        "files": "A list of files consisting the board config.",
        "config": "The path to JSON board configuration file.",
        "build_id_dirs": "Directories containing the debug symbols",
    },
)

FuchsiaPostProcessingScriptInfo = provider(
    doc = "A post processing script info used to containing the post processing script related artifacts",
    fields = {
        "post_processing_script_path": "Path to post processing script",
        "post_processing_script_args": "Post processing script arguments",
        "post_processing_script_inputs": "Dictionary for artifacts used by post processing script.",
    },
)

FuchsiaSizeCheckerInfo = provider(
    doc = """Size reports created by size checker tool.""",
    fields = {
        "size_budgets": "size_budgets.json file",
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

FuchsiaPlatformArtifactsInfo = provider(
    doc = """A set of platform artifacts used by product assembly.""",
    fields = {
        "root": "The root directory for these artifacts",
        "files": "All files contained in the bundle",
    },
)

FuchsiaLegacyBundleInfo = provider(
    doc = """A legacy AIB used by product assembly.""",
    fields = {
        "root": "The root directory for these artifacts",
        "files": "All files contained in the bundle",
    },
)

FuchsiaPartitionsConfigInfo = provider(
    doc = "The partitions configuration files and manifest.",
    fields = {
        "files": "A list of files consisting the partitions config.",
        "config": "The partitions config json manifest.",
    },
)

FuchsiaProductImageInfo = provider(
    doc = "Info needed to pave a Fuchsia image",
    fields = {
        "images_out": "images out directory",
        "product_assembly_out": "product assembly out directory",
        "platform_aibs": "platform aibs file listing path to platform AIBS",
        "build_type": "The build type of the product",
        "build_id_dirs": "Directories containing the debug symbols",
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
        "build_type": "The build type of the product",
        "build_id_dirs": "Directories containing the debug symbols",
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
        "static_packages": "Set of base and cache packages expected in the fvm",
        "structured_config_policy": "File describing the policy of structured config",
        "pre_signing_policy": "File describing the policy of checks required before signing",
        "pre_signing_goldens_dir": "Path to directory containing golden files for pre-signing checks",
        "pre_signing_goldens": "List of golden files for pre-signing checks to be used as build inputs",
    },
)

FuchsiaRepositoryKeysInfo = provider(
    doc = "A directory containing Fuchsia TUF repository keys.",
    fields = {"dir": "Path to the directory"},
)

FuchsiaOmahaOtaConfigInfo = provider(
    doc = "OTA configuration data for products that use the Omaha client.",
    fields = {
        "channels": "The omaha channel configuration data.",
        "tuf_repositories": "A dict of TUF repository configurations, by hostname.",
    },
)

FuchsiaAssemblyDeveloperOverridesListInfo = provider(
    doc = "Map a target pattern to a fuchsia_assembly_developer_overrides() target label.",
    fields = {
        "maps": "A list of (pattern_string, info) pairs, where pattern_string is a label pattern string, and info is a corresponding FuchsiaAssemblyDeveloperOverridesInfo",
    },
)

FuchsiaAssemblyDeveloperOverridesInfo = provider(
    doc = "Info describing developer overrides for assembly.",
    fields = {
        "manifest": "A File value pointing to an input JSON file passed to ffx's --developer-overrides option.",
        "inputs": "A File list pointing to extra inputs listed in the manifest that will be used by ffx when applying the overrides.",
    },
)
