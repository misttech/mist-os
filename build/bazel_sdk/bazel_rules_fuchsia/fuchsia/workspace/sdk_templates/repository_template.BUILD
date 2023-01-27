# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

# This BUILD file is mapped into a downloaded Fuchsia SDK.
#
# This is referenced by //fuchsia/private/fuchsia_sdk_repository.bzl, see
# the `fuchsia_sdk_repository` rule for more information.
load("@rules_fuchsia//fuchsia:defs.bzl", "fuchsia_debug_symbols", "fuchsia_package_resource", "fuchsia_toolchain_info")
load("api_version.bzl", "DEFAULT_FIDL_TARGET_API", "DEFAULT_TARGET_API")
load("@rules_fuchsia//fuchsia/workspace:sdk_host_tool.bzl", "sdk_host_tool")

# Export all files as individual targets.
exports_files(glob(["**/*"]))

# A single target that includes all files in the SDK.
filegroup(
    name = "all_files",
    srcs = glob(["**/*"]),
    visibility = ["//visibility:public"],
)

constraint_value(
    name = "fuchsia_toolchain_version_sdk",
    constraint_setting = "@rules_fuchsia//fuchsia/constraints:version",
    visibility = ["//visibility:public"],
)

# Toolchain has additional tools if the experimental SDK is included.
# This is a temporary implementation for the experimental SDK and not
# published with the core SDK.
constraint_setting(
    name = "sdk_setup",
    default_constraint_value = ":{{has_experimental}}",
    visibility = ["//visibility:public"],
)

constraint_value(
    name = "has_experimental",
    constraint_setting = "sdk_setup",
    visibility = ["//visibility:public"],
)

constraint_value(
    name = "no_experimental",
    constraint_setting = "sdk_setup",
    visibility = ["//visibility:public"],
)

platform(
    name = "fuchsia_platform_sdk",
    constraint_values = [":fuchsia_toolchain_version_sdk"],
    visibility = ["//visibility:public"],
)

# TODO(fxbug.dev/108014): Figure out a more precise way to express toolchain
# runfile dependencies.
filegroup(
    name = "fuchsia_toolchain_files",
    srcs = [
        "meta/manifest.json",
        "//tools:aemu_internal_x64",
        "//tools:x64/ffx",
        "//tools:x64/fssh",
        "//tools:x64/fvm",
        "//tools:x64/pm",
        "//tools:x64/zbi",
    ],
)

fuchsia_debug_symbols(
    name = "debug_symbols",
    build_dir = "//:BUILD.bazel",
    build_id_dirs = ["//:.build-id"],
    visibility = ["//visibility:public"],
)

fuchsia_toolchain_info(
    name = "fuchsia_toolchain_info",
    bindc = select({
        ":has_experimental": "//tools:x64/bindc",
        ":no_experimental": None,
    }),
    blobfs = "//tools:x64/blobfs_do_not_depend",
    bootserver = "//tools:x64/bootserver",
    cmc = "//tools:x64/cmc",
    cmc_manifest = "//tools:x64/cmc-meta.json",
    cmc_includes = select({
        "@platforms//os:fuchsia": "//:cmc_includes",
        "//conditions:default": None,
    }),
    default_fidl_target_api = DEFAULT_FIDL_TARGET_API,
    default_target_api = DEFAULT_TARGET_API,
    exec_cpu = "x64",
    far = "//tools:x64/far",
    ffx = "//tools:x64/ffx",
    fidlc = "//tools:x64/fidlc",
    fidlgen_cpp = "//tools:x64/fidlgen_cpp",
    fidlgen_hlcpp = "//tools:x64/fidlgen",  # (TODO: rename to fidlgen_hlcpp once the Core SDK renames it)
    fssh = "//tools:x64/fssh",
    fvm = "//tools:x64/fvm",
    merkleroot = "//tools:x64/merkleroot",
    pm = "//tools:x64/pm",
    runfiles = ":fuchsia_toolchain_files",
    sdk_id = "{{SDK_ID}}",
    zbi = "//tools:x64/zbi",
)

toolchain(
    name = "fuchsia_toolchain_sdk",
    toolchain = ":fuchsia_toolchain_info",
    toolchain_type = "@rules_fuchsia//fuchsia:toolchain",
)

# The following rules expose the sdk tools that are put in the fuchsia_toolchain
# so uses can run them directly. These targets can be invoked by calling
# `bazel run @fuchsia_sdk//:<tool_name> -- args`
# Additionally, the run_sdk_tool.sh script knows how to invoke these tools
# and can be used as a drop in replacement for the tool.
sdk_host_tool(name = "ffx")

sdk_host_tool(name = "fssh")

sdk_host_tool(name = "cmc")

sdk_host_tool(name = "pm")

sdk_host_tool(name = "far")
