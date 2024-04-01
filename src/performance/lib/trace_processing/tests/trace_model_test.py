#!/usr/bin/env fuchsia-vendored-python
# Copyright 2023 The Fuchsia Authors. All rights reserved.
# Use of this source code is governed by a BSD-style license that can be
# found in the LICENSE file.
"""Unit tests for trace_model.py."""

import unittest

import trace_processing.trace_model as trace_model
import trace_processing.trace_time as trace_time
import test_utils


class TraceModelTest(unittest.TestCase):
    """Trace model tests"""

    def test_trace_slicing(self) -> None:
        model: trace_model.Model = test_utils.get_test_model()

        sliced_model: trace_model.Model = model.slice(
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697800000)
            ),
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(698600000)
            ),
        )
        self.assertEqual(len(list(sliced_model.all_events())), 2)

        tail_model: trace_model.Model = model.slice(
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697800000)
            ),
            None,
        )
        self.assertEqual(len(list(tail_model.all_events())), 4)

        head_model: trace_model.Model = model.slice(
            None,
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(698600000)
            ),
        )
        self.assertEqual(len(list(head_model.all_events())), 5)

        # Slicing with a doubly-infinite interval should result in an identical
        # model.
        copied_model: trace_model.Model = model.slice(None, None)
        test_utils.assertModelsEqual(self, model, copied_model)

    def test_trace_slicing_sched(self) -> None:
        model: trace_model.Model = test_utils.get_test_model()

        sliced_model: trace_model.Model = model.slice(
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138)
            ),
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138.9531089)
            ),
        )
        self.assertEqual(len(sliced_model.scheduling_records[0]), 2)
        self.assertEqual(len(sliced_model.scheduling_records[1]), 2)

        tail_model: trace_model.Model = model.slice(
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138.9531089)
            ),
            None,
        )
        self.assertEqual(len(tail_model.scheduling_records[0]), 2)
        self.assertEqual(len(tail_model.scheduling_records[1]), 2)

        head_model: trace_model.Model = model.slice(
            None,
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697503138.9531089)
            ),
        )
        self.assertEqual(len(head_model.scheduling_records[0]), 4)
        self.assertEqual(len(head_model.scheduling_records[1]), 2)

    def test_slice_doesnt_reference_old_model(self) -> None:
        model: trace_model.Model = test_utils.get_test_model()
        sliced_model: trace_model.Model = model.slice(
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(697800000)
            ),
            trace_time.TimePoint.from_epoch_delta(
                trace_time.TimeDelta.from_microseconds(698600000)
            ),
        )
        self.assertEqual(len(list(sliced_model.all_events())), 2)

        sentinel_name: str = "OLD_MODEL_SENTINEL"
        for event in model.all_events():
            event.name = sentinel_name

        for event in sliced_model.all_events():
            if isinstance(event, trace_model.DurationEvent):
                for child in event.child_durations:
                    self.assertIsNotNone(child)
                    self.assertNotEqual(child.name, sentinel_name)
                for child in event.child_flows:
                    self.assertIsNotNone(child)
                    self.assertNotEqual(child.name, sentinel_name)
            elif isinstance(event, trace_model.FlowEvent):
                previous_flow_name: str = (
                    event.previous_flow.name
                    if event.previous_flow is not None
                    else None
                )
                next_flow_name: str = (
                    event.next_flow.name
                    if event.next_flow is not None
                    else None
                )
                self.assertNotEqual(previous_flow_name, sentinel_name)
                self.assertNotEqual(next_flow_name, sentinel_name)
