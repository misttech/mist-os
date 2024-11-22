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

_INITIAL_SIDS_PATH = os.path.join(_SCRIPT_DIRECTORY, "initial_sids")

_COMPOSITE_POLICY_PATHS = (
    (
        (
            "base_policy.conf",
            "new_file/bounded_transition_policy.conf",
        ),
        "bounded_transition_policy.pp",
    ),
    (
        (
            "base_policy.conf",
            "new_file/minimal_policy.conf",
        ),
        "minimal_policy.pp",
    ),
    (
        (
            "base_policy.conf",
            "new_file/class_defaults_policy.conf",
        ),
        "class_defaults_policy.pp",
    ),
    (
        (
            "base_policy.conf",
            "new_file/role_transition_policy.conf",
        ),
        "role_transition_policy.pp",
    ),
    (
        (
            "base_policy.conf",
            "new_file/role_transition_not_allowed_policy.conf",
        ),
        "role_transition_not_allowed_policy.pp",
    ),
    (
        (
            "base_policy.conf",
            "new_file/type_transition_policy.conf",
        ),
        "type_transition_policy.pp",
    ),
    (
        (
            "base_policy.conf",
            "new_file/range_transition_policy.conf",
        ),
        "range_transition_policy.pp",
    ),
    (
        (
            "base_policy.conf",
            "new_file/minimal_policy.conf",
            "new_file/allow_fork.conf",
        ),
        "allow_fork.pp",
    ),
)


def compile_composite_policies(checkpolicy_executable):
    for inputs, output in _COMPOSITE_POLICY_PATHS:
        with tempfile.TemporaryDirectory() as temporary_directory_name:
            input_paths = tuple(
                f"{_INPUT_POLICY_DIRECTORY}/{input_path}"
                for input_path in inputs
            )
            output_path = f"{_OUTPUT_POLICY_DIRECTORY}/{output}"
            merged_path = f"{temporary_directory_name}/policy.conf"
            merge_policies.merge_text_policies(
                _INITIAL_SIDS_PATH, input_paths, merged_path
            )
            merge_policies.compile_text_policy_to_binary_policy(
                checkpolicy_executable,
                merged_path,
                output_path,
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

    compile_composite_policies(args.checkpolicy_executable)
