# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import unittest
import os

from unittest.mock import MagicMock
from dataclasses import dataclass
from command_runner import run_command


@dataclass
class OutputHandler:
    output: bytes = b""

    def __call__(self, line: bytes) -> None:
        self.output += line


class TestCommandRunner(unittest.TestCase):
    def setUp(self) -> None:
        self.cur_path = os.path.dirname(__file__)
        while not os.path.isdir(self.cur_path):
            self.cur_path = os.path.split(self.cur_path)[0]
        self.test_data_path = os.path.join(self.cur_path, "test_data")

    def test_run_command_success(self) -> None:
        cmd = ["echo", "Hello, World"]
        output_handler = OutputHandler()
        error_handler = MagicMock()

        exit_code = run_command(cmd, output_handler, error_handler)

        self.assertEqual(exit_code, 0)
        error_handler.assert_not_called()
        self.assertEqual(output_handler.output, b"Hello, World\n")

    def test_run_command_no_binary_found(self) -> None:
        cmd = ["nonexistent_command"]
        output_handler = MagicMock()
        error_handler = MagicMock()
        got_error = False
        try:
            run_command(cmd, output_handler, error_handler)
        except FileNotFoundError:
            got_error = True
        self.assertTrue(got_error)
        output_handler.assert_not_called()
        error_handler.assert_not_called()

    def test_run_command_exit_code(self) -> None:
        cmd = [os.path.join(self.test_data_path, "exit"), "123"]
        output_handler = MagicMock()
        error_handler = MagicMock()
        exit_code = run_command(cmd, output_handler, error_handler)
        self.assertEqual(exit_code, 123)
        output_handler.assert_not_called()
        error_handler.assert_not_called()

    def test_run_command_test_output(self) -> None:
        cmd = [os.path.join(self.test_data_path, "output_mock")]
        output_handler = OutputHandler()
        error_handler = OutputHandler()
        exit_code = run_command(cmd, output_handler, error_handler)
        self.assertEqual(exit_code, 0)
        self.assertEqual(
            output_handler.output, b"Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n"
        )
        self.assertEqual(
            error_handler.output,
            b"Error Line 1\nError Line 2\nError Line 3\nError Line 4\nError Line 5\n",
        )


if __name__ == "__main__":
    unittest.main()
