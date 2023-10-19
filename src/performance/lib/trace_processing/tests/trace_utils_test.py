#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for trace_utils.py."""

from typing import Any, Dict, List
import unittest

import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time
import trace_processing.trace_utils as trace_utils


class TraceUtilsTest(unittest.TestCase):
    """Trace utils tests"""

    def test_filter_events(self) -> None:
        events: List[trace_model.Event] = [
            trace_model.DurationEvent(
                duration=None,
                parent=None,
                child_durations=[],
                child_flows=[],
                base=trace_model.Event(
                    category="cat_a",
                    name="name_a",
                    start=trace_time.TimePoint.from_epoch_delta(
                        trace_time.TimeDelta.from_microseconds(
                            697778328.2160872
                        )
                    ),
                    pid=7009,
                    tid=7022,
                    args={},
                ),
            ),
            trace_model.DurationEvent(
                duration=None,
                parent=None,
                child_durations=[],
                child_flows=[],
                base=trace_model.Event(
                    category="cat_b",
                    name="name_b",
                    start=trace_time.TimePoint.from_epoch_delta(
                        trace_time.TimeDelta.from_microseconds(
                            697778328.2160872
                        )
                    ),
                    pid=7009,
                    tid=7022,
                    args={},
                ),
            ),
        ]

        filtered: List[trace_model.Event] = list(
            trace_utils.filter_events(events, category="cat_a", name="name_a")
        )
        self.assertEqual(filtered, [events[0]])

        filtered2: List[trace_model.Event] = list(
            trace_utils.filter_events(events, category="cat_c", name="name_c")
        )
        self.assertEqual(filtered2, [])

    def test_filter_events_with_type(self) -> None:
        events: List[trace_model.Event] = [
            trace_model.DurationEvent(
                duration=None,
                parent=None,
                child_durations=[],
                child_flows=[],
                base=trace_model.Event(
                    category="cat_a",
                    name="name_a",
                    start=trace_time.TimePoint.from_epoch_delta(
                        trace_time.TimeDelta.from_microseconds(
                            697778328.2160872
                        )
                    ),
                    pid=7009,
                    tid=7022,
                    args={},
                ),
            ),
            trace_model.DurationEvent(
                duration=None,
                parent=None,
                child_durations=[],
                child_flows=[],
                base=trace_model.Event(
                    category="cat_b",
                    name="name_b",
                    start=trace_time.TimePoint.from_epoch_delta(
                        trace_time.TimeDelta.from_microseconds(
                            697778328.2160872
                        )
                    ),
                    pid=7009,
                    tid=7022,
                    args={},
                ),
            ),
        ]

        filtered: List[trace_model.Event] = list(
            trace_utils.filter_events(
                events,
                category="cat_a",
                name="name_a",
                type=trace_model.DurationEvent,
            )
        )
        self.assertEqual(filtered, [events[0]])

        filtered2: List[trace_model.Event] = list(
            trace_utils.filter_events(
                events,
                category="cat_c",
                name="name_c",
                type=trace_model.DurationEvent,
            )
        )
        self.assertEqual(filtered2, [])

        filtered3: List[trace_model.Event] = list(
            trace_utils.filter_events(
                events,
                category="cat_a",
                name="name_a",
                type=trace_model.InstantEvent,
            )
        )
        self.assertEqual(filtered3, [])
