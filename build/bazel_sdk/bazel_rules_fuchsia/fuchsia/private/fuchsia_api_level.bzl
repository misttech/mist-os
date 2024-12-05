# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

""" Defines utilities for working with fuchsia api levels. """

# NOTE: INTERNAL_ONLY_VALID_TARGET_APIS is part of the generated content of @fuchsia_sdk
# and does not exist in @rules_fuchsia.
load("@fuchsia_sdk//:api_version.bzl", "INTERNAL_ONLY_VALID_TARGET_APIS")

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

FUCHSIA_API_LEVEL_STATUS_SUPPORTED = "supported"
FUCHSIA_API_LEVEL_STATUS_UNSUPPORTED = "unsupported"
FUCHSIA_API_LEVEL_STATUS_IN_DEVELOPMENT = "in-development"

def get_fuchsia_api_levels():
    """ Returns the list of API levels in this SDK.

    Values are returned as a struct with the following fields:
    struct(
        abi_revision = "0xED74D73009C2B4E3",
        api_level = "10",
        as_u32 = 10,
        status = "unsupported"
    )

    `as_u32` is interesting in the case of special API levels like `HEAD`.
    clang only wants to be passed API levels in their numeric form.

    The status is not an API to be relied on but the STATUS_* constants can be
    used.
    """
    return INTERNAL_ONLY_VALID_TARGET_APIS

def get_fuchsia_api_level(ctx):
    """ Returns the raw api level to use for building.

    This method can return any of the valid API levels including the empty string.
"""
    return ctx.attr._fuchsia_api_level[FuchsiaAPILevelInfo].level

def fail_missing_api_level(name):
    fail("'{}' does not have a valid API level set. Valid API levels are {}".format(name, [lvl.api_level for lvl in get_fuchsia_api_levels()]))

def _valid_api_levels(ctx):
    levels = [entry.api_level for entry in get_fuchsia_api_levels()]

    # The unset level is still valid since it can indicate that the user did
    # not set the value. If we don't do this then we have no way of knowing if the
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
)

def _verify_cc_head_api_level_impl(ctx):
    f = ctx.actions.declare_file(ctx.label.name + "__verify_head_api_level.cc")
    ctx.actions.write(f, content = "")

    if get_fuchsia_api_level(ctx) != "HEAD":
        fail("You are trying to use an unstable API in a stable package.\n" +
             "You must target \"HEAD\" in order to use this library: " + ctx.attr.library_name)

    return DefaultInfo(
        files = depset([f]),
    )

verify_cc_head_api_level = rule(
    implementation = _verify_cc_head_api_level_impl,
    doc = """Forces an unstable cc_library to only be used if targeting HEAD

    This rule should only be used by the generated cc_library rules for sdk
    elements. It will create an empty c++ file which can be added to the srcs
    of the cc_library. The check will look at the Fuchsia API level and fail if
    it is not "HEAD".
    """,
    attrs = {
        "library_name": attr.string(mandatory = True),
        "_fuchsia_api_level": attr.label(
            # We have to explicitly depend on the @fuchsia_sdk target because
            # this is used from the internal_sdk as well.
            default = "@fuchsia_sdk//fuchsia:fuchsia_api_level",
        ),
    },
)
