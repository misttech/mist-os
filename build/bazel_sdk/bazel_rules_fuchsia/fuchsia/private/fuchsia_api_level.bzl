# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

""" Defines utilities for working with fuchsia api levels. """

load("//:api_version.bzl", "VALID_TARGET_APIS")

# We define the provider in this file because it is a private implementation
# detail in this file. It is only made public so that it can be used in tests.
FuchsiaAPILevelInfo = provider(
    "Specifies what api to use while building",
    fields = {
        "level": "The API level",
    },
)

# The name for the API level target.
FUCHSIA_API_LEVEL_TARGET_NAME = "//fuchsia:fuchsia_api_level"

# Rules that require the fuchsia api level should depend on this attribute set.
# They can then use the helper functions in this file to get the flags needed.
FUCHSIA_API_LEVEL_ATTRS = {
    "_fuchsia_api_level": attr.label(
        default = FUCHSIA_API_LEVEL_TARGET_NAME,
    ),
}

def get_fuchsia_api_level(ctx):
    """ Returns the raw api level to use for building.

    This method can return any of the valid API levels including the empty string.
"""
    return ctx.attr._fuchsia_api_level[FuchsiaAPILevelInfo].level

def _valid_api_levels(ctx):
    if getattr(ctx.attr, "valid_api_levels_for_test", None):
        levels = ctx.attr.valid_api_levels_for_test
    else:
        levels = [entry.api_level for entry in VALID_TARGET_APIS]

    # The unset level is still valid since it can indicate that the user did
    # not set the value. If we don't do this then we have no way if the
    # user passed the flag along or not.
    return levels + [""]

def _fuchsia_api_level_impl(ctx):
    raw_level = ctx.build_setting_value
    if raw_level not in _valid_api_levels(ctx):
        fail("ERROR: {} is not a valid API level. API level should be one of {}".format(
            raw_level,
            _valid_api_levels(ctx),
        ))

    return FuchsiaAPILevelInfo(
        level = raw_level,
    )

fuchsia_api_level = rule(
    doc = """A build configuration value containing the fuchsia api level

    The fuchsia_api_level is a build configuration value that can be set from
    the command line. This lets users define how they what api level they want
    to use outside of a BUILD.bazel file.
    """,
    implementation = _fuchsia_api_level_impl,
    build_setting = config.string(flag = True),
    attrs = {
        "valid_api_levels_for_test": attr.string_list(
            doc = """A set of levels to use for testing.

            This attr should not be used outside of a testing environment.""",
            default = [],
        ),
    },
)
