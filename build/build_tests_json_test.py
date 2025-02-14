#!/usr/bin/env fuchsia-vendored-python
# Copyright 2025 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

"""Unit-tests for build/build_tests_json.py functions."""

import json
import os
import sys
import tempfile
import typing as T
import unittest
from pathlib import Path

# Import build_tests_json.py as a module.
_SCRIPT_DIR = os.path.dirname(__file__)
sys.path.insert(0, _SCRIPT_DIR)
import build_tests_json


class BuildTestsJsonTest(unittest.TestCase):
    def setUp(self):
        self._td = tempfile.TemporaryDirectory()
        self._dir = Path(self._td.name)
        self.source_dir = self._dir / "source"
        self.source_dir.mkdir()

        self.build_dir = self.source_dir / "out" / "not-default"
        self.build_dir.mkdir(parents=True)

        self.output_dir = self._dir / "output"
        self.output_dir.mkdir()

        (self.build_dir / "obj" / "tests").mkdir(parents=True)

    def tearDown(self):
        self._td.cleanup()

    def _test(
        self, tests_from_metadata: T.Dict, test_groups: T.Dict
    ) -> (T.Set[Path], T.Dict):
        tests_from_metadata = json.dumps(tests_from_metadata)
        tests_from_metadata_path = self.build_dir / "tests_from_metadata.json"
        tests_from_metadata_path.write_text(tests_from_metadata)

        test_groups = json.dumps(test_groups)
        test_groups_path = (
            self.build_dir / "obj" / "tests" / "product_bundle_test_groups.json"
        )
        test_groups_path.write_text(test_groups)

        inputs = build_tests_json.build_tests_json(self.build_dir)

        tests_string = (self.build_dir / "tests.json").read_text()
        tests = json.loads(tests_string)

        return (inputs, tests)

    def test_only_tests_from_metadata(self):
        tests_from_metadata = [
            {"test": {"name": "test1"}},
            {"test": {"name": "test2"}},
        ]
        (_, tests) = self._test(tests_from_metadata, [])
        self.assertEqual(tests_from_metadata, tests)

    def test_only_test_groups(self):
        tests_json = [{"test": {"name": "test1"}}, {"test": {"name": "test2"}}]
        tests_json = json.dumps(tests_json)
        tests_json_path = self.build_dir / "pb_tests.json"
        tests_json_path.write_text(tests_json)

        test_groups = [
            {"product_bundle_name": "my_pb", "tests_json": str(tests_json_path)}
        ]
        (_, tests) = self._test([], test_groups)

        expected_tests_json = [
            {"product_bundle": "my_pb", "test": {"name": "test1-my_pb"}},
            {"product_bundle": "my_pb", "test": {"name": "test2-my_pb"}},
        ]
        self.assertEqual(expected_tests_json, tests)

    def test_full(self):
        tests_from_metadata = [
            {"test": {"name": "test1"}},
            {"test": {"name": "test2"}},
        ]
        tests_json = [{"test": {"name": "test1"}}, {"test": {"name": "test2"}}]
        tests_json = json.dumps(tests_json)
        tests_json_path = self.build_dir / "pb_tests.json"
        tests_json_path.write_text(tests_json)

        test_groups = [
            {"product_bundle_name": "my_pb", "tests_json": str(tests_json_path)}
        ]
        (_, tests) = self._test(tests_from_metadata, test_groups)

        expected_tests_json = [
            {"test": {"name": "test1"}},
            {"test": {"name": "test2"}},
            {"product_bundle": "my_pb", "test": {"name": "test1-my_pb"}},
            {"product_bundle": "my_pb", "test": {"name": "test2-my_pb"}},
        ]
        self.assertEqual(expected_tests_json, tests)

    def test_ninja_inputs(self):
        (inputs, _) = self._test([], [])
        self.assertEqual(
            {
                Path(self.build_dir / "tests_from_metadata.json"),
                Path(
                    self.build_dir
                    / "obj"
                    / "tests"
                    / "product_bundle_test_groups.json"
                ),
            },
            inputs,
        )

    def test_only_write_if_changed(self):
        tests_from_metadata = [
            {"test": {"name": "test1"}},
            {"test": {"name": "test2"}},
        ]
        tests_json = json.dumps(tests_from_metadata)
        tests_json_path = self.build_dir / "tests.json"
        tests_json_path.write_text(tests_json)
        previous_write_time = os.path.getmtime(tests_json_path)

        self._test(tests_from_metadata, [])

        # ensure the file did not change
        current_write_time = os.path.getmtime(tests_json_path)
        self.assertEqual(previous_write_time, current_write_time)


if __name__ == "__main__":
    unittest.main()
