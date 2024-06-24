# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest

from command import Command


class TestCommand(unittest.TestCase):
    def setUp(self) -> None:
        self.sdk_tool_path = "/path/to/sdk"
        self.ffx_path = f"{self.sdk_tool_path}/ffx"
        self.target = "test_target"
        self.test_url = "fuchsia-pkg://fuchsia.com/pkg#meta/test_component.cm"
        self.realm = "/test_realm"
        self.max_severity_logs = "INFO"
        self.test_args = ["arg1", "arg2"]
        self.test_filters = ["filter1", "filter2"]
        self.run_disabled_tests = True
        self.parallel = "1"
        self.output_directory = "/path/to/output"

        self.command = Command(
            self.sdk_tool_path,
            self.target,
            self.test_url,
            self.realm,
            self.max_severity_logs,
            self.test_args,
            self.test_filters,
            self.run_disabled_tests,
            self.parallel,
            self.output_directory,
        )

    def test_get_command(self) -> None:
        isolate_dir = "/path/to/isolate"

        expected_command = [
            self.ffx_path,
            "--isolate-dir",
            isolate_dir,
            "-t",
            self.target,
            "test",
            "run",
            self.test_url,
            "--realm",
            self.realm,
            "--output-directory",
            self.output_directory,
            "--max-severity-logs",
            self.max_severity_logs,
            "--parallel",
            self.parallel,
            "--run-disabled",
        ]

        self.assertEqual(
            self.command.get_command(isolate_dir), expected_command
        )

    def test_get_command_with_default_values(self) -> None:
        self.command = Command(
            self.sdk_tool_path,
            None,
            self.test_url,
            None,
            None,
            [],
            [],
            False,
            None,
            self.output_directory,
        )

        isolate_dir = "/path/to/isolate"

        expected_command = [
            self.ffx_path,
            "--isolate-dir",
            isolate_dir,
            "test",
            "run",
            self.test_url,
            "--output-directory",
            self.output_directory,
        ]

        self.assertEqual(
            self.command.get_command(isolate_dir), expected_command
        )
