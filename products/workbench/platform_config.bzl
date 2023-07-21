# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Workbench platform config

This is a platform configuration for workbench, which can be used by other
products. This platform configuration is meant to feed into Fuchsia's product
assembly process.
"""

load(
    "@fuchsia_sdk//fuchsia:assembly.bzl",
    "BUILD_TYPES",
)

workbench_platform_config = {
    "build_type": BUILD_TYPES.ENG,
    "battery": {
        "enabled": True,
    },
    "input": {
        "supported_input_devices": [
            "keyboard",
            "mouse",
            "touchscreen",
        ],
    },
    "media": {
        "audio_device_registry_enabled": True,
    },
    "starnix": {
        "enabled": True,
        "enable_android_support": True,
    },
    "storage": {
        "configure_fshost": True,
        "live_usb_enabled": False,
    },
    "session": {
        "enabled": True,
    },
    "ui": {
        "enabled": True,
    },
}
