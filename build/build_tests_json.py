#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""
Generate tests.json.
"""

import json
import os
import typing as T
from pathlib import Path


def build_tests_json(build_dir: Path) -> T.Set[Path]:
    """Generate the tests.json file.

    tests.json is created by merging two things:

    1) tests_from_metadata.json
       A collection of test specs found from a GN metadata walk. These test
       specs are copied without modification into the final tests.json.

    2) product_bundle_test_groups.json
       A file that declares a mapping of product bundle name to a specific set
       of tests found in another tests.json. These test specs will be modified
       to include `product_bundle: <name>` before merging it into the final
       tests.json.

    Args:
        build_dir: Fuchsia build directory.

    Returns:
        A set of Path values for the input files read by this function.
    """
    tests_from_metadata_path = os.path.join(
        build_dir, "tests_from_metadata.json"
    )
    test_groups_path = os.path.join(
        build_dir, "obj", "tests", "product_bundle_test_groups.json"
    )
    tests_json_path = os.path.join(build_dir, "tests.json")

    # Open the list of tests that were collected from a GN metadata walk.
    with open(tests_from_metadata_path, "r") as f:
        tests = json.load(f)

    # Open the mapping between test sets and product bundle name.
    with open(test_groups_path, "r") as f:
        test_groups = json.load(f)
    # For every group of tests that are supposed to target a specific product
    # bundle, we parse the tests, add `product_bundle: <name>` and add the test
    # to `tests`. When infra reads the final tests.json file, it will read that
    # field and know to flash the product bundle with <name> before running the
    # test.
    for test_group in test_groups:
        product_bundle_name = test_group["product_bundle_name"]
        product_bundle_tests_file = build_dir / test_group["tests_json"]
        # Read the tests.json that is assigned to this specific product bundle.
        with open(product_bundle_tests_file, "r") as inner_tests_json:
            product_bundle_tests = json.load(inner_tests_json)
        # Update the test spec to include the product bundle target.
        for test in product_bundle_tests:
            name = test["test"]["name"] + "-" + product_bundle_name
            test["test"]["name"] = name
            test["product_bundle"] = product_bundle_name
        tests += product_bundle_tests

    # Write the final list of tests to tests.json if the contents changed.
    contents_changed = True
    if Path(tests_json_path).exists():
        with open(tests_json_path, "r") as f:
            previous_tests = json.load(f)
            if previous_tests == tests:
                contents_changed = False
    if contents_changed:
        with open(tests_json_path, "w") as f:
            json.dump(tests, f, indent=2)

    return {Path(tests_from_metadata_path), Path(test_groups_path)}
