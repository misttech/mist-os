# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""
Arguments for defining minimal products.

These are extracted into a loadable .bzl file for sharing between repos.
"""

load("@fuchsia_build_info//:args.bzl", "authorized_ssh_keys_label")
load(
    "@rules_fuchsia//fuchsia:assembly.bzl",
    "BUILD_TYPES",
    "INPUT_DEVICE_TYPE",
)
load("//build/info:info.bzl", "DEFAULT_PRODUCT_BUILD_INFO")

MINIMAL_PLATFORM_BASE = {
    "build_type": BUILD_TYPES.ENG,
    "development_support": {
        "authorized_ssh_keys_path": "LABEL(%s)" % authorized_ssh_keys_label,
    } if authorized_ssh_keys_label else {},
    "fonts": {
        "enabled": False,
    },
    "ui": {
        "supported_input_devices": [
            INPUT_DEVICE_TYPE.BUTTON,
            INPUT_DEVICE_TYPE.TOUCHSCREEN,
        ],
    },
    "power": {
        "enable_non_hermetic_testing": True,
    },
    "storage": {
        "storage_host_enabled": True,
    },
}

MINIMAL_PRODUCT_BASE = {
    "build_info": DEFAULT_PRODUCT_BUILD_INFO | {
        "name": "minimal",
    },
}
