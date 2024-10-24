# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Arguments for creating Zedboot recovery images. Intentionally kept in
fuchsia.git so they can be changed together with their GN counterparts.

These are extracted into a loadable .bzl file for sharing between repos.
"""

ZEDBOOT_IMAGE_ARGS = {
    "legacy_bundle": "//build/bazel/assembly/assembly_input_bundles:legacy_zedboot",
    "platform_artifacts": "//build/bazel/assembly/assembly_input_bundles:platform_bringup",
}
