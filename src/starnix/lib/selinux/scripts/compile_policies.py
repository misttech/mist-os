#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
#
################################################################################
# WARNING: This script is currently not suitable for use in hermetic builds.   #
# It depends on the SELinux utility, `checkpolicy`, which is not available in  #
# the Fuchsia build.                                                           #
################################################################################
#
# This tool invokes the `merge_policies` module to merge predefined collections
# of partial policy files into a predefined binary policy files.

import argparse
import os
import pathlib
import tempfile

import merge_policies

# Use the directory of this script to anchor paths. This logic would need to be
# updated if/when this script is integrated into hermetic builds.
_SCRIPT_DIRECTORY = os.path.dirname(os.path.realpath(__file__))

_INPUT_POLICY_DIRECTORY = f"{_SCRIPT_DIRECTORY}/../testdata/composite_policies"

_OUTPUT_POLICY_DIRECTORY = (
    f"{_SCRIPT_DIRECTORY}/../testdata/composite_policies/compiled"
)

_LEGACY_POLICY_DIRECTORY = f"{_SCRIPT_DIRECTORY}/../testdata/micro_policies"

_INITIAL_SIDS_PATH = os.path.join(_SCRIPT_DIRECTORY, "initial_sids")

_COMPOSITE_POLICY_PATHS = [
    (
        [
            "base_policy.conf",
            "new_file/bounded_transition_policy.conf",
        ],
        "bounded_transition_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/minimal_policy.conf",
        ],
        "minimal_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/class_defaults_policy.conf",
        ],
        "class_defaults_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/exceptions_config_policy.conf",
        ],
        "exceptions_config_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/role_transition_policy.conf",
        ],
        "role_transition_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/role_transition_not_allowed_policy.conf",
        ],
        "role_transition_not_allowed_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/memfd_transition.conf",
        ],
        "memfd_transition.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/type_transition_policy.conf",
        ],
        "type_transition_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/range_transition_policy.conf",
        ],
        "range_transition_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/minimal_policy.conf",
            "new_file/allow_fork.conf",
        ],
        "allow_fork.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/minimal_policy.conf",
            "new_file/with_unlabeled_access_domain_policy.conf",
        ],
        "with_unlabeled_access_domain_policy.pp",
    ),
    (
        [
            "base_policy.conf",
            "new_file/minimal_policy.conf",
            "new_file/with_unlabeled_access_domain_policy.conf",
            "new_file/with_additional_domain_policy.conf",
        ],
        "with_additional_domain_policy.pp",
    ),
]

_HANDLE_UNKNOWN_POLICY_INPUTS = [
    "new_file/handle_unknown_policy.conf",
]
_HANDLE_UNKNOWN_POLICY_OUTPUT = "handle_unknown_policy-%s.pp"

_LEGACY_POLICIES = [
    # keep-sorted start
    "allow_a_attr_b_attr_class0_perm0_policy",
    "allow_a_t_a1_attr_class0_perm0_a2_attr_class0_perm1_policy",
    "allow_a_t_b_attr_class0_perm0_policy",
    "allow_a_t_b_t_class0_perm0_policy",
    "allow_with_constraints_policy",
    "constraints_policy",
    "file_no_defaults_policy",
    "file_range_source_high_policy",
    "file_range_source_low_high_policy",
    "file_range_source_low_policy",
    "file_range_target_high_policy",
    "file_range_target_low_high_policy",
    "file_range_target_low_policy",
    "file_source_defaults_policy",
    "file_target_defaults_policy",
    "hooks_tests_policy",
    "minimal_policy",
    "multiple_levels_and_categories_policy",
    "no_allow_a_attr_b_attr_class0_perm0_policy",
    "no_allow_a_t_b_attr_class0_perm0_policy",
    "no_allow_a_t_b_t_class0_perm0_policy",
    "security_context_tests_policy",
    "security_server_tests_policy",
    # keep-sorted end
]


def _compile_composite_policy(
    checkpolicy_executable: str,
    inputs: list[str],
    output: str,
    handle_unknown: str,
) -> None:
    """(Re)Compile "composite" test policy sources into a binary policy file."""
    input_paths = list(
        f"{_INPUT_POLICY_DIRECTORY}/{input_path}" for input_path in inputs
    )
    output_path = f"{_OUTPUT_POLICY_DIRECTORY}/{output}"
    with tempfile.TemporaryDirectory() as temporary_directory_name:
        merged_path = f"{temporary_directory_name}/policy.conf"
        merge_policies.merge_text_policies(
            _INITIAL_SIDS_PATH, input_paths, merged_path
        )
        merge_policies.compile_text_policy_to_binary_policy(
            checkpolicy_executable,
            merged_path,
            output_path,
            handle_unknown,
        )


def compile_policies(checkpolicy_executable: str) -> None:
    """(Re)Compile all test policies with the specified `checkpolicy_executable`.
    Both "composite" policies, built from a set of fragments, and legacy
    all-in-one policies are rebuilt.
    """
    for inputs, output in _COMPOSITE_POLICY_PATHS:
        _compile_composite_policy(
            checkpolicy_executable, inputs, output, "deny"
        )
    for name in _LEGACY_POLICIES:
        merge_policies.compile_text_policy_to_binary_policy(
            checkpolicy_executable,
            f"{_LEGACY_POLICY_DIRECTORY}/{name}.conf",
            f"{_LEGACY_POLICY_DIRECTORY}/{name}.pp",
            "deny",
        )
    for handle_unknown in ("allow", "deny", "reject"):
        _compile_composite_policy(
            checkpolicy_executable,
            _HANDLE_UNKNOWN_POLICY_INPUTS,
            _HANDLE_UNKNOWN_POLICY_OUTPUT % handle_unknown,
            handle_unknown,
        )


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--checkpolicy-executable",
        required=True,
        type=pathlib.Path,
        help="Path to the SELinux checkpolicy utility executable",
    )
    args = parser.parse_args()

    compile_policies(args.checkpolicy_executable)
