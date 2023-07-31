# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.

import asyncio
import io
import json
import typing
import unittest

import event
import log


class TestLogOutput(unittest.IsolatedAsyncioTestCase):
    async def test_logs_json(self):
        """Test that logs are properly serialized to JSON."""
        recorder = event.EventRecorder()
        output = io.StringIO()
        log_task = asyncio.create_task(log.writer(recorder, output))
        recorder.emit_init()
        id = recorder.emit_build_start(["//test"])
        recorder.emit_end(id=id)
        recorder.emit_info_message("Done testing")
        recorder.emit_end()

        await log_task

        events: typing.List[event.Event] = [
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
