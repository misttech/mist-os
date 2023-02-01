# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia assembly input bundle names."""

# The names of all of the platform's 'testonly=false' Assembly Input Bundles
PLATFORM_AIB_NAMES = [
    "common_bringup",
    "common_minimal",
    "common_minimal_userdebug",
    "empty_live_usb",
    "live_usb",
    "starnix_support",
    "emulator_support",
    "omaha_client",
    "kernel_args_user",
    "kernel_args_userdebug",
    "wlan_base",
    "wlan_legacy_privacy_support",
    "wlan_contemporary_privacy_only_support",
    "wlan_fullmac_support",
    "wlan_softmac_support",
    "virtualization_support",
]

# The names of all of the platform's Assembly Input Bundles.
ENG_PLATFORM_AIB_NAMES = PLATFORM_AIB_NAMES + [
    "common_minimal_eng",
    "system_update_checker",
    "kernel_args_eng",
    "example_assembly_bundle",
]
