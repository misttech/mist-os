#!/usr/bin/env fuchsia-vendored-python
# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import merge_policies
import pathlib
import tempfile
import unittest


class FastbootTests(unittest.TestCase):
    TESTDATA_DIR = None

    def test_merge_success(self):
        a_policy_path = f"{FastbootTests.TESTDATA_DIR}/a_policy.conf"
        b_policy_path = f"{FastbootTests.TESTDATA_DIR}/b_policy.conf"
        golden_policy_path = (
            f"{FastbootTests.TESTDATA_DIR}/golden_a_b_policy.conf"
        )
        with tempfile.TemporaryDirectory() as temporary_directory_name:
            merged_policy_path = f"{temporary_directory_name}/policy.conf"
            merge_policies.merge_text_policies(
                (a_policy_path, b_policy_path), merged_policy_path
            )
            with open(merged_policy_path) as merged_policy, open(
                golden_policy_path
            ) as golden_policy:
                self.assertSequenceEqual(
                    tuple(merged_policy), tuple(golden_policy)
                )

    def test_merge_failure(self):
        a_policy_path = f"{FastbootTests.TESTDATA_DIR}/a_policy.conf"
        invalid_policy_path = (
            f"{FastbootTests.TESTDATA_DIR}/invalid_policy.conf"
        )

        with tempfile.TemporaryDirectory() as temporary_directory_name:
            merged_policy_path = f"{temporary_directory_name}/policy.conf"
            with self.assertRaises(ValueError) as context:
                merge_policies.merge_text_policies(
                    (a_policy_path, invalid_policy_path), merged_policy_path
                )

            self.assertIn("Expected policy with", str(context.exception))
            self.assertIn(
                "but filtered policy contains", str(context.exception)
            )


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument(
        "--testdata-dir",
        required=True,
        type=pathlib.Path,
        help="Path to testdata",
    )
    args = parser.parse_args()
    FastbootTests.TESTDATA_DIR = args.testdata_dir
    unittest.main(argv=["-v"])


if __name__ == "__main__":
    main()
