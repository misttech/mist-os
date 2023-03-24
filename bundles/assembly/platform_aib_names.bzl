# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia assembly input bundle names."""

load("@fuchsia_icu_config//:constants.bzl", "icu_flavors")

# The names of all of the platform's 'testonly=false' Assembly Input Bundles
PLATFORM_AIB_NAMES = [
    "bootstrap",
    "core_realm",
    "core_realm_networking",
    "core_realm_user_and_userdebug",
    "common_minimal",
    "common_minimal_userdebug",
    "empty_live_usb",
    "emulator_support",
    "fshost_fxfs",
    "kernel_args_user",
    "kernel_args_userdebug",
    "live_usb",
    "netstack2",
    "netstack3",
    "omaha_client",
    "starnix_support",
    "wlan_base",
    "wlan_contemporary_privacy_only_support",
    "wlan_fullmac_support",
    "wlan_legacy_privacy_support",
    "wlan_softmac_support",
    "virtcon",
    "virtualization_support",
    "intl_services.icu_default_{}".format(icu_flavors.default_git_commit),
    "intl_services.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "intl_services.icu_stable_{}".format(icu_flavors.stable_git_commit),
]

# The names of all of the platform's Assembly Input Bundles.
ENG_PLATFORM_AIB_NAMES = PLATFORM_AIB_NAMES + [
    "core_realm_eng",
    "common_minimal_eng",
    "system_update_checker",
    "kernel_args_eng",
    "example_assembly_bundle",
]
