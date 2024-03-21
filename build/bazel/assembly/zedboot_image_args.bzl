# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Arguments for creating Zedboot recovery images. Intentionally kept in
fuchsia.git so they can be changed together with their GN counterparts.

These are extracted into a loadable .bzl file for sharing between repos.
"""

load("@fuchsia_sdk//fuchsia:assembly.bzl", "PACKAGE_MODE")

ZEDBOOT_IMAGE_ARGS = {
    "package_mode": PACKAGE_MODE.BOOTFS,
    "legacy_bundle": "//build/bazel/assembly/assembly_input_bundles:legacy_zedboot",
    "platform_artifacts": "//build/bazel/assembly/assembly_input_bundles:platform_bringup",
    "product_config": "//build/bazel/assembly/product_configurations:zedboot",
}
