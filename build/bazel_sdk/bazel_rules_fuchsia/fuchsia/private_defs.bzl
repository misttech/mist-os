# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Private definitions for Fuchsia rules that are only exposed for the SDK test suite.

This is necessary to run the SDK test suite against the in-tree @_get_fuchsia_sdk
repository, which depends on @rules_fuchsia, and does not have
@fuchsia_sdk//fuchsia/private:* files or targets.

See https://fxbug.dev/383536158.

Nothing should use this except the Bazel SDK test suite at //build/bazel_sdk/tests.

Once all third-party workspaces have migrated to use @rules_fuchsia, the test
suite will be able to load @rules_fuchsia//fuchsia/private:* files directly
instead, and this .bzl will be removed.
"""

load(
    "//fuchsia/private:fuchsia_api_level.bzl",
    _FuchsiaAPILevelInfo = "FuchsiaAPILevelInfo",
    _fuchsia_api_level = "fuchsia_api_level",
    _get_fuchsia_api_level = "get_fuchsia_api_level",
)
load(
    "//fuchsia/private:fuchsia_debug_symbols.bzl",
    _FUCHSIA_DEBUG_SYMBOLS_ATTRS = "FUCHSIA_DEBUG_SYMBOLS_ATTRS",
    _fuchsia_collect_all_debug_symbols_infos_aspect = "fuchsia_collect_all_debug_symbols_infos_aspect",
    _fuchsia_debug_symbols = "fuchsia_debug_symbols",
    _strip_resources = "strip_resources",
    _transform_collected_debug_symbols_infos = "transform_collected_debug_symbols_infos",
)
load("//fuchsia/private:fuchsia_transition.bzl", _fuchsia_transition = "fuchsia_transition")
load(
    "//fuchsia/private:providers.bzl",
    _FuchsiaCollectedPackageResourcesInfo = "FuchsiaCollectedPackageResourcesInfo",
    _FuchsiaComponentInfo = "FuchsiaComponentInfo",
    _FuchsiaComponentManifestInfo = "FuchsiaComponentManifestInfo",
    _FuchsiaDebugSymbolInfo = "FuchsiaDebugSymbolInfo",
    _FuchsiaPackageInfo = "FuchsiaPackageInfo",
    _FuchsiaPackageResourcesInfo = "FuchsiaPackageResourcesInfo",
)
load("//fuchsia/private:utils.bzl", _append_suffix_to_label = "append_suffix_to_label")
load(
    "//fuchsia/private/assembly:providers.bzl",
    _FuchsiaBoardConfigInfo = "FuchsiaBoardConfigInfo",
    _FuchsiaBoardInputBundleInfo = "FuchsiaBoardInputBundleInfo",
    _FuchsiaPartitionsConfigInfo = "FuchsiaPartitionsConfigInfo",
    _FuchsiaProductConfigInfo = "FuchsiaProductConfigInfo",
    _FuchsiaVirtualDeviceInfo = "FuchsiaVirtualDeviceInfo",
)
load(
    "//fuchsia/private:fuchsia_toolchains.bzl",
    _FUCHSIA_TOOLCHAIN_DEFINITION = "FUCHSIA_TOOLCHAIN_DEFINITION",
    _get_fuchsia_sdk_toolchain = "get_fuchsia_sdk_toolchain",
)

# assembly/providers.bzl
FuchsiaBoardConfigInfo = _FuchsiaBoardConfigInfo
FuchsiaBoardInputBundleInfo = _FuchsiaBoardInputBundleInfo
FuchsiaPartitionsConfigInfo = _FuchsiaPartitionsConfigInfo
FuchsiaVirtualDeviceInfo = _FuchsiaVirtualDeviceInfo

# fuchsia_debug_symbols.bzl
FUCHSIA_DEBUG_SYMBOLS_ATTRS = _FUCHSIA_DEBUG_SYMBOLS_ATTRS
fuchsia_collect_all_debug_symbols_infos_aspect = _fuchsia_collect_all_debug_symbols_infos_aspect
fuchsia_debug_symbols = _fuchsia_debug_symbols
strip_resources = _strip_resources
transform_collected_debug_symbols_infos = _transform_collected_debug_symbols_infos

# fuchsia_api_level.bzl
FuchsiaAPILevelInfo = _FuchsiaAPILevelInfo
fuchsia_api_level = _fuchsia_api_level
get_fuchsia_api_level = _get_fuchsia_api_level

# fuchsia_transition.bzl
fuchsia_transition = _fuchsia_transition

# fuchsia_toolchains.bzl
FUCHSIA_TOOLCHAIN_DEFINITION = _FUCHSIA_TOOLCHAIN_DEFINITION
get_fuchsia_sdk_toolchain = _get_fuchsia_sdk_toolchain

# providers.bzl
FuchsiaCollectedPackageResourcesInfo = _FuchsiaCollectedPackageResourcesInfo
FuchsiaComponentInfo = _FuchsiaComponentInfo
FuchsiaComponentManifestInfo = _FuchsiaComponentManifestInfo
FuchsiaDebugSymbolInfo = _FuchsiaDebugSymbolInfo
FuchsiaPackageInfo = _FuchsiaPackageInfo
FuchsiaPackageResourcesInfo = _FuchsiaPackageResourcesInfo
FuchsiaProductConfigInfo = _FuchsiaProductConfigInfo

# utils.bzl
append_suffix_to_label = _append_suffix_to_label
