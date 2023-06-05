# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia assembly input bundle names."""

load("@fuchsia_icu_config//:constants.bzl", "icu_flavors")

# These are the user-buildtype-safe platform AIBs that are used by bootstrap
# feature-set-level assemblies.  This is a subset of the overall platform AIBs
# so that these systems (e.g. bringup) don't need to build the entire platform.
BOOTSTRAP_USER_PLATFORM_AIB_NAMES = [
    "bootstrap",
    "driver_framework_v1",
    "driver_framework_v2",
    "empty_live_usb",
    "emulator_support",
    "kernel_args_user",
    "kernel_args_userdebug",
    "live_usb",
    "virtcon",
]

# These are the eng-buildtype-safe platform AIBs that are used by bootstrap
# feature-set-level assemblies.  This is a subset of the overall platform AIBs
# so that these systems (e.g. bringup) don't need to build the entire platform.
BOOTSTRAP_ENG_PLATFORM_AIB_NAMES = [
    "kernel_args_eng",
]

# This is the combined set of valid AIBs for "bringup" builds (which are the
# ones that need to use the bootstrap feature-set-level
BRINGUP_PLATFORM_AIB_NAMES = BOOTSTRAP_USER_PLATFORM_AIB_NAMES + BOOTSTRAP_ENG_PLATFORM_AIB_NAMES

# The names of all of the platform's 'testonly=false' Assembly Input Bundles
USER_PLATFORM_AIB_NAMES = BOOTSTRAP_USER_PLATFORM_AIB_NAMES + [
    "audio_device_registry",
    "common_minimal",
    "common_minimal_userdebug",
    "core_realm",
    "core_realm_networking",
    "core_realm_user_and_userdebug",
    "fan",
    "fonts",
    "fshost_common",
    "fshost_f2fs",
    "fshost_fxfs",
    "fshost_fxfs_fxblob",
    "fshost_fxfs_minfs_migration",
    "fshost_minfs",
    "fshost_storage",
    "intl_services.icu_default_{}".format(icu_flavors.default_git_commit),
    "intl_services.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "intl_services.icu_stable_{}".format(icu_flavors.stable_git_commit),
    "netstack2",
    "netstack3",
    "omaha_client",
    "radar_proxy_without_injector",
    "session_manager",
    "starnix_support",
    "ui",
    "ui_legacy",
    "ui_legacy_package_user_and_userdebug",
    "ui_package_user_and_userdebug",
    "ui_user_and_userdebug",
    "virtualization_support",
    "wlan_base",
    "wlan_contemporary_privacy_only_support",
    "wlan_fullmac_support",
    "wlan_legacy_privacy_support",
    "wlan_softmac_support",
]

USERDEBUG_PLATFORM_AIB_NAMES = USER_PLATFORM_AIB_NAMES + [
    "core_realm_development_access",
    "core_realm_development_access_rcs_no_usb",
    "core_realm_development_access_rcs_usb",
    "radar_proxy_with_injector",
]

# The names of all of the platform's Assembly Input Bundles.
ENG_PLATFORM_AIB_NAMES = BOOTSTRAP_ENG_PLATFORM_AIB_NAMES + USERDEBUG_PLATFORM_AIB_NAMES + [
    "audio_dev_support",
    "common_minimal_eng",
    "core_realm_eng",
    "example_assembly_bundle",
    "system_update_checker",
    "testing_support",
    "ui_eng",
    "ui_legacy_package_eng",
    "ui_package_eng",
]
