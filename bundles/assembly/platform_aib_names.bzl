# Copyright 2022 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Fuchsia assembly input bundle names."""

load("@fuchsia_icu_config//:constants.bzl", "icu_flavors")

# These are the user-buildtype-safe platform AIBs that are used by bootstrap
# feature-set-level assemblies.  This is a subset of the overall platform AIBs
# so that these systems (e.g. bringup) don't need to build the entire platform.
BOOTSTRAP_USER_PLATFORM_AIB_NAMES = [
    "zircon",
    "bootstrap",
    "console_user",
    "driver_framework",
    "emulator_support",
    "fshost_common",
    "fshost_fxfs",
    "fshost_fvm",
    "fshost_fvm_f2fs",
    "fshost_fvm_fxfs",
    "fshost_fvm_minfs",
    "fshost_fvm_minfs_migration",
    "fshost_storage",
    "paravirtualization_support",
    "kernel_anonymous_memory_compression",
    "kernel_anonymous_memory_compression_eager_lru",
    "kernel_contiguous_physical_pages",
    "kernel_args_user",
    "kernel_debug_broker_user",
    "legacy_power_framework",
    "live_usb",
    "resources",
    "virtcon",
    "virtcon_disable",
]

# These are the userdebug platform AIBs that are used by bootstrap
# feature-set-level assemblies.  This is a subset of the overall platform AIBs
# so that these systems (e.g. bringup) don't need to build the entire platform.
BOOTSTRAP_USERDEBUG_PLATFORM_AIB_NAMES = [
    "bootstrap_userdebug",
    "clock_development_tools",
    "console_userdebug",
    "kernel_args_eng",
    "kernel_args_userdebug",
    "kernel_debug_broker_userdebug",
    "netsvc",
    "ptysvc",
]

# These are the eng-buildtype-safe platform AIBs that are used by bootstrap
# feature-set-level assemblies.  This is a subset of the overall platform AIBs
# so that these systems (e.g. bringup) don't need to build the entire platform.
BOOTSTRAP_ENG_PLATFORM_AIB_NAMES = [
    "bootstrap_eng",
    "kernel_pmm_checker_enabled",
]

# This is the combined set of valid AIBs for "bringup" builds (which are the
# ones that need to use the bootstrap feature-set-level)
BRINGUP_PLATFORM_AIB_NAMES = BOOTSTRAP_USER_PLATFORM_AIB_NAMES + BOOTSTRAP_USERDEBUG_PLATFORM_AIB_NAMES + BOOTSTRAP_ENG_PLATFORM_AIB_NAMES

# The names of all of the platform's 'testonly=false' Assembly Input Bundles
# Please keep sorted, it makes merge conflicts less likely vs adding to the
# end of the list.
USER_PLATFORM_AIB_NAMES = BOOTSTRAP_USER_PLATFORM_AIB_NAMES + [
    # keep-sorted: start
    "audio_core",
    "audio_core_routing",
    "audio_core_use_adc_device",
    "audio_device_registry",
    "battery_manager",
    "bluetooth_a2dp",
    "bluetooth_avrcp",
    "bluetooth_core",
    "bluetooth_hfp_ag",
    "bluetooth_snoop_eager",
    "bluetooth_snoop_lazy",
    "brightness_manager",
    "camera",
    "common_standard",
    "core_realm",
    "core_realm_networking",
    "core_realm_user_and_userdebug",
    "diagnostics_triage_detect_mali",
    "element_manager",
    "factory_data",
    "factory_reset_trigger",
    "fan",
    "feedback_large_disk",
    "feedback_low_memory_product_config",
    "feedback_remote_device_id_provider",
    "feedback_user_config",
    "feedback_userdebug_config",
    "fonts",
    "fonts_hermetic",
    "input_group_one",
    "input_group_two",
    "intl_services.icu_default_{}".format(icu_flavors.default_git_commit),
    "intl_services.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "intl_services_small.icu_default_{}".format(icu_flavors.default_git_commit),
    "intl_services_small.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "intl_services_small_with_timezone.icu_default_{}".format(icu_flavors.default_git_commit),
    "intl_services_small_with_timezone.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "netstack2",
    "netstack3",
    "netstack3_packages",
    "netstack3_packages_gub",
    "netstack_migration",
    "netstack_migration_packages",
    "netstack_migration_packages_gub",
    "network_realm",
    "network_realm_packages",
    "network_realm_packages_gub",
    "network_tun",
    "networking_basic",
    "networking_basic_packages",
    "networking_basic_packages_gub",
    "networking_with_virtualization",
    "no_update_checker",
    "omaha_client",
    "power_metrics_recorder",
    "radar_proxy_without_injector",
    "sensors_framework",
    "session_manager",
    "setui",
    "setui.icu_default_{}".format(icu_flavors.default_git_commit),
    "setui.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "setui_with_camera",
    "setui_with_camera.icu_default_{}".format(icu_flavors.default_git_commit),
    "setui_with_camera.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "soundplayer",
    "standard_user",
    "starnix_support",
    "system_update_configurator",
    "thread_lowpan",
    "ui",
    "ui_package_eng_userdebug_with_synthetic_device_support",
    "ui_package_user_and_userdebug",
    "ui_user_and_userdebug",
    "ui_user_and_userdebug.icu_default_{}".format(icu_flavors.default_git_commit),
    "ui_user_and_userdebug.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "virtualization_support",
    "wlan_contemporary_privacy_only_support",
    "wlan_fullmac_support",
    "wlan_legacy_privacy_support",
    "wlan_policy",
    "wlan_softmac_support",
    "wlan_wlanix",
    "zoneinfo",
    # keep-sorted: end
]

USERDEBUG_PLATFORM_AIB_NAMES = BOOTSTRAP_USERDEBUG_PLATFORM_AIB_NAMES + USER_PLATFORM_AIB_NAMES + [
    "adb_support",
    "core_realm_development_access",
    "core_realm_development_access_rcs_no_usb",
    "core_realm_development_access_rcs_usb",
    "omaha_client_empty_eager_config",
    "power_framework",
    "standard_userdebug",
    "standard_userdebug_and_eng",
    "mdns_fuchsia_device_wired_service",
    "radar_proxy_with_injector",
    "sl4f",
    "wlan_development",
]

# The names of all of the platform's Assembly Input Bundles.
ENG_PLATFORM_AIB_NAMES = BOOTSTRAP_ENG_PLATFORM_AIB_NAMES + USERDEBUG_PLATFORM_AIB_NAMES + [
    "audio_development_support",
    "bluetooth_pandora",
    "core_realm_eng",
    "example_assembly_bundle",
    "full_package_drivers",
    "standard_eng",
    "networking_test_collection",
    "pkgfs_disable_executability_restrictions",
    "sensors_playback",
    "system_update_checker",
    "testing_support",
    "ui_eng",
    "ui_eng.icu_default_{}".format(icu_flavors.default_git_commit),
    "ui_eng.icu_latest_{}".format(icu_flavors.latest_git_commit),
    "ui_package_eng",
    "video_development_support",
]
