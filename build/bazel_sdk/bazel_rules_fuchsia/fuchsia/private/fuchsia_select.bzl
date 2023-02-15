# Copyright 2021 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Helpers for using select() within Fuchsia rules."""

_ERROR = """
****************************************************************************
ERROR: You have to specify a config in order to build Fuchsia.
For example:
    bazel build --config=fuchsia_x64 ...
    bazel build --config=fuchsia_arm64 ...
****************************************************************************
"""

def fuchsia_select(configs):
    """select() variant that prints a meaningful error.

    Args:
        configs: A dict of config name-value pairs.

    Returns:
        Selected attribute value depending on the config.
    """
    return select(configs, no_match_error = _ERROR)

# TODO(jayzhuang): Remove this function when downstream usages are removed.
def if_fuchsia(value, if_not = [], _unused_rules_fuchsia_root = "@rules_fuchsia"):
    """Selects `value` if targeting Fuchsia. Otherwise selects `if_not`.

    Args:
        value: The value to select for if targeting Fuchsia.
        if_not: The value to select for if not targeting Fuchsia.
        _unused_rules_fuchsia_root: The root label for rules_fuchsia (this repo).
    Returns:
        Selected value depending on whether we're targeting Fuchsia.
    """
    return fuchsia_select({
        "@platforms//os:fuchsia": value,
        "//conditions:default": if_not,
    })

def fuchsia_only_target():
    return if_fuchsia(
        [],
        ["@platforms//:incompatible"],
    )
