#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for trace_importing.py."""

import os
from typing import Any, Dict, List
import unittest

import trace_processing.trace_importing as trace_importing
import trace_processing.trace_model as trace_model
import trace_processing.trace_utils as trace_utils
import test_utils


class TraceImportingTest(unittest.TestCase):
    """Trace importing tests"""

    def setUp(self):
        # A second dirname is required to account for the .pyz archive which
        # contains the test and a third one since data is a sibling of the test.
        self._runtime_deps_path: str = os.path.join(
            os.path.dirname(os.path.dirname(os.path.dirname(__file__))),
            "runtime_deps",
        )

    def test_create_model(self) -> None:
        """Test case to ensure we can load a model from a file"""

        model: trace_model.Model = test_utils.get_test_model()
        model_from_json: trace_model.Model = (
            trace_importing.create_model_from_file_path(
                os.path.join(self._runtime_deps_path, "model.json")
            )
        )
        test_utils.assertModelsEqual(self, model, model_from_json)

    def test_dangling_begin_event(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_string(
            """
{
  "displayTimeUnit": "ns",
  "traceEvents": [
    {
      "cat": "category",
      "name": "name",
      "ts": 0.0,
      "ph": "B",
      "tid": 0,
      "pid": 0
    }
  ],
  "systemTraceEvents": {
    "events": [],
    "type": "fuchsia"
  }
}
"""
        )
        self.assertEqual(len(list(model.all_events())), 0)

    def test_integral_timestamp_and_duration(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_string(
            """
{
  "displayTimeUnit": "ns",
  "traceEvents": [
    {
      "cat": "test",
      "name": "integral",
      "ts": 12345,
      "pid": 35204,
      "tid": 323993,
      "ph": "X",
      "dur": 200
    }
  ],
  "systemTraceEvents": {
    "events": [],
    "type": "fuchsia"
  }
}
"""
        )
        self.assertNotEqual(len(list(model.all_events())), 0)

    def test_zero_length_duration_events(self) -> None:
        # This is a regression test for a bug (https://fxbug.dev/42082034) where
        # trace importing fails to correctly handle zero-length trace
        # durations with the ph='X' type.
        #
        # The bug arose because the trace importer sorted the trace events
        # using a non-stable sort (likely quicksort). We use an example input
        # here with a moderate number of trace events (100) because the bug
        # did not reproduce with a small number of trace events (such as 1 or
        # 10) but did reproduce with a larger number (such as 20).
        trace_json: Dict[str, Any] = {
            "displayTimeUnit": "ns",
            "traceEvents": [],
            "systemTraceEvents": {
                "events": [],
                "type": "fuchsia",
            },
        }
        for idx in range(100):
            trace_json["traceEvents"].append(
                {
                    "cat": "some_category",
                    "name": "some_event",
                    "ts": 1000 + idx,
                    "pid": 35204,
                    "tid": 323993,
                    "ph": "X",
                    "dur": 0,
                }
            )

        model: trace_model.Model = trace_importing.create_model_from_json(
            trace_json
        )
        self.assertNotEqual(len(list(model.all_events())), 0)

    def test_preserve_ordering_same_start_time(self) -> None:
        # Test that the ordering of duration events is preserved when the
        # durations have the same start timestamp. The tests the case of
        # separate begin and end records (ph='B' and ph='E') in the input.
        #
        # This is a regression test for a bug (https://fxbug.dev/42082034). The
        # bug arose because the trace importer sorted the trace events using a
        # non-stable sort (likely quicksort). We use an example input here
        # with a moderate number of trace events (100) because the bug did
        # not reproduce with a small number of trace events (such as 1 or 10
        # or 20) but did reproduce with a larger number (such as 50).
        expected_names: List[str] = []
        trace_json: Dict[str, Any] = {
            "displayTimeUnit": "ns",
            "traceEvents": [],
            "systemTraceEvents": {
                "events": [],
                "type": "fuchsia",
            },
        }
        for idx in range(100):
            event_name: str = f"event_{idx}"
            trace_json["traceEvents"].append(
                {
                    "cat": "some_category",
                    "name": event_name,
                    "ts": 1000,
                    "pid": 35204,
                    "tid": 323993,
                    "ph": "B",
                }
            )
            trace_json["traceEvents"].append(
                {
                    "cat": "some_category",
                    "name": event_name,
                    "ts": 1000,
                    "pid": 35204,
                    "tid": 323993,
                    "ph": "E",
                }
            )
            expected_names.append(event_name)

        # Check that the events are imported with the expected ordering.
        model: trace_model.Model = trace_importing.create_model_from_json(
            trace_json
        )
        self.assertEqual(
            [event.name for event in model.all_events()], expected_names
        )

    def test_flow_ids(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            os.path.join(self._runtime_deps_path, "flow_ids.json")
        )

        events: List[trace_model.Event] = list(model.all_events())
        self.assertEqual(len(events), 4)

        flow_events: List[trace_model.Event] = list(
            trace_utils.filter_events(events, type=trace_model.FlowEvent)
        )
        flow_events.sort(key=lambda x: x.start)
        self.assertEqual(len(flow_events), 3)
        self.assertIsNotNone(flow_events[0].next_flow)
        self.assertIsNotNone(flow_events[1].next_flow)
        self.assertIsNone(flow_events[2].next_flow)

    def test_flow_event_binding_points(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            os.path.join(self._runtime_deps_path, "flow_event_binding.json")
        )

        process: trace_model.Process = model.processes[0]
        thread: trace_model.Thread = process.threads[0]
        self.assertEqual(len(model.processes), 1)
        self.assertEqual(len(process.threads), 1)
        flow_events: List[trace_model.Event] = list(
            trace_utils.filter_events(thread.events, type=trace_model.FlowEvent)
        )
        self.assertEqual(len(flow_events), 6)
        self.assertEqual(
            flow_events[0]
            .enclosing_duration.start.to_epoch_delta()
            .to_milliseconds_f(),
            10.0,
        )
        self.assertEqual(
            flow_events[1]
            .enclosing_duration.start.to_epoch_delta()
            .to_milliseconds_f(),
            20.0,
        )
        self.assertEqual(
            flow_events[2]
            .enclosing_duration.start.to_epoch_delta()
            .to_milliseconds_f(),
            40.0,
        )
        self.assertEqual(
            flow_events[3]
            .enclosing_duration.start.to_epoch_delta()
            .to_milliseconds_f(),
            50.0,
        )
        self.assertEqual(
            flow_events[4]
            .enclosing_duration.start.to_epoch_delta()
            .to_milliseconds_f(),
            60.0,
        )
        self.assertEqual(
            flow_events[5]
            .enclosing_duration.start.to_epoch_delta()
            .to_milliseconds_f(),
            70.0,
        )

    def test_async_events_with_id2(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            os.path.join(self._runtime_deps_path, "id2_async.json")
        )
        self.assertEqual(len(model.processes), 2)
        self.assertEqual(
            len(
                list(
                    trace_utils.filter_events(
                        model.all_events(),
                        category="test",
                        name="async",
                        type=trace_model.AsyncEvent,
                    )
                )
            ),
            2,
        )
        self.assertEqual(
            len(
                list(
                    trace_utils.filter_events(
                        model.all_events(),
                        category="test",
                        name="async2",
                        type=trace_model.AsyncEvent,
                    )
                )
            ),
            2,
        )

    def test_chrome_metadata_events(self) -> None:
        model: trace_model.Model = trace_importing.create_model_from_file_path(
            os.path.join(self._runtime_deps_path, "chrome_metadata.json")
        )
        process: trace_model.Process = model.processes[0]
        thread: trace_model.Thread = process.threads[0]
        self.assertEqual(len(model.processes), 1)
        self.assertEqual(len(process.threads), 1)
        self.assertEqual(process.name, "Test process")
        self.assertEqual(thread.name, "Test thread")
