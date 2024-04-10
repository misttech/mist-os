# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Public definitions for Fuchsia rules.

Documentation for all rules exported by this file is located at docs/defs.md

See also:
 - @fuchsia_sdk//fuchsia:assembly.bzl
 - @fuchsia_sdk//fuchsia:licenses.bzl
"""

load(
    "//fuchsia/constraints/platforms:supported_platforms.bzl",
    _fuchsia_platforms = "fuchsia_platforms",
)
load(
    "//fuchsia/private:compilation_database.bzl",
    _clangd_compilation_database = "clangd_compilation_database",
)
load(
    "//fuchsia/private:fuchsia_api_level.bzl",
    _FUCHSIA_API_LEVEL_STATUS_SUPPORTED = "FUCHSIA_API_LEVEL_STATUS_SUPPORTED",
    _get_fuchsia_api_levels = "get_fuchsia_api_levels",
)
load(
    "//fuchsia/private:fuchsia_archivist_pipeline_test.bzl",
    _fuchsia_archivist_pipeline_test = "fuchsia_archivist_pipeline_test",
    _fuchsia_archivist_pipeline_test_manifest = "fuchsia_archivist_pipeline_test_manifest",
)
load(
    "//fuchsia/private:fuchsia_bind_cc_library.bzl",
    _fuchsia_bind_cc_library = "fuchsia_bind_cc_library",
)
load(
    "//fuchsia/private:fuchsia_bind_library.bzl",
    _fuchsia_bind_library = "fuchsia_bind_library",
)
load(
    "//fuchsia/private:fuchsia_cc.bzl",
    _fuchsia_cc_binary = "fuchsia_cc_binary",
    _fuchsia_cc_test = "fuchsia_cc_test",
    _fuchsia_wrap_cc_binary = "fuchsia_wrap_cc_binary",
    _fuchsia_wrap_cc_test = "fuchsia_wrap_cc_test",
)
load(
    "//fuchsia/private:fuchsia_cc_driver.bzl",
    _fuchsia_cc_driver = "fuchsia_cc_driver",
)
load(
    "//fuchsia/private:fuchsia_component.bzl",
    _fuchsia_component = "fuchsia_component",
    _fuchsia_driver_component = "fuchsia_driver_component",
    _fuchsia_test_component = "fuchsia_test_component",
)
load(
    "//fuchsia/private:fuchsia_component_manifest.bzl",
    _fuchsia_component_manifest = "fuchsia_component_manifest",
    _fuchsia_component_manifest_shard = "fuchsia_component_manifest_shard",
    _fuchsia_component_manifest_shard_collection = "fuchsia_component_manifest_shard_collection",
)
load(
    "//fuchsia/private:fuchsia_cpu_select.bzl",
    _fuchsia_cpu_filter_dict = "fuchsia_cpu_filter_dict",
    _fuchsia_cpu_select = "fuchsia_cpu_select",
)
load(
    "//fuchsia/private:fuchsia_debug_symbols.bzl",
    _fuchsia_debug_symbols = "fuchsia_debug_symbols",
)
load(
    "//fuchsia/private:fuchsia_devicetree_visitor.bzl",
    _fuchsia_devicetree_visitor = "fuchsia_devicetree_visitor",
)
load(
    "//fuchsia/private:fuchsia_driver_bind_rules.bzl",
    _fuchsia_driver_bind_bytecode = "fuchsia_driver_bind_bytecode",
)
load(
    "//fuchsia/private:fuchsia_driver_tool.bzl",
    _fuchsia_driver_tool = "fuchsia_driver_tool",
)
load(
    "//fuchsia/private:fuchsia_fidl_bind_library.bzl",
    _fuchsia_fidl_bind_library = "fuchsia_fidl_bind_library",
)
load(
    "//fuchsia/private:fuchsia_fidl_library.bzl",
    _fuchsia_fidl_library = "fuchsia_fidl_library",
)
load(
    "//fuchsia/private:fuchsia_package.bzl",
    _fuchsia_package = "fuchsia_package",
    _fuchsia_test_package = "fuchsia_test_package",
    _fuchsia_unittest_package = "fuchsia_unittest_package",
    _get_component_manifests = "get_component_manifests",
    _get_driver_component_manifests = "get_driver_component_manifests",
)
load(
    "//fuchsia/private:fuchsia_package_group.bzl",
    _fuchsia_package_group = "fuchsia_package_group",
)
load(
    "//fuchsia/private:fuchsia_package_resource.bzl",
    _fuchsia_package_resource = "fuchsia_package_resource",
    _fuchsia_package_resource_collection = "fuchsia_package_resource_collection",
    _fuchsia_package_resource_group = "fuchsia_package_resource_group",
)
load(
    "//fuchsia/private:fuchsia_prebuilt_package.bzl",
    _fuchsia_prebuilt_package = "fuchsia_prebuilt_package",
)
load(
    "//fuchsia/private:fuchsia_rust.bzl",
    _fuchsia_wrap_rust_binary = "fuchsia_wrap_rust_binary",
)
load(
    "//fuchsia/private:fuchsia_remote_product_bundle.bzl",
    _fuchsia_remote_product_bundle = "fuchsia_remote_product_bundle",
)
load(
    "//fuchsia/private:fuchsia_select.bzl",
    _fuchsia_select = "fuchsia_select",
)
load(
    "//fuchsia/private:legacy_fuchsia_fidl_cc_library.bzl",
    _fuchsia_fidl_hlcpp_library = "fuchsia_fidl_hlcpp_library",
    _fuchsia_fidl_llcpp_library = "fuchsia_fidl_llcpp_library",
)

# Workspace-dependent rules.
load(
    "//fuchsia/workspace:fuchsia_toolchain_info.bzl",
    _fuchsia_toolchain_info = "fuchsia_toolchain_info",
)

# Rules

fuchsia_bind_library = _fuchsia_bind_library
fuchsia_cc_binary = _fuchsia_cc_binary
fuchsia_cc_driver = _fuchsia_cc_driver
fuchsia_cc_test = _fuchsia_cc_test
fuchsia_wrap_cc_binary = _fuchsia_wrap_cc_binary
fuchsia_wrap_cc_test = _fuchsia_wrap_cc_test
fuchsia_wrap_rust_binary = _fuchsia_wrap_rust_binary
fuchsia_component = _fuchsia_component
fuchsia_test_component = _fuchsia_test_component
fuchsia_component_manifest = _fuchsia_component_manifest
fuchsia_component_manifest_shard = _fuchsia_component_manifest_shard
fuchsia_component_manifest_shard_collection = _fuchsia_component_manifest_shard_collection
fuchsia_debug_symbols = _fuchsia_debug_symbols
fuchsia_driver_component = _fuchsia_driver_component
fuchsia_driver_bind_bytecode = _fuchsia_driver_bind_bytecode
fuchsia_driver_tool = _fuchsia_driver_tool
fuchsia_fidl_library = _fuchsia_fidl_library
fuchsia_fidl_bind_library = _fuchsia_fidl_bind_library
fuchsia_bind_cc_library = _fuchsia_bind_cc_library
fuchsia_fidl_hlcpp_library = _fuchsia_fidl_hlcpp_library
fuchsia_fidl_llcpp_library = _fuchsia_fidl_llcpp_library
fuchsia_archivist_pipeline_test_manifest = _fuchsia_archivist_pipeline_test_manifest
fuchsia_archivist_pipeline_test = _fuchsia_archivist_pipeline_test
fuchsia_package = _fuchsia_package
fuchsia_test_package = _fuchsia_test_package
fuchsia_unittest_package = _fuchsia_unittest_package
fuchsia_prebuilt_package = _fuchsia_prebuilt_package
fuchsia_package_resource = _fuchsia_package_resource
fuchsia_package_resource_collection = _fuchsia_package_resource_collection
fuchsia_package_resource_group = _fuchsia_package_resource_group
fuchsia_package_group = _fuchsia_package_group
fuchsia_remote_product_bundle = _fuchsia_remote_product_bundle
fuchsia_select = _fuchsia_select
fuchsia_cpu_select = _fuchsia_cpu_select
fuchsia_cpu_filter_dict = _fuchsia_cpu_filter_dict
fuchsia_devicetree_visitor = _fuchsia_devicetree_visitor
get_component_manifests = _get_component_manifests
get_fuchsia_api_levels = _get_fuchsia_api_levels
get_driver_component_manifests = _get_driver_component_manifests
clangd_compilation_database = _clangd_compilation_database
fuchsia_toolchain_info = _fuchsia_toolchain_info

# Platform definitions
fuchsia_platforms = _fuchsia_platforms

# Constants
FUCHSIA_API_LEVEL_STATUS_SUPPORTED = _FUCHSIA_API_LEVEL_STATUS_SUPPORTED
