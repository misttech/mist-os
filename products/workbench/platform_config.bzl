# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Workbench platform config

This is a platform configuration for workbench, which can be used by other
products. This platform configuration is meant to feed into Fuchsia's product
assembly process.
"""

load("@fuchsia_build_info//:args.bzl", "delegated_network_provisioning")
load(
    "@fuchsia_sdk//fuchsia:assembly.bzl",
    "BUILD_TYPES",
)

workbench_platform_config = {
    "build_type": BUILD_TYPES.ENG,
    "battery": {
        "enabled": True,
    },
    "connectivity": {
        "network": {
            "networking": "standard",
            "netcfg_config_path": "LABEL(//src/connectivity/policy/netcfg/config:%s.json)" % ("delegated_network_provisioning" if delegated_network_provisioning else "netcfg_default"),
            "include_tun": True,
            "netstack_version": "netstack3",
        },
    },
    "development_support": {
        "tools": {
            "connectivity": {
                "enable_networking": True,
                "enable_wlan": True,
                "enable_thread": True,
            },
        },
    },
    "timekeeper": {
        "first_sampling_delay_sec": 86400,
        "back_off_time_between_pull_samples_sec": 86400,
    },
    "media": {
        "audio": "partial_stack",
    },
    "starnix": {
        "enabled": True,
        "enable_android_support": True,
    },
    "usb": {
        "peripheral": {
            "functions": [
                "cdc",
                "adb",
            ],
        },
    },
    "session": {
        "enabled": True,
    },
    "ui": {
        "enabled": True,
        "supported_input_devices": [
            "button",
            "keyboard",
            "mouse",
            "touchscreen",
        ],
        "with_synthetic_device_support": True,
    },
    "power": {
        "suspend_enabled": True,
    },
    "kernel": {
        "oom_behavior": {
            "reboot": {
                "timeout": "low",
            },
        },
    },
}
