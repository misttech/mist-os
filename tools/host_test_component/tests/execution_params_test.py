# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import json
import unittest

from execution_params import ExecutionParams


class TestExecutionParams(unittest.TestCase):
    TEST_URL = "fuchsia-pkg://fuchsia.com/pkg#meta/test_component.cm"

    def test_init(self) -> None:
        test_url = self.TEST_URL
        test_args = ["arg1", "arg2"]
        test_filters = ["filter1", "filter2"]
        run_disabled_tests = True
        parallel = "auto"
        max_severity_logs = "error"
        realm = "production"

        execution_params = ExecutionParams(
            test_url,
            test_args,
            test_filters,
            run_disabled_tests,
            parallel,
            max_severity_logs,
            realm,
        )

        self.assertEqual(execution_params.test_url, test_url)
        self.assertEqual(execution_params.test_args, test_args)
        self.assertEqual(execution_params.test_filters, test_filters)
        self.assertTrue(execution_params.run_disabled_tests)
        self.assertEqual(execution_params.parallel, parallel)
        self.assertEqual(execution_params.max_severity_logs, max_severity_logs)
        self.assertEqual(execution_params.realm, realm)

    def test_initialize_from_json(self) -> None:
        json_str = """{{
            "test_url": "{}",
            "test_args": ["arg1", "arg2"],
            "test_filters": ["filter1", "filter2"],
            "run_disabled_tests": true,
            "parallel": "1",
            "max_severity_logs": "WARN",
            "realm": "/some/moniker"
        }}""".format(
            self.TEST_URL
        )

        execution_params = ExecutionParams.initialize_from_json(json_str)

        self.assertEqual(execution_params.test_url, self.TEST_URL)
        self.assertEqual(execution_params.test_args, ["arg1", "arg2"])
        self.assertEqual(execution_params.test_filters, ["filter1", "filter2"])
        self.assertTrue(execution_params.run_disabled_tests)
        self.assertEqual(execution_params.parallel, "1")
        self.assertEqual(execution_params.max_severity_logs, "WARN")
        self.assertEqual(execution_params.realm, "/some/moniker")

    def test_initialize_from_json_missing_url(self) -> None:
        # Test with missing 'test_url' key in the JSON data
        json_str = """{
            "test_args": ["arg1", "arg2"],
            "test_filters": ["filter1", "filter2"],
            "run_disabled_tests": true,
            "parallel": "1",
            "max_severity_logs": "INFO",
            "realm": "/some/moniker"
        }"""

        with self.assertRaises(ValueError) as context:
            ExecutionParams.initialize_from_json(json_str)

        self.assertEqual(
            str(context.exception), "Missing 'test_url' in the JSON data"
        )

    def test_initialize_from_json_with_defaults(self) -> None:
        json_data = {
            "test_url": self.TEST_URL,
            # Other keys are missing
        }
        json_str = json.dumps(json_data)

        execution_params = ExecutionParams.initialize_from_json(json_str)

        self.assertEqual(execution_params.test_url, self.TEST_URL)
        self.assertEqual(execution_params.test_args, [])
        self.assertEqual(execution_params.test_filters, [])
        self.assertFalse(execution_params.run_disabled_tests)
        self.assertIsNone(execution_params.parallel)
        self.assertIsNone(execution_params.max_severity_logs)
        self.assertIsNone(execution_params.realm)
