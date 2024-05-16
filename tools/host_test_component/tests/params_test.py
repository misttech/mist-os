# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import os
import tempfile

from params import (
    Params,
    SDK_TOOL_PATH_KEY,
    EXECUTION_JSON_KEY,
    OUTPUT_DIRECTORY_KEY,
    TARGETS_KEY,
)
from execution_params import ExecutionParams


class TestParams(unittest.TestCase):
    TEST_URL = "fuchsia-pkg://fuchsia.com/pkg#meta/test_component.cm"

    def setUp(self):
        # Create temporary directories for sdk tool path and output directory
        self.sdk_tool_path = tempfile.mkdtemp()
        self.output_directory = tempfile.mkdtemp()

    def tearDown(self):
        # Clean up temporary directories
        if os.path.exists(self.sdk_tool_path):
            os.rmdir(self.sdk_tool_path)
        if os.path.exists(self.output_directory):
            os.rmdir(self.output_directory)

    def test_valid_initialization(self):
        env_vars = {
            SDK_TOOL_PATH_KEY: self.sdk_tool_path,
            TARGETS_KEY: "target1",
            OUTPUT_DIRECTORY_KEY: self.output_directory,
            EXECUTION_JSON_KEY: """{{
                    "test_url": "{}",
                    "test_args": ["arg1", "arg2"],
                    "test_filters": ["filter1", "filter2"],
                    "run_disabled_tests": true,
                    "parallel": "1",
                    "max_severity_logs": "INFO",
                    "realm": "/some/moniker"
                }}""".format(
                self.TEST_URL
            ),
        }

        params = Params.initialize(env_vars)

        self.assertEqual(params.sdk_tool_path, self.sdk_tool_path)
        self.assertEqual(params.target, "target1")
        self.assertIsInstance(params.execution_params, ExecutionParams)
        self.assertEqual(params.output_directory, self.output_directory)

        # Verify attributes of ExecutionParams
        execution_params = params.execution_params
        self.assertEqual(execution_params.test_url, self.TEST_URL)
        self.assertEqual(execution_params.test_args, ["arg1", "arg2"])
        self.assertEqual(execution_params.test_filters, ["filter1", "filter2"])
        self.assertTrue(execution_params.run_disabled_tests)
        self.assertEqual(execution_params.parallel, "1")
        self.assertEqual(execution_params.max_severity_logs, "INFO")
        self.assertEqual(execution_params.realm, "/some/moniker")

    def test_missing_sdk_tool_path(self):
        env_vars = {
            TARGETS_KEY: "target1",
            OUTPUT_DIRECTORY_KEY: self.output_directory,
            EXECUTION_JSON_KEY: '{"test_url": self.TEST_URL}',
        }

        with self.assertRaises(ValueError) as context:
            Params.initialize(env_vars)

        self.assertEqual(
            str(context.exception),
            f"'{SDK_TOOL_PATH_KEY}' environment variable is not available.",
        )

    def test_missing_output_directory(self):
        env_vars = {
            SDK_TOOL_PATH_KEY: self.sdk_tool_path,
            TARGETS_KEY: "target1",
            EXECUTION_JSON_KEY: '{"test_url": self.TEST_URL}',
        }

        with self.assertRaises(ValueError) as context:
            Params.initialize(env_vars)

        self.assertEqual(
            str(context.exception),
            f"'{OUTPUT_DIRECTORY_KEY}' environment variable is not available.",
        )

    def test_non_existent_sdk_tool_path(self):
        env_vars = {
            SDK_TOOL_PATH_KEY: "/nonexistent/path/to/sdk_tool",
            TARGETS_KEY: "target1",
            OUTPUT_DIRECTORY_KEY: self.output_directory,
            EXECUTION_JSON_KEY: '{"test_url": self.TEST_URL}',
        }

        with self.assertRaises(ValueError) as context:
            Params.initialize(env_vars)

        self.assertEqual(
            str(context.exception),
            f"'{SDK_TOOL_PATH_KEY}: /nonexistent/path/to/sdk_tool' path does not exist.",
        )

    def test_non_existent_output_directory(self):
        env_vars = {
            SDK_TOOL_PATH_KEY: self.sdk_tool_path,
            TARGETS_KEY: "target1",
            OUTPUT_DIRECTORY_KEY: "/nonexistent/output",
            EXECUTION_JSON_KEY: '{"test_url": self.TEST_URL}',
        }

        with self.assertRaises(ValueError) as context:
            Params.initialize(env_vars)

        self.assertEqual(
            str(context.exception),
            f"'{OUTPUT_DIRECTORY_KEY}: /nonexistent/output' path does not exist.",
        )

    def test_missing_execution_json(self):
        env_vars = {
            SDK_TOOL_PATH_KEY: self.sdk_tool_path,
            TARGETS_KEY: "target1",
            OUTPUT_DIRECTORY_KEY: self.output_directory,
        }

        with self.assertRaises(ValueError) as context:
            Params.initialize(env_vars)

        self.assertEqual(
            str(context.exception),
            f"'{EXECUTION_JSON_KEY}' environment variable is not available.",
        )

    def test_invalid_execution_json(self):
        env_vars = {
            SDK_TOOL_PATH_KEY: self.sdk_tool_path,
            TARGETS_KEY: "target1",
            OUTPUT_DIRECTORY_KEY: self.output_directory,
            EXECUTION_JSON_KEY: "{}",
        }

        with self.assertRaises(ValueError) as context:
            Params.initialize(env_vars)

        self.assertEqual(
            str(context.exception),
            "Missing 'test_url' in the JSON data",
        )
