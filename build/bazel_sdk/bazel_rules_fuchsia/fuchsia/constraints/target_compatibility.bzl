# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Tools to check the comatibility of fuchsia targets"""

load("@platforms//host:constraints.bzl", "HOST_CONSTRAINTS")

_SUPPORTED_HOST_CPU_PLATFORMS = [
    "@platforms//cpu:x86_64",
]
_SUPPORTED_HOST_OS_PLATFORMS = [
    "@platforms//os:linux",
]

def _define_host_can_build_fuchsia_flag_impl(ctx):
    valid_host_cpu = any([cpu in HOST_CONSTRAINTS for cpu in _SUPPORTED_HOST_CPU_PLATFORMS])
    valid_host_os = any([os in HOST_CONSTRAINTS for os in _SUPPORTED_HOST_OS_PLATFORMS])

    if valid_host_cpu and valid_host_os:
        valid_host = "yes"
    else:
        valid_host = "no"

    return [
        config_common.FeatureFlagInfo(value = valid_host),
    ]

define_host_can_build_fuchsia_flag = rule(
    doc = """A build value that can be used as a constraint.

    This build config allows us to select on whether the build is executing on a
    supported host platform. If we do not do this then glob builds will fail
    due on host machines we do not support due to not having a valid toolchain.
    """,
    implementation = _define_host_can_build_fuchsia_flag_impl,
)

# A select() statement that returns a `target_compatible_with` value
# that will be [ "@platforms//os:fuchsia" ] if the current host system
# is supported by the Fuchsia SDK cross-toolchain, or [ "@platforms//:incompatible" ] otherwise.
#
# Example usage:
#
#  ```
#  cc_library(
#     name = "foo",
#     ...
#     target_compatible_with = FUCHSIA_TARGET_COMPATIBILITY_SELECT,
#  )
#  ```
#
FUCHSIA_TARGET_COMPATIBILITY_SELECT = select({
    "@fuchsia_sdk//fuchsia/constraints:can_host_build_fuchsia": ["@platforms//os:fuchsia"],
    "//conditions:default": ["@platforms//:incompatible"],
})

# A dictionary that can be expanded in rule definitions
# to indicate that a target is compatible with Fuchsia, if
# the Bazel host system is supported by the Fuchsia SDK cross-toolchain.
#
# Example usage:
#
# ```
# cc_library(
#    name = "foo",
#    ...
#    **FUCHSIA_TARGET_COMPATIBILITY_KWARGS,
# )
# ```
#
FUCHSIA_TARGET_COMPATIBILTY_KWARGS = {
    "target_compatible_with": FUCHSIA_TARGET_COMPATIBILITY_SELECT,
}
