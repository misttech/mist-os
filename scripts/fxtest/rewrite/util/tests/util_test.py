# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import argparse
import asyncio
import os
from tempfile import TemporaryDirectory
import unittest

from util import arg_option
from util import command


class TestArgOptions(unittest.TestCase):
    def test_boolean_optional_action(self):
        """Test BooleanOptionalAction.

        This test ensures that a flag --build is set to True when --build
        is passed, False when --no-build is passed, and None if not
        specified.
        """

        parser = argparse.ArgumentParser()
        parser.add_argument(
            "--build", action=arg_option.BooleanOptionalAction, default=None
        )

        self.assertEqual(parser.parse_args([]).build, None)
        self.assertEqual(parser.parse_args(["--build"]).build, True)
        self.assertEqual(parser.parse_args(["--no-build"]).build, False)

    def test_boolean_optional_action_failure(self):
        """Test that setting nargs raises an error."""

        parser = argparse.ArgumentParser()
        self.assertRaises(
            ValueError,
            lambda: parser.add_argument(
                "--foo", action=arg_option.BooleanOptionalAction, nargs=1
            ),
        )

    def test_selection_action(self):
        """Test SelectionAction.

        This test ensures that multiple arguments can all write to the
        same destination variable. Short/long names for flags are
        canonicalized to the long version.
        """

        parser = argparse.ArgumentParser()
        parser.add_argument(
            "-m", "--main-option", action=arg_option.SelectionAction, dest="option"
        )
        parser.add_argument(
            "-a", "--alt-option", action=arg_option.SelectionAction, dest="option"
        )
        parser.add_argument("option", action=arg_option.SelectionAction)

        args = parser.parse_args(["-m", "one", "two", "-a", "three", "four"])
        self.assertListEqual(
            args.option,
            [
                "--main-option",
                "one",
                "two",
                "--alt-option",
                "three",
                "four",
            ],
        )


class TestCommand(unittest.IsolatedAsyncioTestCase):
    def assertStdout(self, event: command.CommandEvent, line: bytes):
        """Helper to assert on contents of a StdoutEvent.

        Args:
            event (command.CommandEvent): Event to cast and compare.
            line (bytes): Expected line value.
        """
        self.assertTrue(isinstance(event, command.StdoutEvent))
        assert isinstance(event, command.StdoutEvent)
        e: command.StdoutEvent = event
        self.assertEqual(e.text, line)

    def assertStderr(self, event: command.CommandEvent, line: bytes):
        """Helper to assert on contents of a StderrEvent.

        Args:
            event (command.CommandEvent): Event to cast and compare.
            line (bytes): Expected line value.
        """
        self.assertTrue(isinstance(event, command.StderrEvent))
        assert isinstance(event, command.StderrEvent)
        e: command.StderrEvent = event
        self.assertEqual(e.text, line)

    def assertTermination(self, event: command.CommandEvent, return_code: int):
        """Helper to assert on contents of a TerminationEvent.

        Args:
            event (command.CommandEvent): Event to cast and compare.
            return_code (int): Expected return code.
        """
        self.assertTrue(isinstance(event, command.TerminationEvent))
        assert isinstance(event, command.TerminationEvent)
        e: command.TerminationEvent = event
        self.assertEqual(e.return_code, return_code)

    async def test_basic_command(self):
        """Test running a basic command and getting the output.

        We create a file in a temporary directory and simply assert that `ls`
        prints that file as output.
        """
        with TemporaryDirectory() as td:
            with open(os.path.join(td, "temp-file.txt"), "w") as f:
                f.write("hello world")

            cmd = await command.AsyncCommand.create("ls", ".", env={"CWD": td})
            events = []
            complete = await cmd.run_to_completion(lambda event: events.append(event))
            self.assertEqual(len(events), 2, f"Events was actually {events}")

            self.assertStdout(events[0], b"temp-file.txt\n")
            self.assertTermination(events[1], 0)

            self.assertEqual(complete.stdout, "temp-file.txt\n")
            self.assertEqual(complete.return_code, 0)

    async def test_with_stderr(self):
        """Test running a command with stderr output.

        We create a temporary directory and try to `ls` a file we know does not
        exist. `ls` should print to stderr and report an error return code.
        """
        with TemporaryDirectory() as td:
            cmd = await command.AsyncCommand.create(
                "ls", os.path.join(td, "does-not-exist")
            )
            complete = await cmd.run_to_completion()
            self.assertEqual(complete.stdout, "")
            self.assertNotEqual(complete.stderr, "")
            self.assertNotEqual(complete.return_code, 0)

    async def test_symbolized_command(self):
        """Test piping output through another program.

        We run `ls` as in the above tests, but this time we pipe the output
        through `sed` to change the word "temp" to "temporary" and assert on
        the new output.
        """
        with TemporaryDirectory() as td:
            with open(os.path.join(td, "temp-file.txt"), "w") as f:
                f.write("hello world")

            cmd = await command.AsyncCommand.create(
                "ls",
                ".",
                env={"CWD": td},
                symbolizer_args=["sed", "s/temp/temporary/g"],
            )
            events = []
            await cmd.run_to_completion(lambda event: events.append(event))
            self.assertEqual(len(events), 2, f"Events was actually {events}")

            self.assertStdout(events[0], b"temporary-file.txt\n")
            self.assertTermination(events[1], 0)

    async def test_terminate_and_kill(self):
        """Test that we can terminate and kill programs.

        We spawn `sleep` to run for over a day, then terminate it. We expect
        the return code to be set by the OS to represent that the program
        was killed.
        """
        cmd = await command.AsyncCommand.create("sleep", "100000")
        task = asyncio.create_task(cmd.run_to_completion())
        cmd.terminate()
        out: command.CommandOutput = await task
        self.assertEqual(out.return_code, -15)

        cmd = await command.AsyncCommand.create("sleep", "100000")
        task = asyncio.create_task(cmd.run_to_completion())
        cmd.kill()
        out = await task
        self.assertEqual(out.return_code, -9)

        cmd = await command.AsyncCommand.create(
            "sleep", "100000", symbolizer_args=["sleep", "100000"]
        )
        task = asyncio.create_task(cmd.run_to_completion())
        cmd.terminate()
        out = await task
        self.assertEqual(out.return_code, -15)
        self.assertEqual(out.wrapper_return_code, -15)

        cmd = await command.AsyncCommand.create(
            "sleep", "100000", symbolizer_args=["sleep", "100000"]
        )
        task = asyncio.create_task(cmd.run_to_completion())
        cmd.kill()
        out = await task
        self.assertEqual(out.return_code, -9)
        self.assertEqual(out.wrapper_return_code, -9)

    def test_invalid_program(self):
        """Test running a program that doesn't exist, and expect an error."""
        self.assertRaises(
            command.AsyncCommandError,
            lambda: asyncio.run(command.AsyncCommand.create("..........")),
        )


if __name__ == "__main__":
    unittest.main()
