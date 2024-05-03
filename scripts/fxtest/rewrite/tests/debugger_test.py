# Copyright 2024 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import io
import os
import signal
import subprocess
import typing
import unittest
import unittest.mock as mock

import debugger
from test_list_file import Test
import tests_json_file


class TestDebuggerTest(unittest.IsolatedAsyncioTestCase):
    def setUp(self) -> None:
        # Setup a mock for the subprocess.
        self.subprocess_mock_ = mock.MagicMock(return_value=mock.MagicMock())
        patch = mock.patch("debugger.subprocess.Popen", self.subprocess_mock_)
        patch.start()
        self.addCleanup(patch.stop)

        # Also need to mock out open since we're not actually creating a fifo.
        open_patch = mock.patch("builtins.open", mock.mock_open())
        open_mock = open_patch.start()
        open_mock.return_value = io.StringIO()
        self.addCleanup(open_patch.stop)

        self.fifo_path_ = ""

        def set_fifo_path(*args: typing.Any, **kwargs: typing.Any) -> None:
            self.fifo_path_ = args[0]

        # Replace mkfifo with nothing but our side effect. We just care about the path that was
        # generated
        mkfifo_patch = mock.patch(
            "debugger.os.mkfifo", side_effect=set_fifo_path
        )
        mkfifo_patch.start()
        self.addCleanup(mkfifo_patch.stop)

        return super().setUp()

    async def test_break_on_failure(self) -> None:
        """Tests zxdb command generation with no breakpoints and break-on-failure is set."""

        # Don't need to do any waiting to check the arguments are correct.
        async def callback() -> None:
            pass

        package_name = "fuchsia-pkg://fuchsia.com/foo_test#meta/foo_test.cm"
        test = Test(
            build=tests_json_file.TestEntry(
                test=tests_json_file.TestSection(package_name, "", ""),
            ),
        )

        debugger.spawn([test], callback, True, [])

        # Should have immediately called the mock, since debugger.spawn is synchronous.
        self.assertTrue(self.subprocess_mock_.called)

        expected_args = [
            "fx",
            "ffx",
            "debug",
            "connect",
            "--new-agent",
            "--",
            "--execute",
            f"attach --weak --recursive {package_name}",
            "--console-mode",
            "embedded",
            "--embedded-mode-context",
            "test failure",
            "--stream-file",
            f"{self.fifo_path_}",
            "--signal-when-ready",
            str(os.getpid()),
        ]

        self.subprocess_mock_.assert_called_with(
            args=expected_args,
            start_new_session=True,
            stderr=subprocess.STDOUT,
        )

    async def test_break_on_failure_multiple_packages(self) -> None:
        """Tests zxdb command generation when there are multiple packages selected for test."""

        # Don't need to do any waiting to check the arguments are correct.
        async def callback() -> None:
            pass

        tests = []
        package_name = "fuchsia-pkg://fuchsia.com/foo_test#meta/foo_test.cm"
        package_name2 = "fuchsia-pkg://fuchsia.com/bar_test#meta/bar_test.cm"
        tests.append(
            Test(
                build=tests_json_file.TestEntry(
                    test=tests_json_file.TestSection(package_name, "", ""),
                ),
            )
        )
        tests.append(
            Test(
                build=tests_json_file.TestEntry(
                    test=tests_json_file.TestSection(package_name2, "", ""),
                ),
            )
        )

        debugger.spawn(tests, callback, True, [])

        # Should have immediately called the mock, since debugger.spawn is synchronous.
        self.assertTrue(self.subprocess_mock_.called)

        expected_args = [
            "fx",
            "ffx",
            "debug",
            "connect",
            "--new-agent",
            "--",
            "--execute",
            f"attach --weak --recursive {package_name}",
            "--execute",
            f"attach --weak --recursive {package_name2}",
            "--console-mode",
            "embedded",
            "--embedded-mode-context",
            "test failure",
            "--stream-file",
            f"{self.fifo_path_}",
            "--signal-when-ready",
            str(os.getpid()),
        ]

        self.subprocess_mock_.assert_called_with(
            args=expected_args,
            start_new_session=True,
            stderr=subprocess.STDOUT,
        )

    async def test_explicit_breakpoints_no_break_on_failure(self) -> None:
        """Tests zxdb command generation when the user specifies breakpoints but not
        break-on-failure."""

        # Don't need to do any waiting to check the arguments are correct.
        async def callback() -> None:
            pass

        package_name = "fuchsia-pkg://fuchsia.com/foo_test#meta/foo_test.cm"
        test = Test(
            build=tests_json_file.TestEntry(
                test=tests_json_file.TestSection(package_name, "", ""),
            ),
        )

        debugger.spawn([test], callback, False, ["myfile.rs:1234"])

        # Should have immediately called the mock, since debugger.spawn is synchronous.
        self.assertTrue(self.subprocess_mock_.called)

        # No embedded mode context will be given if break_on_failure is not specified.
        expected_args = [
            "fx",
            "ffx",
            "debug",
            "connect",
            "--new-agent",
            "--",
            "--execute",
            # Always strong attach when an explicit breakpoint is given. All attaches should be
            # recursive.
            f"attach --recursive {package_name}",
            "--console-mode",
            "embedded",
            "--stream-file",
            f"{self.fifo_path_}",
            "--signal-when-ready",
            str(os.getpid()),
            # Breakpoints come last.
            "--execute",
            "break myfile.rs:1234",
        ]

        self.subprocess_mock_.assert_called_with(
            args=expected_args,
            start_new_session=True,
            stderr=subprocess.STDOUT,
        )

    async def test_explicit_breakpoints_with_break_on_failure(self) -> None:
        """Tests zxdb command generation when the user specifies breakpoints and
        break-on-failure."""

        # Don't need to do any waiting to check the arguments are correct.
        async def callback() -> None:
            pass

        package_name = "fuchsia-pkg://fuchsia.com/foo_test#meta/foo_test.cm"
        test = Test(
            build=tests_json_file.TestEntry(
                test=tests_json_file.TestSection(package_name, "", ""),
            ),
        )

        debugger.spawn([test], callback, True, ["myfile.rs:1234"])

        # Should have immediately called the mock, since debugger.spawn is synchronous.
        self.assertTrue(self.subprocess_mock_.called)

        # Note now the embedded mode context is present because break_on_failure is true, despite
        # the presence of user requested breakpoints.
        expected_args = [
            "fx",
            "ffx",
            "debug",
            "connect",
            "--new-agent",
            "--",
            "--execute",
            # Always strong attach when an explicit breakpoint is given. All attaches should be
            # recursive.
            f"attach --recursive {package_name}",
            "--console-mode",
            "embedded",
            "--embedded-mode-context",
            "test failure",
            "--stream-file",
            f"{self.fifo_path_}",
            "--signal-when-ready",
            str(os.getpid()),
            # Breakpoints come last.
            "--execute",
            "break myfile.rs:1234",
        ]

        self.subprocess_mock_.assert_called_with(
            args=expected_args,
            start_new_session=True,
            stderr=subprocess.STDOUT,
        )

    async def test_callback_when_ready(self) -> None:
        """Tests that the callback given to spawn is called when zxdb signals that it is ready."""
        condvar = asyncio.Condition()

        async def condvar_notify() -> None:
            async with condvar:
                condvar.notify_all()

        # This mock will get called as a callback.
        mock_callback = mock.MagicMock(side_effect=condvar_notify)

        package_name = "fuchsia-pkg://fuchsia.com/foo_test#meta/foo_test.cm"
        test = Test(
            build=tests_json_file.TestEntry(
                test=tests_json_file.TestSection(package_name, "", ""),
            ),
        )

        debugger.spawn([test], mock_callback, True, [])

        # Simulate the debugger sending sigusr1, there is no subprocess so our pid is the pid
        # listening. This kicks off the task that will notify the condition variable so that we may
        # proceed below.
        await asyncio.sleep(0.2)
        os.kill(os.getpid(), signal.SIGUSR1)

        async with condvar:
            await condvar.wait()

            mock_callback.assert_called_once()
