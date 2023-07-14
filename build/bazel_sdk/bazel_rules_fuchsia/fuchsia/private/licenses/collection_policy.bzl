# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Allowlists rules names, attributes, targets for licenses collecting."""

load("common.bzl", "bool_dict", "check_is_target")

# TODO(114260): Evolve policy to handle real-world projects.
ignore_policy = struct(
    # These rule attributes will be ignored:
    rule_attributes = {
        "fuchsia_component_manifest": ["_sdk_coverage_shard"],
        "_build_fuchsia_package": ["_fuchsia_sdk_debug_symbols"],
        "fuchsia_product_bundle": ["update_version_file", "_sdk_manifest"],
    },

    # These rules will be ignored:
    rules = bool_dict([
        "package_repo_path_flag",  # Config flag
        "fuchsia_scrutiny_config",  # Build time verification data.
        "fuchsia_debug_symbols",  # Debug symbols have no separate licenses.
    ]),

    # These targets will be ignored:
    targets = bool_dict([
    ]),

    # Anything withing these workspaces will be ignored:
    workspaces = bool_dict([
        "bazel_tools",
    ]),

    # Anything withing these package will be ignored:
    packages = bool_dict([
        "@fuchsia_sdk//fuchsia/tools",
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
