# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia assembly input bundle names."""

# The names of all of the platform's 'testonly=false' Assembly Input Bundles
PLATFORM_AIB_NAMES = [
    "common_bringup",
    "common_minimal",
    "common_minimal_userdebug",
    "emulator_support",
    "omaha-client",
    "kernel_args_user",
    "kernel_args_userdebug",
    "archivist-no-detect-service",
    "archivist-no-service",
    "archivist-bringup",
    "archivist-minimal",
    "wlan_base",
    "wlan_phy",
    "wlan_legacy_privacy_support",
    "wlan_contemporary_privacy_only_support",
    "wlan_fullmac_support",
    "wlan_softmac_support",
]

# The names of all of the platform's Assembly Input Bundles.
ENG_PLATFORM_AIB_NAMES = PLATFORM_AIB_NAMES + [
    "common_minimal_eng",
    "system-update-checker",
    "kernel_args_eng",
]
