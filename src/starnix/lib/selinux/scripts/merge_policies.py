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
# Known limitations:
# - All output policies are `# handle_unknown deny`;
# - Conditionals are not supported: inputs must be a sequence of policy
#   statements that are exactly one line each; empty lines and comment lines are
#   also allowed.

import argparse
import pathlib
import re
import subprocess
import sys
import tempfile

# Lines in policy files that match these patterns must be grouped together in
# the order the patterns appear.
_ORDERED_POLICY_STATEMENT_PATTERNS = tuple(
    re.compile(f"^{pattern}$")
    for pattern in (
        "class[ \t\v]+[^ \t\v]+",
        "sid[ \t\v]+[^ \t\v]+",
        "common[ \t\v]+[^ \t\v]+.*",
        "class[ \t\v]+[^ \t\v]+[ \t\v]+.*",
        "default_(user|role|type|range)[ \t\v]+[^ \t\v]+.*",
        "sensitivity[ \t\v]+[^ \t\v]+.*",
        "dominance[ \t\v]+[^ \t\v]+.*",
        "category[ \t\v]+[^ \t\v]+.*",
        "level[ \t\v]+[^ \t\v]+.*",
        "mlsconstrain[ \t\v]+[^ \t\v]+.*",
        "policycap[ \t\v]+[^ \t\v]+.*",
        "attribute[ \t\v]+[^ \t\v]+.*",
        "bool[ \t\v]+[^ \t\v]+.*",
        "type[ \t\v]+[^ \t\v]+.*",
        "typealias[ \t\v]+[^ \t\v]+.*",
        "typeattribute[ \t\v]+[^ \t\v]+.*",
        "allow[ \t\v]+[^ \t\v]+[ \t\v]+[^ \t\v:]+([ \t\v]*:[ \t\v]*[^ \t\v]+)[ \t\v]+.*",
        "dontaudit[ \t\v]+[^ \t\v]+.*",
        "type_transition[ \t\v]+[^ \t\v]+[ \t\v]+[^ \t\v:]+([ \t\v]*:[ \t\v]*[^ \t\v]+)[ \t\v]+[^ \t\v]+",
        "type_member[ \t\v]+[^ \t\v]+.*",
        "type_change[ \t\v]+[^ \t\v]+.*",
        "type_transition[ \t\v]+[^ \t\v]+[ \t\v]+[^ \t\v:]+([ \t\v]*:[ \t\v]*[^ \t\v]+)[ \t\v]+[^ \t\v]+[ \t\v]+.*",
        "range_transition[ \t\v]+[^ \t\v]+.*",
        "role[ \t\v]+[^ \t\v]+",
        "role[ \t\v]+[^ \t\v]+[ \t\v]+.*",
        "attribute_role[ \t\v]+[^ \t\v]+.*",
        "roleattribute[ \t\v]+[^ \t\v]+.*",
        "role_transition[ \t\v]+[^ \t\v]+.*$",
        "allow[ \t\v]+[^ \t\v]+[ \t\v]+[^ \t\v]+$",
        "user[ \t\v]+[^ \t\v]+.*$",
        "constrain[ \t\v]+[^ \t\v]+.*$",
        "sid[ \t\v]+[^ \t\v]+[ \t\v]+.*$",
        "fs_use_xattr[ \t\v]+[^ \t\v]+.*$",
        "fs_use_trans[ \t\v]+[^ \t\v]+.*$",
        "fs_use_task[ \t\v]+[^ \t\v]+.*$",
        "genfscon[ \t\v]+[^ \t\v]+.*$",
        "portcon[ \t\v]+[^ \t\v]+.*$",
    )
)

# Regular expression for empty/comment-only lines to check that all meaningful
# lines matched a pattern.
_WHITESPACE_OR_COMMENT_PATTERN = re.compile("^[ \t]*(#.*)?$")


def _filter_lines(lines, pattern):
    return tuple(line for line in lines if pattern.match(line) is not None)


def _negative_filter_lines(lines, pattern):
    return tuple(line for line in lines if pattern.match(line) is None)


def _compile_text_policy_to_binary_policy(
    checkpolicy_executable_path, input_file_path, output_file_path
):
    subprocess.run(
        [
            checkpolicy_executable_path,
            "--mls",
            "-c",
            "33",
            "--output",
            output_file_path,
            "-t",
            "selinux",
            input_file_path,
        ],
        check=True,
    )


def merge_text_policies(input_file_paths, output_file_path):
    # Accumulate lines of input from all `input_file_paths` and sort them.
    unsorted_input_lines = set()
    for input_path in input_file_paths:
        with open(input_path, mode="rt") as input_file:
            # Use `splitlines()` to omit `\n`.
            unsorted_input_lines.update(input_file.read().splitlines())
    input_lines = tuple(sorted(unsorted_input_lines))

    # Accumulate input lines grouped according to which statement pattern
    # they match. This step is required to ensure that `checkpolicy` will
    # compile the combined policy statements from all `input_file_paths`.
    policy_lines_from_input_files = []
    for pattern in _ORDERED_POLICY_STATEMENT_PATTERNS:
        policy_lines_from_input_files.extend(
            _filter_lines(input_lines, pattern)
        )
    policy_lines_from_input_files = tuple(policy_lines_from_input_files)

    # Filter out empty or comment-only lines and count them. This will be
    # used to ensure that no meaningful lines are discarded when the policy
    # has been processed.
    expected_lines = frozenset(
        _negative_filter_lines(input_lines, _WHITESPACE_OR_COMMENT_PATTERN)
    )
    expected_num_lines = len(expected_lines)

    # Ensure that no meaningful policy statements were discarded.
    actual_num_lines = len(policy_lines_from_input_files)
    if actual_num_lines != expected_num_lines:
        # If some statements were discarded, emit a diff to stderr and
        # `raise`.
        text_policy_set = frozenset(policy_lines_from_input_files)
        for found_but_unexpected_line in text_policy_set - expected_lines:
            print(f"< {found_but_unexpected_line}", file=sys.stderr)
        for expected_but_not_found_line in expected_lines - text_policy_set:
            print(f"> {expected_but_not_found_line}", file=sys.stderr)

        raise ValueError(
            f"Expected policy with {expected_num_lines}, but filtered policy contains {actual_num_lines} lines"
        )

    with open(output_file_path, mode="wt") as output_file:
        # All policies must begin with a `# handle_unknown ...` clause. Tests
        # usually implement "default deny" and allow what is necessary.
        output_file.write(f"# handle_unknown deny\n")

        for line in policy_lines_from_input_files:
            output_file.write(f"{line}\n")


if __name__ == "__main__":
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--checkpolicy-executable",
        required=True,
        type=pathlib.Path,
        help="Path to the SELinux checkpolicy utility executable",
    )
    parser.add_argument(
        "--output",
        required=True,
        type=pathlib.Path,
        help="Path to use for output binary policy file",
    )
    parser.add_argument(
        "text_partial_policy",
        nargs="+",
        type=pathlib.Path,
        help="Paths to partial policy files to be merged into combined policy",
    )
    args = parser.parse_args()

    with tempfile.TemporaryDirectory() as temporary_directory_name:
        text_policy_file_path = f"{temporary_directory_name}/policy.conf"
        merge_text_policies(args.text_partial_policy, text_policy_file_path)
        compile_text_policy_to_binary_policy(text_policy_file_path, args.output)
