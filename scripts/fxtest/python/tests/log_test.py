# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import contextlib
import io
import json
import unittest

import event
import log


class TestLogOutput(unittest.IsolatedAsyncioTestCase):
    async def _write_test_logs(self) -> io.StringIO:
        """Write out test logs

        Returns:
            io.StringIO: A buffer containing test logs.
        """
        recorder = event.EventRecorder()
        output = io.StringIO()
        log_task = asyncio.create_task(log.writer(recorder, output))
        recorder.emit_init()
        id = recorder.emit_build_start(["//test"])
        recorder.emit_end(id=id)
        recorder.emit_info_message("Done testing")
        recorder.emit_end()

        await log_task

        return output

    async def test_logs_json(self) -> None:
        """Test that logs are properly serialized to JSON."""
        output = await self._write_test_logs()

        events: list[event.Event] = [
            event.Event.from_dict(json.loads(line))  # type:ignore
            for line in output.getvalue().splitlines()
        ]

        self.assertEqual(len(events), 5)
        payloads = [e.payload for e in events if e.payload is not None]
        self.assertEqual(len(payloads), 3)
        self.assertIsNotNone(payloads[0].start_timestamp)
        self.assertEqual(payloads[1].build_targets, ["//test"])
        self.assertEqual(
            payloads[2].user_message,
            event.Message("Done testing", event.MessageLevel.INFO),
        )

    async def test_pretty_print(self) -> None:
        output = await self._write_test_logs()
        stdout = io.StringIO()
        with contextlib.redirect_stdout(stdout):
            log.pretty_print(log.LogSource.from_stream(output))
        self.assertEqual(stdout.getvalue(), "0 tests were run\n")
