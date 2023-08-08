# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import contextlib
import io
import unittest
import unittest.mock as mock

import args
import console
import event


class TestConsoleOutput(unittest.IsolatedAsyncioTestCase):
    @mock.patch("console.termout.is_valid", return_value=True)
    @mock.patch(
        "console.statusinfo.os.get_terminal_size",
        return_value=mock.MagicMock(columns=80),
    )
    async def test_console(
        self, _is_valid_mock: mock.Mock, _terminal_size_mock: mock.Mock
    ):
        """Test that a few different types of events provide some output for the terminal."""

        output = io.StringIO()
        with contextlib.redirect_stdout(output):
            recorder = event.EventRecorder()
            default_flags = args.parse_args(["--status"])
            status_event = asyncio.Event()
            printer_task = asyncio.create_task(
                console.console_printer(recorder, default_flags, status_event)
            )
            status_event.set()

            recorder.emit_init()
            build_id = recorder.emit_build_start([])
            recorder.emit_end(id=build_id)
            recorder.emit_info_message("A message")
            test_group_id = recorder.emit_test_group(3)
            suite_ids = []
            suite_ids.append(
                recorder.emit_test_suite_started(
                    "foo", hermetic=True, parent=test_group_id
                )
            )
            suite_ids.append(
                recorder.emit_test_suite_started(
                    "bar", hermetic=True, parent=test_group_id
                )
            )
            suite_ids.append(
                recorder.emit_test_suite_started(
                    "baz", hermetic=False, parent=test_group_id
                )
            )

            before = output.getvalue()
            while output.getvalue() == before:
                # Wait until the output refreshes.
                await asyncio.sleep(0.1)

            for id in suite_ids:
                recorder.emit_test_suite_ended(
                    id, event.TestSuiteStatus.PASSED, "Passed"
                )

            recorder.emit_end(id=test_group_id)
            recorder.emit_end()

            await printer_task

        self.assertTrue("Status" in output.getvalue())
