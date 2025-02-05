# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import contextlib
import os
import unittest
from dataclasses import dataclass
from io import StringIO

from command_runner import CommandError, run_command


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

        with contextlib.redirect_stdout(StringIO()) as out:
            with contextlib.redirect_stderr(StringIO()) as err:
                exit_code = run_command(cmd)

                self.assertEqual(exit_code, 0)
                self.assertEqual(err.getvalue(), "")
                self.assertEqual(out.getvalue(), "Hello, World\n")

    def test_run_command_no_binary_found(self) -> None:
        cmd = ["nonexistent_command"]
        got_error = False

        with contextlib.redirect_stdout(StringIO()) as out:
            with contextlib.redirect_stderr(StringIO()) as err:
                try:
                    run_command(cmd)
                except CommandError:
                    got_error = True
        self.assertTrue(got_error)
        self.assertEqual(out.getvalue(), "")
        self.assertEqual(err.getvalue(), "")

    def test_run_command_exit_code(self) -> None:
        cmd = [os.path.join(self.test_data_path, "exit"), "123"]
        with contextlib.redirect_stdout(StringIO()) as out:
            with contextlib.redirect_stderr(StringIO()) as err:
                exit_code = run_command(cmd)
        self.assertEqual(exit_code, 123)
        self.assertEqual(out.getvalue(), "")
        self.assertEqual(err.getvalue(), "")

    def test_run_command_test_output(self) -> None:
        cmd = [os.path.join(self.test_data_path, "output_mock")]
        with contextlib.redirect_stdout(StringIO()) as out:
            with contextlib.redirect_stderr(StringIO()) as err:
                exit_code = run_command(cmd)
        self.assertEqual(exit_code, 0)
        self.assertEqual(
            out.getvalue(), "Line 1\nLine 2\nLine 3\nLine 4\nLine 5\n"
        )
        self.assertEqual(
            err.getvalue(),
            "Error Line 1\nError Line 2\nError Line 3\nError Line 4\nError Line 5\n",
        )


if __name__ == "__main__":
    unittest.main()
