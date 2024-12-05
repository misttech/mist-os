# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Allowlists rules names, attributes, targets for licenses collecting."""

load("common.bzl", "bool_dict", "check_is_target", "to_package_str")

# TODO(114260): Evolve policy to handle real-world projects.
ignore_policy = struct(
    # These rule attributes will be ignored:
    rule_attributes = {
        "fuchsia_component_manifest": ["_sdk_coverage_shard"],
        "_build_fuchsia_package": ["_fuchsia_sdk_debug_symbols"],
        "_build_fuchsia_product_bundle": ["update_version_file"],
    },

    # These rules will be ignored:
    rules = bool_dict([
        "fuchsia_scrutiny_config",  # Build time verification data.
        "fuchsia_debug_symbols",  # Debug symbols have no separate licenses.
    ]),

    # These targets will be ignored:
    targets = bool_dict([
        str(Label(label))
        for label in [
            "@fuchsia_sdk//:meta/manifest.json",  # SDK metadata, not shipping to clients.
            "@platforms//os:os",  # Constraint
            "@platforms//os:fuchsia",  # Constraint
        ]
    ]),

    # Anything withing these workspaces will be ignored:
    # NOTE: with Bzlmod enabled, these check against canonical repo names, which might not be
    # intended. However, we can't just pass these through `Label()` as some of these repo names
    # are not actually known to `rules_fuchsia` (this module).
    workspaces = bool_dict([
        "bazel_tools",
        "fuchsia_clang",  # TODO(95670): clang bazel defs should provide licenses.
        "fuchsia_sdk",  # TODO(130784): sdk atoms should provide licenses.
        "internal_sdk",  # TODO(130784): sdk atoms should provide licenses.
        "assembly_developer_overrides",  # Local development overrides don't provide licenses.
    ]),

    # Anything withing these package will be ignored:
    packages = bool_dict([
        to_package_str(Label(label))
        for label in [
            "@fuchsia_sdk//fuchsia/tools",
        ]
    ]),
)

def is_3p_target(target):
    """Whether the target is third_party, another workspace (typically 3P) or a prebuilt.

    Args:
        target: The target to check.
    Returns:
        Whether the target is third_party.
    """
    check_is_target(target)
    label = target.label
    if label.workspace_name:
        # Anything in another workspace is typically third_party code.
        return True
    elif "third_party" in label.package:
        return True
    elif "prebuilt" in label.package:
        return True
    else:
        return False
